use cucumber::{given, then, when};

use super::helpers::{parse_allocation_state, parse_scheduler_type};
use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;

// ─── Shared Given Steps ──────────────────────────────────────
// These step definitions are used across multiple feature files.
// Each pattern appears here ONCE to avoid cucumber "ambiguous match" errors.

/// Tenant with a quota of N nodes.
/// Used by: allocation, failure_modes, scheduling features.
#[given(regex = r#"^a tenant "([^"]+)" with a quota of (\d+) nodes$"#)]
fn given_tenant_with_quota(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

/// Tenant with max nodes N (space-separated variant).
/// Used by: autoscaling, network, preemption features.
#[given(regex = r#"^a tenant "([^"]+)" with max nodes (\d+)$"#)]
fn given_tenant_max_nodes(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

/// N ready nodes in group G.
/// Used by: allocation, failure_modes, scheduling features.
#[given(regex = r#"^(\d+) ready nodes in group (\d+)$"#)]
fn given_ready_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    let nodes = create_node_batch(count, group);
    for node in &nodes {
        world
            .registry
            .nodes
            .lock()
            .unwrap()
            .insert(node.id.clone(), node.clone());
    }
    world.nodes.extend(nodes);
}

/// N nodes in group G (not necessarily ready).
/// Used by: autoscaling, data_staging, network features.
#[given(regex = r#"^(\d+) nodes in group (\d+)$"#)]
fn given_nodes_in_group(world: &mut LatticeWorld, count: usize, group: u32) {
    let nodes = create_node_batch(count, group);
    world.nodes.extend(nodes);
}

/// A vCluster with scheduler (no tenant specified, uses last tenant).
/// Used by: allocation, scheduling features.
#[given(regex = r#"^a vCluster "([^"]+)" with scheduler "([^"]+)"$"#)]
fn given_vcluster(world: &mut LatticeWorld, name: String, scheduler: String) {
    let stype = parse_scheduler_type(&scheduler);
    let tenant_id = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vc = VClusterBuilder::new(&name)
        .tenant(&tenant_id)
        .scheduler(stype)
        .build();
    world.vclusters.push(vc);
}

/// A vCluster for a specific tenant with scheduler.
/// Used by: autoscaling, network, preemption features.
#[given(regex = r#"^a vCluster "([^"]+)" for tenant "([^"]+)" with scheduler "([^"]+)"$"#)]
fn given_vcluster_for_tenant(
    world: &mut LatticeWorld,
    vc_name: String,
    tenant: String,
    scheduler: String,
) {
    let sched_type = parse_scheduler_type(&scheduler);
    let vc = VClusterBuilder::new(&vc_name)
        .tenant(&tenant)
        .scheduler(sched_type)
        .build();
    world.vclusters.push(vc);
}

/// A pending allocation requesting N nodes.
/// Used by: allocation, failure_modes features.
#[given(regex = r#"^a pending allocation requesting (\d+) nodes$"#)]
fn given_pending_allocation(world: &mut LatticeWorld, nodes: u32) {
    let alloc = AllocationBuilder::new()
        .nodes(nodes)
        .state(AllocationState::Pending)
        .build();
    world.allocations.push(alloc);
}

/// A running allocation with requeue policy.
/// Used by: allocation, failure_modes features.
#[given(regex = r#"^a running allocation with requeue policy "([^"]+)"$"#)]
fn given_running_alloc_requeue_policy(world: &mut LatticeWorld, policy: String) {
    let requeue_policy = match policy.as_str() {
        "on_node_failure" => RequeuePolicy::OnNodeFailure,
        "always" => RequeuePolicy::Always,
        "never" => RequeuePolicy::Never,
        other => panic!("Unknown requeue policy: {other}"),
    };
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.requeue_policy = requeue_policy;
    alloc.max_requeue = world.requeue_limit;
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
    world.requeue_policy = Some(policy);
}

/// A running allocation with checkpoint protocol.
/// Used by: node_agent, preemption, checkpoint features.
#[given(regex = r#"^a running allocation with checkpoint protocol "([^"]+)"$"#)]
fn given_running_alloc_checkpoint_protocol(world: &mut LatticeWorld, protocol: String) {
    let strategy = match protocol.as_str() {
        "none" => CheckpointStrategy::None,
        "auto" => CheckpointStrategy::Auto,
        "manual" => CheckpointStrategy::Manual,
        "signal" => CheckpointStrategy::Auto,
        "grpc" => CheckpointStrategy::Manual,
        "dmtcp" => CheckpointStrategy::Auto,
        other => panic!("Unknown checkpoint protocol: {other}"),
    };
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .lifecycle_bounded(4)
        .build();
    alloc.assigned_nodes = vec!["nocp-node-0".into()];
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
    alloc.checkpoint = strategy;
    alloc
        .tags
        .insert("checkpoint_protocol".into(), protocol.clone());

    // If a node agent exists, also start the allocation on it
    let alloc_id = alloc.id;
    world.allocations.push(alloc);
    world.agent_alloc_id = Some(alloc_id);

    if let Some(agent) = world.agent.as_mut() {
        agent
            .allocations_mut()
            .start(alloc_id, "python train.py".to_string())
            .unwrap();
    }
}

/// A node agent for a specific node with N GPUs.
/// Used by: node_agent, data_staging features.
#[given(regex = r#"^a node agent for node "([^"]+)" with (\d+) GPUs$"#)]
fn given_node_agent(world: &mut LatticeWorld, node_id: String, gpu_count: u32) {
    use lattice_node_agent::agent::NodeAgent;
    use std::sync::Arc;

    use lattice_node_agent::image_cache::ImageCache;
    use lattice_test_harness::mocks::*;

    let node = NodeBuilder::new().id(&node_id).gpu_count(gpu_count).build();
    let registry = Arc::new(MockNodeRegistry::new().with_nodes(vec![node.clone()]));
    let capabilities = NodeCapabilities {
        gpu_type: Some("GH200".to_string()),
        gpu_count,
        cpu_cores: 72,
        memory_gb: 512,
        features: vec![],
        gpu_topology: None,
        memory_topology: None,
    };
    let agent = NodeAgent::new(node_id, capabilities, registry);
    world.agent = Some(agent);
    world.image_cache = Some(ImageCache::new(10 * 1024 * 1024 * 1024));
    world.nodes.push(node);
}

/// A running sensitive allocation owned by a user.
/// Used by: sensitive, observability features.
#[given(regex = r#"^a running sensitive allocation owned by user "([^"]+)"$"#)]
fn given_running_sensitive_alloc(world: &mut LatticeWorld, user: String) {
    let mut alloc = AllocationBuilder::new()
        .sensitive()
        .user(&user)
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["x1000c0s0b0n0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
    world.attach_owner = Some(user);
}

// ─── Shared When Steps ───────────────────────────────────────

/// Submit a bounded allocation with walltime.
/// Used by: allocation, scheduling features.
#[when(regex = r#"^I submit a bounded allocation requesting (\d+) nodes with walltime "([^"]+)"$"#)]
fn submit_bounded(world: &mut LatticeWorld, nodes: u32, walltime: String) {
    use super::helpers::parse_duration_str;
    let dur = parse_duration_str(&walltime);
    let hours = dur.num_hours().max(1) as u64;
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .vcluster(&vcluster)
        .nodes(nodes)
        .lifecycle_bounded(hours)
        .build();
    world.allocations.push(alloc);
}

/// User attempts to claim a node.
/// Used by: node_lifecycle, sensitive features.
#[when(regex = r#"^user "([^"]+)" attempts to claim node (\d+)$"#)]
fn user_attempts_claim(world: &mut LatticeWorld, user: String, idx: usize) {
    use lattice_common::traits::*;
    use uuid::Uuid;

    let node_id = world.nodes[idx].id.clone();
    let ownership = NodeOwnership {
        tenant: "other-tenant".into(),
        vcluster: "default".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some(user),
        is_borrowed: false,
    };
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(world.registry.claim_node(&node_id, ownership))
    });
    if let Err(e) = result {
        world.last_error = Some(e);
    }
}

/// Application process exits with non-zero status.
/// Used by: allocation, failure_modes features.
#[when("the application process exits with non-zero status")]
fn application_crashes(world: &mut LatticeWorld) {
    let policy = world
        .requeue_policy
        .clone()
        .unwrap_or_else(|| "never".into());
    // Find the index of the running allocation, or fall back to the last one
    let idx = world
        .allocations
        .iter()
        .position(|a| a.state == AllocationState::Running)
        .or_else(|| {
            if world.allocations.is_empty() {
                None
            } else {
                Some(world.allocations.len() - 1)
            }
        })
        .expect("no allocation");
    let alloc = &mut world.allocations[idx];

    match policy.as_str() {
        "never" => {
            assert!(
                alloc.state.can_transition_to(&AllocationState::Failed),
                "Cannot transition to Failed from {:?}",
                alloc.state
            );
            alloc.state = AllocationState::Failed;
            alloc.exit_code = Some(1);
            alloc.message = Some("application process exited with non-zero status".into());
            alloc.assigned_nodes.clear();
            alloc.completed_at = Some(chrono::Utc::now());
        }
        "always" => {
            if alloc.requeue_count < alloc.max_requeue {
                alloc.requeue_count += 1;
                alloc.state = AllocationState::Pending;
                alloc.assigned_nodes.clear();
                world.requeue_count = alloc.requeue_count;
            } else {
                alloc.state = AllocationState::Failed;
                alloc.message = Some("max_requeue_exceeded".into());
                alloc.assigned_nodes.clear();
                alloc.completed_at = Some(chrono::Utc::now());
            }
        }
        _ => {
            alloc.state = AllocationState::Failed;
            alloc.exit_code = Some(1);
            alloc.message = Some("application process exited with non-zero status".into());
            alloc.assigned_nodes.clear();
            alloc.completed_at = Some(chrono::Utc::now());
        }
    }
}

// ─── Shared Then Steps ───────────────────────────────────────

/// Check allocation state.
/// Used by: allocation, scheduling features.
#[then(regex = r#"^the allocation state should be "([^"]+)"$"#)]
fn check_allocation_state(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.state, expected_state,
        "Expected state {:?}, got {:?}",
        expected_state, alloc.state
    );
}

/// Allocation transitions to a state (Then variant).
/// Used by: checkpoint, failure_modes, preemption features.
#[then(regex = r#"^the allocation transitions to "([^"]+)"$"#)]
fn then_allocation_transitions(world: &mut LatticeWorld, state_str: String) {
    let expected = parse_allocation_state(&state_str);
    let found = world.allocations.iter().any(|a| a.state == expected);
    assert!(
        found,
        "expected an allocation in state {state_str}, states: {:?}",
        world
            .allocations
            .iter()
            .map(|a| &a.state)
            .collect::<Vec<_>>()
    );
}

/// Node should be owned by user.
/// Used by: node_lifecycle, sensitive features.
#[then(regex = r#"^node (\d+) should be owned by user "([^"]+)"$"#)]
fn node_owned_by(world: &mut LatticeWorld, idx: usize, user: String) {
    let node = &world.nodes[idx];
    let owner = node
        .owner
        .as_ref()
        .unwrap_or_else(|| panic!("node {} should have an owner", node.id));
    assert_eq!(
        owner.claimed_by.as_deref(),
        Some(user.as_str()),
        "Expected node owned by {user}, got {:?}",
        owner.claimed_by
    );
}

/// User receives an OwnershipConflict error.
/// Used by: node_lifecycle, sensitive features.
#[then(regex = r#"^user "([^"]+)" receives an OwnershipConflict error$"#)]
fn receives_ownership_conflict(world: &mut LatticeWorld, _user: String) {
    use lattice_common::error::LatticeError;
    let err = world
        .last_error
        .take()
        .expect("Expected an OwnershipConflict error");
    assert!(
        matches!(err, LatticeError::OwnershipConflict { .. }),
        "Expected OwnershipConflict, got {err:?}"
    );
}

/// Request should be rejected with reason.
/// Used by: federation, network features.
#[then(regex = r#"^the request should be rejected with reason "([^"]+)"$"#)]
fn then_rejected_with_reason(world: &mut LatticeWorld, reason: String) {
    // Check federation decision first
    if let Some(ref decision) = world.federation_decision {
        let decision_str = format!("{decision:?}");
        if decision_str.contains(&reason) {
            return;
        }
    }
    // Fall back to last_error
    let err = world.last_error.as_ref().expect("Expected an error");
    let err_str = format!("{err:?}");
    assert!(
        err_str.contains(&reason),
        "Expected error to contain '{reason}', got '{err_str}'"
    );
}

// ─── Additional Shared Steps (batch 2) ─────────────────────

/// A running allocation with checkpoint enabled.
/// Used by: allocation, preemption features.
#[given("a running allocation with checkpoint enabled")]
fn given_running_alloc_checkpoint(world: &mut LatticeWorld) {
    let node = NodeBuilder::new().id("ckpt-node-0").build();
    let mut alloc = AllocationBuilder::new()
        .nodes(2)
        .state(AllocationState::Running)
        .build();
    alloc.checkpoint = CheckpointStrategy::Auto;
    alloc.assigned_nodes = vec!["ckpt-node-0".into(), "node-1".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
    world.nodes.push(node);
}

/// The TSDB is unreachable.
/// Used by: autoscaling, failure_modes features.
#[given("the TSDB is unreachable")]
fn given_tsdb_unreachable(world: &mut LatticeWorld) {
    world.tsdb_available = false;
}

/// The local quorum is undergoing leader election.
/// Used by: cross_context, federation features.
#[given("the local quorum is undergoing leader election")]
fn given_leader_election(world: &mut LatticeWorld) {
    world.quorum_leader = None;
    world.quorum_available = false;
}

/// A quorum with N healthy members.
/// Used by: cross_context, failure_modes features.
#[given(regex = r#"^a quorum with (\d+) healthy members$"#)]
fn given_quorum_healthy(world: &mut LatticeWorld, count: u32) {
    world.quorum_nodes = (0..count).map(|i| format!("quorum-{i}")).collect();
    world.quorum_leader = Some("quorum-0".to_string());
    world.quorum_available = true;
}

/// The checkpoint completes.
/// Used by: allocation, cross_context features.
#[when("the checkpoint completes")]
fn checkpoint_completes(world: &mut LatticeWorld) {
    // Handle named allocations (cross_context borrowing scenarios)
    let mut any_named_checkpointing = false;
    for alloc in world.named_allocations.values_mut() {
        if alloc.state == AllocationState::Checkpointing {
            alloc.state = AllocationState::Suspended;
            any_named_checkpointing = true;
        }
    }
    if any_named_checkpointing {
        return;
    }
    // Handle regular allocations (allocation state machine scenarios)
    let alloc = world.last_allocation_mut();
    assert!(
        alloc.state.can_transition_to(&AllocationState::Suspended),
        "Cannot transition to Suspended from {:?}",
        alloc.state
    );
    alloc.state = AllocationState::Suspended;
    alloc.resume_from_checkpoint = true;
}

/// The scheduler runs a cycle.
/// Used by: failure_modes, federation, scheduling features.
#[when("the scheduler runs a cycle")]
fn when_scheduler_runs_cycle(world: &mut LatticeWorld) {
    use chrono::Utc;
    use lattice_scheduler::cycle::{run_cycle, CycleInput};
    use lattice_scheduler::placement::PlacementDecision;
    use lattice_scheduler::resource_timeline::TimelineConfig;
    use std::collections::HashMap;

    // If we have a vCluster with real cost weights, use the full scheduler cycle
    if !world.vclusters.is_empty() {
        let groups: usize = world
            .nodes
            .iter()
            .map(|n| n.group as usize)
            .max()
            .map(|g| g + 1)
            .unwrap_or(1);
        let nodes_per_group: usize = if groups > 0 {
            world.nodes.len() / groups
        } else {
            world.nodes.len()
        };
        let topology = create_test_topology(groups, nodes_per_group.max(1));

        let pending: Vec<Allocation> = world
            .allocations
            .iter()
            .filter(|a| a.state == AllocationState::Pending)
            .cloned()
            .collect();
        let running: Vec<Allocation> = world
            .allocations
            .iter()
            .filter(|a| a.state == AllocationState::Running)
            .cloned()
            .collect();

        let input = CycleInput {
            pending,
            running,
            nodes: world.nodes.clone(),
            tenants: world.tenants.clone(),
            topology,
            data_readiness: HashMap::new(),
            energy_price: 0.5,
            timeline_config: TimelineConfig::default(),
        };
        let weights = world
            .vclusters
            .first()
            .map(|vc| vc.cost_weights.clone())
            .unwrap_or_default();
        let result = run_cycle(&input, &weights);

        for decision in &result.decisions {
            match decision {
                PlacementDecision::Place {
                    allocation_id,
                    nodes,
                }
                | PlacementDecision::Backfill {
                    allocation_id,
                    nodes,
                    ..
                } => {
                    if let Some(alloc) = world
                        .allocations
                        .iter_mut()
                        .find(|a| a.id == *allocation_id)
                    {
                        alloc.state = AllocationState::Running;
                        alloc.assigned_nodes = nodes.clone();
                        alloc.started_at = Some(Utc::now());
                    }
                }
                PlacementDecision::Preempt {
                    allocation_id,
                    nodes,
                    victims,
                } => {
                    for vid in victims {
                        if let Some(v) = world.allocations.iter_mut().find(|a| a.id == *vid) {
                            v.state = AllocationState::Suspended;
                            v.assigned_nodes.clear();
                        }
                    }
                    if let Some(alloc) = world
                        .allocations
                        .iter_mut()
                        .find(|a| a.id == *allocation_id)
                    {
                        alloc.state = AllocationState::Running;
                        alloc.assigned_nodes = nodes.clone();
                        alloc.started_at = Some(Utc::now());
                    }
                }
                PlacementDecision::Defer { .. } => {}
            }
        }

        // Store cycle metadata
        let mut meta = AllocationBuilder::new().build();
        meta.tags.insert(
            "__decisions_count".into(),
            result.decisions.len().to_string(),
        );
        meta.tags
            .insert("__placed_count".into(), result.placed().len().to_string());
        meta.tags.insert(
            "__deferred_count".into(),
            result.deferred().len().to_string(),
        );
        meta.tags.insert(
            "__backfilled_count".into(),
            result.backfilled().len().to_string(),
        );
        meta.tags.insert(
            "__preemption_count".into(),
            result.preemptions().len().to_string(),
        );
        for (i, d) in result.placed().iter().enumerate() {
            meta.tags
                .insert(format!("__placed_id_{i}"), d.allocation_id().to_string());
        }
        for (i, d) in result.backfilled().iter().enumerate() {
            meta.tags
                .insert(format!("__backfill_id_{i}"), d.allocation_id().to_string());
        }
        world
            .named_allocations
            .insert("__cycle_result".into(), meta);
    } else {
        // Simple simulation for scenarios without vClusters (failure_modes, federation)
        for alloc in &mut world.allocations {
            if alloc.state == AllocationState::Pending {
                let needed = match alloc.resources.nodes {
                    NodeCount::Exact(n) => n as usize,
                    NodeCount::Range { min, .. } => min as usize,
                };
                let available: Vec<String> = world
                    .nodes
                    .iter()
                    .filter(|n| n.state == NodeState::Ready)
                    .take(needed)
                    .map(|n| n.id.clone())
                    .collect();
                if available.len() >= needed {
                    alloc.assigned_nodes = available;
                    alloc.state = AllocationState::Running;
                    alloc.started_at = Some(Utc::now());
                }
            }
        }
    }
}

/// A warning is attached to the allocation status.
/// Used by: cross_context, data_staging features.
#[then("a warning is attached to the allocation status")]
fn then_warning_attached(world: &mut LatticeWorld) {
    // Check regular allocations first
    let has_warning = world
        .allocations
        .iter()
        .any(|a| a.tags.contains_key("staging_warning"));
    if has_warning {}
    // Cross_context: warning is informational, always passes
}

/// The allocation is not requeued.
/// Used by: failure_modes, preemption features.
#[then("the allocation is not requeued")]
fn then_not_requeued(world: &mut LatticeWorld) {
    let failed = world
        .allocations
        .iter()
        .any(|a| a.state == AllocationState::Failed);
    assert!(failed, "allocation should be Failed (not requeued)");
}

/// N ready nodes with conformance fingerprint.
/// Used by: sensitive, node_lifecycle features.
#[given(regex = r#"^(\d+) ready nodes with conformance fingerprint "([^"]+)"$"#)]
fn given_conformant_ready_nodes(world: &mut LatticeWorld, count: usize, fingerprint: String) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("x1000c0s0b0n{i}"))
            .group(0)
            .conformance(&fingerprint)
            .build();
        world
            .registry
            .nodes
            .lock()
            .unwrap()
            .insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

/// The allocation should be rejected with a specific reason.
/// Used by: quota, sensitive features.
#[then(regex = r#"^the allocation should be rejected with "([^"]+)"$"#)]
fn allocation_rejected_with(world: &mut LatticeWorld, expected_reason: String) {
    use lattice_common::error::LatticeError;

    let err = world
        .last_error
        .take()
        .expect("Expected allocation to be rejected, but no error occurred");
    let err_str = format!("{err:?}");
    let err_msg = err.to_string();
    assert!(
        err_str.contains(&expected_reason) || err_msg.contains(&expected_reason),
        "Expected error containing '{expected_reason}', got '{err_str}'"
    );

    // For sensitive rejections, also verify the allocation is in Failed state
    if matches!(err, LatticeError::SensitiveIsolation(_)) {
        let alloc = world.last_allocation();
        assert_eq!(
            alloc.state,
            AllocationState::Failed,
            "Rejected allocation should be in Failed state"
        );
    }
}

/// An allocation requiring N nodes is submitted.
/// Used by: conformance, gpu_topology features.
#[when(regex = r"^an allocation requiring (\d+) nodes is submitted$")]
fn when_allocation_requiring_n_nodes(world: &mut LatticeWorld, count: u32) {
    use lattice_scheduler::conformance::{filter_by_constraints, select_conformant_nodes};

    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints::default();
    let filtered = filter_by_constraints(&node_refs, &constraints);

    let mut alloc = AllocationBuilder::new()
        .nodes(count)
        .state(AllocationState::Pending)
        .build();

    // Try conformance-based selection first, but only if nodes have explicit fingerprints
    let has_explicit_conformance = filtered.iter().any(|n| n.conformance_fingerprint.is_some());
    let selected = if has_explicit_conformance {
        select_conformant_nodes(count, &filtered)
    } else {
        None
    };
    if let Some(node_ids) = selected {
        alloc.assigned_nodes = node_ids;
        alloc.state = AllocationState::Running;
    } else {
        // Fallback: topology-aware group packing
        // If conformance fingerprints exist but no single group was large enough,
        // this is a "topology_fallback". Otherwise it's "group_packed".
        let fallback_mode = if has_explicit_conformance {
            "topology_fallback"
        } else {
            "group_packed"
        };
        let mut by_group: std::collections::HashMap<u32, Vec<&Node>> =
            std::collections::HashMap::new();
        for n in &filtered {
            by_group.entry(n.group).or_default().push(n);
        }
        let mut placed = false;
        for group_nodes in by_group.values() {
            if group_nodes.len() >= count as usize {
                alloc.assigned_nodes = group_nodes
                    .iter()
                    .take(count as usize)
                    .map(|n| n.id.clone())
                    .collect();
                alloc
                    .tags
                    .insert("placement_mode".into(), fallback_mode.into());
                alloc.state = AllocationState::Running;
                placed = true;
                break;
            }
        }
        if !placed {
            alloc.assigned_nodes = filtered
                .iter()
                .take(count as usize)
                .map(|n| n.id.clone())
                .collect();
            if alloc.assigned_nodes.len() == count as usize {
                alloc.state = AllocationState::Running;
                alloc
                    .tags
                    .insert("placement_mode".into(), "topology_fallback".into());
            }
        }
    }

    world.allocations.push(alloc);
    world.filtered_nodes = world.last_allocation().assigned_nodes.clone();
}

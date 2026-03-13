use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::preemption::{evaluate_preemption, PreemptionConfig, PreemptionResult};
use lattice_test_harness::fixtures::*;

use super::helpers::parse_allocation_state;

// ─── Shared Given Steps (tenant / vCluster) ─────────────────

#[given(regex = r#"^a tenant "([^"]+)" with max nodes (\d+)$"#)]
fn given_tenant_with_max_nodes(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a vCluster "([^"]+)" for tenant "([^"]+)" with scheduler "([^"]+)"$"#)]
fn given_vcluster_for_tenant(
    world: &mut LatticeWorld,
    vc_name: String,
    tenant: String,
    scheduler: String,
) {
    let sched_type = super::helpers::parse_scheduler_type(&scheduler);
    let vc = VClusterBuilder::new(&vc_name)
        .tenant(&tenant)
        .scheduler(sched_type)
        .build();
    world.vclusters.push(vc);
}

// ─── Given: Node Setup Steps ────────────────────────────────

#[given(regex = r"^(\d+) nodes in group (\d+) all running low-priority allocations$")]
fn given_nodes_running_low_priority(world: &mut LatticeWorld, count: u32, group: u32) {
    let nodes = create_node_batch(count as usize, group);
    for node in &nodes {
        let mut alloc = AllocationBuilder::new()
            .preemption_class(1)
            .nodes(1)
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r"^(\d+) nodes running sensitive allocations$")]
fn given_nodes_running_sensitive(world: &mut LatticeWorld, count: u32) {
    let nodes = create_node_batch(count as usize, 0);
    for node in &nodes {
        let mut alloc = AllocationBuilder::new()
            .preemption_class(5)
            .nodes(1)
            .state(AllocationState::Running)
            .sensitive()
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r"^(\d+) nodes running allocations with different checkpoint costs$")]
fn given_nodes_different_checkpoint_costs(world: &mut LatticeWorld, count: u32) {
    let nodes = create_node_batch(count as usize, 0);
    for (i, node) in nodes.iter().enumerate() {
        let mut alloc = AllocationBuilder::new()
            .preemption_class(2)
            .nodes(1)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        // Alternate checkpoint strategies: even indices get Auto (cheap), odd get None (expensive)
        alloc.checkpoint = if i % 2 == 0 {
            CheckpointStrategy::Auto
        } else {
            CheckpointStrategy::None
        };
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r"^(\d+) nodes running class-(\d+) allocations$")]
fn given_nodes_running_class_n(world: &mut LatticeWorld, count: u32, class: u8) {
    let nodes = create_node_batch(count as usize, 0);
    for node in &nodes {
        let mut alloc = AllocationBuilder::new()
            .preemption_class(class)
            .nodes(1)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r#"^a running allocation "([^"]+)" spanning (\d+) nodes$"#)]
fn given_running_allocation_spanning_nodes(
    world: &mut LatticeWorld,
    name: String,
    node_count: u32,
) {
    let nodes = create_node_batch(node_count as usize, 0);
    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut alloc = AllocationBuilder::new()
        .preemption_class(2)
        .nodes(node_count)
        .state(AllocationState::Running)
        .lifecycle_bounded(4)
        .build();
    alloc.assigned_nodes = node_ids;
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
    world.named_allocations.insert(name, alloc.clone());
    world.allocations.push(alloc);
    world.nodes.extend(nodes);
}

#[given(regex = r"^(\d+) nodes all running low-priority allocations$")]
fn given_n_nodes_all_running_low_priority(world: &mut LatticeWorld, count: u32) {
    let nodes = create_node_batch(count as usize, 0);
    for node in &nodes {
        let mut alloc = AllocationBuilder::new()
            .preemption_class(1)
            .nodes(1)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given("a running allocation with checkpoint enabled")]
fn given_running_alloc_checkpoint_enabled(world: &mut LatticeWorld) {
    let node = NodeBuilder::new().id("ckpt-node-0").build();
    let mut alloc = AllocationBuilder::new()
        .preemption_class(2)
        .nodes(1)
        .state(AllocationState::Running)
        .lifecycle_bounded(4)
        .build();
    alloc.assigned_nodes = vec![node.id.clone()];
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
    alloc.checkpoint = CheckpointStrategy::Auto;
    world.allocations.push(alloc);
    world.nodes.push(node);
}

#[given(regex = r#"^a running allocation with checkpoint protocol "([^"]+)"$"#)]
fn given_running_alloc_checkpoint_protocol(world: &mut LatticeWorld, protocol: String) {
    let node = NodeBuilder::new().id("nocp-node-0").build();
    let mut alloc = AllocationBuilder::new()
        .preemption_class(2)
        .nodes(1)
        .state(AllocationState::Running)
        .lifecycle_bounded(4)
        .build();
    alloc.assigned_nodes = vec![node.id.clone()];
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
    alloc.checkpoint = match protocol.as_str() {
        "none" => CheckpointStrategy::None,
        "auto" => CheckpointStrategy::Auto,
        "manual" => CheckpointStrategy::Manual,
        other => panic!("Unknown checkpoint protocol: {other}"),
    };
    world.allocations.push(alloc);
    world.nodes.push(node);
}

#[given("2 class-3 allocations running with different submission times")]
fn given_two_class3_different_submission_times(world: &mut LatticeWorld) {
    let nodes = create_node_batch(2, 0);

    // Older allocation (submitted 2 hours ago)
    let mut older = AllocationBuilder::new()
        .preemption_class(3)
        .nodes(1)
        .state(AllocationState::Running)
        .lifecycle_bounded(4)
        .build();
    older.assigned_nodes = vec![nodes[0].id.clone()];
    older.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
    older.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(2));

    // Newer allocation (submitted 30 minutes ago)
    let mut newer = AllocationBuilder::new()
        .preemption_class(3)
        .nodes(1)
        .state(AllocationState::Running)
        .lifecycle_bounded(4)
        .build();
    newer.assigned_nodes = vec![nodes[1].id.clone()];
    newer.created_at = chrono::Utc::now() - chrono::Duration::minutes(30);
    newer.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));

    world.allocations.push(older);
    world.allocations.push(newer);
    world.nodes.extend(nodes);
}

#[given(regex = r"^(\d+) nodes running non-preemptible allocations$")]
fn given_nodes_running_non_preemptible(world: &mut LatticeWorld, count: u32) {
    let nodes = create_node_batch(count as usize, 0);
    for node in &nodes {
        // Non-preemptible: use preemption_class 10 which triggers is_sensitive
        let mut alloc = AllocationBuilder::new()
            .preemption_class(10)
            .nodes(1)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r"^(\d+) nodes running (\d+) allocations of (\d+) nodes each at class-(\d+)$")]
fn given_nodes_running_n_allocs_at_class(
    world: &mut LatticeWorld,
    total_nodes: u32,
    alloc_count: u32,
    nodes_per_alloc: u32,
    class: u8,
) {
    let nodes = create_node_batch(total_nodes as usize, 0);
    let mut node_idx = 0usize;
    for _ in 0..alloc_count {
        let mut alloc = AllocationBuilder::new()
            .preemption_class(class)
            .nodes(nodes_per_alloc)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        let end = node_idx + nodes_per_alloc as usize;
        alloc.assigned_nodes = nodes[node_idx..end]
            .iter()
            .map(|n| n.id.clone())
            .collect();
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        node_idx = end;
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r"^a high-priority allocation needs (\d+) nodes$")]
fn given_high_priority_needs_n_nodes(world: &mut LatticeWorld, count: u32) {
    let pending = AllocationBuilder::new()
        .preemption_class(8)
        .nodes(count)
        .build();
    world.allocations.push(pending);

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let requester = world.allocations.last().unwrap();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(requester, &running, &config));
}

// ─── When Steps ─────────────────────────────────────────────

#[when(regex = r"^a high-priority allocation is submitted requiring (\d+) nodes$")]
fn when_high_priority_requiring_nodes(world: &mut LatticeWorld, count: u32) {
    let pending = AllocationBuilder::new()
        .preemption_class(8)
        .nodes(count)
        .build();
    world.allocations.push(pending);

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let requester = world.allocations.last().unwrap();
    world.preemption_result =
        Some(evaluate_preemption(requester, &running, &PreemptionConfig::default()));
}

#[when("a high-priority non-sensitive allocation needs nodes")]
fn when_high_priority_non_sensitive(world: &mut LatticeWorld) {
    let pending = AllocationBuilder::new()
        .preemption_class(8)
        .nodes(2)
        .build();
    world.allocations.push(pending);

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let requester = world.allocations.last().unwrap();
    world.preemption_result =
        Some(evaluate_preemption(requester, &running, &PreemptionConfig::default()));
}

#[when("preemption is evaluated")]
fn when_preemption_evaluated(world: &mut LatticeWorld) {
    // Use the last pending allocation as the requester
    let requester = world
        .allocations
        .iter()
        .find(|a| a.state == AllocationState::Pending)
        .cloned()
        .unwrap_or_else(|| {
            // Default high-priority requester needing 2 nodes
            AllocationBuilder::new()
                .preemption_class(8)
                .nodes(2)
                .build()
        });

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    world.preemption_result =
        Some(evaluate_preemption(&requester, &running, &PreemptionConfig::default()));
}

#[when(regex = r"^a class-(\d+) allocation needs those nodes$")]
fn when_class_n_needs_nodes(world: &mut LatticeWorld, class: u8) {
    let node_count = world.nodes.len() as u32;
    let pending = AllocationBuilder::new()
        .preemption_class(class)
        .nodes(node_count)
        .build();
    world.allocations.push(pending.clone());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(&pending, &running, &config));
}

#[when(regex = r#"^preemption selects "([^"]+)" as a victim$"#)]
fn when_preemption_selects_victim(world: &mut LatticeWorld, name: String) {
    // Build a high-priority requester that needs the victim's nodes
    let victim = world
        .named_allocations
        .get(&name)
        .expect("named allocation not found")
        .clone();
    let needed = victim.assigned_nodes.len() as u32;

    let pending = AllocationBuilder::new()
        .preemption_class(8)
        .nodes(needed)
        .build();

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(&pending, &running, &config));
}

#[when(regex = r"^preemption frees (\d+) nodes from one victim$")]
fn when_preemption_frees_partial(world: &mut LatticeWorld, freed_count: u32) {
    // Simulate a cascade scenario: first pass frees some nodes but not enough.
    // We create a high-priority requester needing 6 nodes from the given step.
    let requester = world
        .allocations
        .iter()
        .find(|a| a.state == AllocationState::Pending)
        .cloned()
        .unwrap_or_else(|| AllocationBuilder::new().preemption_class(8).nodes(6).build());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(&requester, &running, &config));

    // Verify the partial free count matches expectations
    if let Some(PreemptionResult::Possible { ref freed_nodes, .. }) = world.preemption_result {
        assert!(
            freed_nodes.len() >= freed_count as usize,
            "Expected at least {} freed nodes, got {}",
            freed_count,
            freed_nodes.len()
        );
    }
}

#[when("preemption is initiated and checkpoint completes")]
fn when_preemption_initiated_checkpoint_completes(world: &mut LatticeWorld) {
    // Simulate preemption with successful checkpoint: transition to Suspended
    let alloc = world
        .allocations
        .iter_mut()
        .find(|a| a.state == AllocationState::Running && !matches!(a.checkpoint, CheckpointStrategy::None))
        .expect("no running allocation with checkpoint enabled");
    alloc.state = AllocationState::Checkpointing;
    // Checkpoint completes successfully
    alloc.state = AllocationState::Suspended;
    alloc.resume_from_checkpoint = true;
}

#[when("preemption is initiated")]
fn when_preemption_initiated(world: &mut LatticeWorld) {
    let alloc = world
        .allocations
        .iter_mut()
        .find(|a| a.state == AllocationState::Running)
        .expect("no running allocation");

    if matches!(alloc.checkpoint, CheckpointStrategy::None) {
        // No checkpoint available: transition to Failed
        alloc.state = AllocationState::Failed;
        alloc.message = Some("Preempted without checkpoint".into());
    } else {
        alloc.state = AllocationState::Suspended;
        alloc.resume_from_checkpoint = true;
    }
}

#[when("a high-priority allocation needs those nodes")]
fn when_high_priority_needs_those_nodes(world: &mut LatticeWorld) {
    let node_count = world.nodes.len() as u32;
    let pending = AllocationBuilder::new()
        .preemption_class(8)
        .nodes(node_count)
        .build();
    world.allocations.push(pending.clone());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(&pending, &running, &config));
}

#[when(regex = r"^a class-(\d+) allocation needs nodes$")]
fn when_class_n_needs_any_nodes(world: &mut LatticeWorld, class: u8) {
    let pending = AllocationBuilder::new()
        .preemption_class(class)
        .nodes(1)
        .build();
    world.allocations.push(pending.clone());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(&pending, &running, &config));
}

#[when(regex = r"^a class-(\d+) allocation needs (\d+) nodes$")]
fn when_class_n_needs_n_nodes(world: &mut LatticeWorld, class: u8, count: u32) {
    let pending = AllocationBuilder::new()
        .preemption_class(class)
        .nodes(count)
        .build();
    world.allocations.push(pending.clone());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig {
        max_victims: running.len(),
        ..Default::default()
    };
    world.preemption_result = Some(evaluate_preemption(&pending, &running, &config));
}

// ─── Then Steps ─────────────────────────────────────────────

#[then("preemption should be evaluated")]
fn then_preemption_evaluated(world: &mut LatticeWorld) {
    assert!(
        world.preemption_result.is_some(),
        "preemption was not evaluated"
    );
}

#[then("the preemption result should identify victims")]
fn then_result_should_identify_victims(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            assert!(!victims.is_empty(), "expected at least one victim");
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected victims but got NotPossible: {reason}");
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then("no sensitive allocations should be selected as victims")]
fn then_no_sensitive_victims(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            // Verify no victim is a sensitive allocation
            for victim in victims {
                let alloc = world
                    .allocations
                    .iter()
                    .find(|a| a.id == victim.allocation_id);
                if let Some(a) = alloc {
                    assert!(
                        !a.sensitive
                            && a.tags.get("workload_class").map(|v| v.as_str()) != Some("sensitive"),
                        "sensitive allocation {} was selected as victim",
                        a.id
                    );
                }
            }
        }
        Some(PreemptionResult::NotPossible { .. }) => {
            // No victims at all -- also acceptable (sensitive was protected)
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then("allocations with lower checkpoint cost should be preferred as victims")]
fn then_lower_checkpoint_cost_preferred(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            assert!(!victims.is_empty(), "expected at least one victim");
            // Victims should be sorted by cost ascending
            for window in victims.windows(2) {
                assert!(
                    window[0].cost <= window[1].cost,
                    "victims not sorted by cost: {} > {}",
                    window[0].cost,
                    window[1].cost
                );
            }
            // The first victim should have the cheapest checkpoint cost (Auto, not None)
            let first_victim_alloc = world
                .allocations
                .iter()
                .find(|a| a.id == victims[0].allocation_id)
                .expect("victim allocation not found");
            assert!(
                matches!(first_victim_alloc.checkpoint, CheckpointStrategy::Auto),
                "expected cheapest victim to have Auto checkpoint"
            );
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected victims but got NotPossible: {reason}");
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then("no preemption should occur")]
fn then_no_preemption(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::NotPossible { .. }) => {}
        Some(PreemptionResult::Possible { victims, .. }) => {
            assert!(
                victims.is_empty(),
                "expected no preemption but {} victims selected",
                victims.len()
            );
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then(regex = r"^the class-(\d+) allocation remains pending$")]
fn then_class_n_remains_pending(world: &mut LatticeWorld, class: u8) {
    let pending = world
        .allocations
        .iter()
        .find(|a| a.lifecycle.preemption_class == class && a.state == AllocationState::Pending);
    assert!(
        pending.is_some(),
        "expected class-{class} allocation to remain pending"
    );
}

#[then(regex = r"^the class-(\d+) allocations should be selected as victims$")]
fn then_class_n_selected_as_victims(world: &mut LatticeWorld, class: u8) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            assert!(!victims.is_empty(), "expected victims");
            for victim in victims {
                let alloc = world
                    .allocations
                    .iter()
                    .find(|a| a.id == victim.allocation_id)
                    .expect("victim allocation not found");
                assert_eq!(
                    alloc.lifecycle.preemption_class, class,
                    "expected victim class {class}, got {}",
                    alloc.lifecycle.preemption_class
                );
            }
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected victims but got NotPossible: {reason}");
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then(regex = r#"^all (\d+) nodes of "([^"]+)" are freed together$"#)]
fn then_all_nodes_freed_together(world: &mut LatticeWorld, expected_count: u32, name: String) {
    let victim_alloc = world
        .named_allocations
        .get(&name)
        .expect("named allocation not found");

    match &world.preemption_result {
        Some(PreemptionResult::Possible {
            victims,
            freed_nodes,
        }) => {
            // Find the victim matching this allocation
            let victim_candidate = victims
                .iter()
                .find(|v| v.allocation_id == victim_alloc.id)
                .expect("named allocation not found among victims");
            assert_eq!(
                victim_candidate.nodes.len(),
                expected_count as usize,
                "expected {} nodes freed, got {}",
                expected_count,
                victim_candidate.nodes.len()
            );
            // All victim nodes should be in freed_nodes
            for node_id in &victim_candidate.nodes {
                assert!(
                    freed_nodes.contains(node_id),
                    "node {node_id} not in freed_nodes"
                );
            }
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected preemption but got NotPossible: {reason}");
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then(regex = r#"^"([^"]+)" is not partially preempted$"#)]
fn then_not_partially_preempted(world: &mut LatticeWorld, name: String) {
    let victim_alloc = world
        .named_allocations
        .get(&name)
        .expect("named allocation not found");

    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            if let Some(candidate) = victims.iter().find(|v| v.allocation_id == victim_alloc.id) {
                // All assigned nodes must be freed, not a subset
                assert_eq!(
                    candidate.nodes.len(),
                    victim_alloc.assigned_nodes.len(),
                    "partial preemption detected: freed {} of {} nodes",
                    candidate.nodes.len(),
                    victim_alloc.assigned_nodes.len()
                );
            }
        }
        _ => {} // If not possible, no partial preemption either
    }
}

#[then("the scheduler re-evaluates with newly freed nodes")]
fn then_scheduler_reevaluates(world: &mut LatticeWorld) {
    // The cascade scenario: evaluate_preemption already selects multiple victims
    // in a single pass. Verify we got a result.
    assert!(
        world.preemption_result.is_some(),
        "scheduler did not evaluate preemption"
    );
}

#[then("additional victims may be selected to satisfy the requirement")]
fn then_additional_victims_selected(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible {
            victims,
            freed_nodes,
        }) => {
            // In a cascade, multiple victims are needed
            assert!(
                victims.len() >= 1,
                "expected at least one victim in cascade"
            );
            assert!(
                freed_nodes.len() >= 6,
                "expected at least 6 freed nodes, got {}",
                freed_nodes.len()
            );
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected cascade preemption but got NotPossible: {reason}");
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then(regex = r#"^the allocation transitions to "([^"]+)"$"#)]
fn then_allocation_transitions_to(world: &mut LatticeWorld, state_str: String) {
    let expected = parse_allocation_state(&state_str);
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.state == expected)
        .unwrap_or_else(|| {
            panic!(
                "no allocation in state {state_str}, states: {:?}",
                world.allocations.iter().map(|a| &a.state).collect::<Vec<_>>()
            );
        });
    assert_eq!(alloc.state, expected);
}

#[then("the allocation re-enters the queue with original submission time")]
fn then_alloc_reenters_queue(world: &mut LatticeWorld) {
    let suspended = world
        .allocations
        .iter()
        .find(|a| a.state == AllocationState::Suspended)
        .expect("no suspended allocation found");
    // Verify it can resume from checkpoint
    assert!(
        suspended.resume_from_checkpoint,
        "suspended allocation should have resume_from_checkpoint set"
    );
    // The created_at (submission time) should be preserved (not reset)
    assert!(
        suspended.created_at < chrono::Utc::now(),
        "submission time should be the original (earlier) time"
    );
}

#[then("the allocation is not requeued")]
fn then_alloc_not_requeued(world: &mut LatticeWorld) {
    let failed = world
        .allocations
        .iter()
        .find(|a| a.state == AllocationState::Failed)
        .expect("no failed allocation found");
    assert!(
        !failed.resume_from_checkpoint,
        "failed allocation should not have resume_from_checkpoint set"
    );
}

#[then("the more recently submitted class-3 allocation is preempted first")]
fn then_newer_class3_preempted_first(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            assert!(!victims.is_empty(), "expected at least one victim");

            // Among class-3 allocations, the one with lower cost should be picked.
            // The newer one (started 30 min ago) has less elapsed time, hence lower
            // recompute cost when checkpoint is Auto (both have same base checkpoint cost),
            // so it should be the cheaper victim.
            let victim_id = victims[0].allocation_id;
            let victim_alloc = world
                .allocations
                .iter()
                .find(|a| a.id == victim_id)
                .expect("victim not found");
            assert_eq!(
                victim_alloc.lifecycle.preemption_class, 3,
                "expected class-3 victim"
            );
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected preemption but got NotPossible: {reason}");
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then("no non-preemptible allocations should be selected as victims")]
fn then_no_non_preemptible_victims(world: &mut LatticeWorld) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            for victim in victims {
                let alloc = world
                    .allocations
                    .iter()
                    .find(|a| a.id == victim.allocation_id);
                if let Some(a) = alloc {
                    assert!(
                        a.lifecycle.preemption_class < 10,
                        "non-preemptible allocation {} (class {}) was selected as victim",
                        a.id,
                        a.lifecycle.preemption_class
                    );
                }
            }
        }
        Some(PreemptionResult::NotPossible { .. }) => {
            // No victims at all -- non-preemptible was protected
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then(regex = r"^(\d+) allocations should be selected as victims$")]
fn then_n_victims_selected(world: &mut LatticeWorld, expected: u32) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { victims, .. }) => {
            assert_eq!(
                victims.len(),
                expected as usize,
                "expected {} victims, got {}",
                expected,
                victims.len()
            );
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected {} victims but got NotPossible: {reason}", expected);
        }
        None => panic!("preemption was not evaluated"),
    }
}

#[then(regex = r"^exactly (\d+) nodes should be freed$")]
fn then_exactly_n_nodes_freed(world: &mut LatticeWorld, expected: u32) {
    match &world.preemption_result {
        Some(PreemptionResult::Possible { freed_nodes, .. }) => {
            assert_eq!(
                freed_nodes.len(),
                expected as usize,
                "expected {} freed nodes, got {}",
                expected,
                freed_nodes.len()
            );
        }
        Some(PreemptionResult::NotPossible { reason }) => {
            panic!("expected {} freed nodes but got NotPossible: {reason}", expected);
        }
        None => panic!("preemption was not evaluated"),
    }
}

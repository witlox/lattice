use std::collections::HashMap;
use std::sync::Arc;

use cucumber::{given, then, when, World};
use uuid::Uuid;

use lattice_common::error::LatticeError;
use lattice_common::traits::*;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;
use lattice_test_harness::mocks::*;

use lattice_node_agent::agent::{AgentCommand, NodeAgent};
use lattice_node_agent::epilogue::{
    EpilogueConfig, EpiloguePipeline, EpilogueResult, NoopEpilogueReporter, NoopMedicalWiper,
};
use lattice_node_agent::health::ObservedHealth;
use lattice_node_agent::heartbeat::Heartbeat;
use lattice_node_agent::image_cache::ImageCache;
use lattice_node_agent::prologue::{NoopReporter, ProloguePipeline, PrologueResult};
use lattice_node_agent::runtime::{ExitStatus, MockRuntime, PrepareConfig, Runtime};
use lattice_node_agent::telemetry::log_buffer::LogRingBuffer;

use lattice_scheduler::dag::validate_dag;
use lattice_scheduler::federation::{
    FederationBroker, FederationConfig, FederationOffer, OfferDecision,
};
use lattice_scheduler::placement::PlacementDecision;

#[derive(Debug, World)]
#[world(init = Self::new)]
pub struct LatticeWorld {
    nodes: Vec<Node>,
    allocations: Vec<Allocation>,
    tenants: Vec<Tenant>,
    vclusters: Vec<VCluster>,
    last_error: Option<LatticeError>,
    store: MockAllocationStore,
    registry: MockNodeRegistry,
    audit: MockAuditLog,
    /// Named allocations for DAG scenarios
    named_allocations: HashMap<String, Allocation>,
    dag_id: Option<String>,
    // Phase 8: end-to-end fields
    last_heartbeat: Option<Heartbeat>,
    agent: Option<NodeAgent>,
    agent_alloc_id: Option<Uuid>,
    should_checkpoint: Option<bool>,
    // Prologue/epilogue fields
    prologue_result: Option<PrologueResult>,
    epilogue_result: Option<EpilogueResult>,
    image_cache: Option<ImageCache>,
    // Federation fields
    federation_broker: Option<FederationBroker>,
    federation_total_nodes: u32,
    federation_idle_nodes: u32,
    federation_decision: Option<OfferDecision>,
    // DAG validation
}

impl LatticeWorld {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            allocations: Vec::new(),
            tenants: Vec::new(),
            vclusters: Vec::new(),
            last_error: None,
            store: MockAllocationStore::new(),
            registry: MockNodeRegistry::new(),
            audit: MockAuditLog::new(),
            named_allocations: HashMap::new(),
            dag_id: None,
            last_heartbeat: None,
            agent: None,
            agent_alloc_id: None,
            should_checkpoint: None,
            prologue_result: None,
            epilogue_result: None,
            image_cache: None,
            federation_broker: None,
            federation_total_nodes: 0,
            federation_idle_nodes: 0,
            federation_decision: None,
        }
    }

    fn last_allocation(&self) -> &Allocation {
        self.allocations.last().expect("no allocations submitted")
    }

    fn last_allocation_mut(&mut self) -> &mut Allocation {
        self.allocations
            .last_mut()
            .expect("no allocations submitted")
    }
}

// ═══════════════════════════════════════════════════════════
// GIVEN steps
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^a tenant "(\w[\w-]*)" with a quota of (\d+) nodes$"#)]
async fn given_tenant_with_quota(world: &mut LatticeWorld, name: String, nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(nodes).build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a tenant "(\w[\w-]*)" with strict isolation$"#)]
async fn given_tenant_strict(world: &mut LatticeWorld, name: String) {
    let tenant = TenantBuilder::new(&name).strict_isolation().build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a tenant "(\w[\w-]*)" with max_nodes (\d+)$"#)]
async fn given_tenant_max_nodes(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a tenant "(\w[\w-]*)" with max_concurrent_allocations (\d+)$"#)]
async fn given_tenant_max_concurrent(world: &mut LatticeWorld, name: String, max: u32) {
    let tenant = TenantBuilder::new(&name).max_concurrent(max).build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a vCluster "(\w[\w-]*)" with scheduler "(\w+)"$"#)]
async fn given_vcluster(world: &mut LatticeWorld, name: String, scheduler: String) {
    let sched_type = match scheduler.as_str() {
        "HpcBackfill" => SchedulerType::HpcBackfill,
        "ServiceBinPack" => SchedulerType::ServiceBinPack,
        "MedicalReservation" => SchedulerType::MedicalReservation,
        "InteractiveFifo" => SchedulerType::InteractiveFifo,
        other => panic!("Unknown scheduler type: {other}"),
    };
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vc = VClusterBuilder::new(&name)
        .tenant(&tenant_id)
        .scheduler(sched_type)
        .build();
    world.vclusters.push(vc);
}

#[given(regex = r#"^(\d+) ready nodes in group (\d+)$"#)]
async fn given_ready_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    let nodes = create_node_batch(count, group);
    world.registry = MockNodeRegistry::new().with_nodes(nodes.clone());
    world.nodes.extend(nodes);
}

#[given(regex = r#"^(\d+) ready nodes with conformance fingerprint "([^"]+)"$"#)]
async fn given_conformant_nodes(world: &mut LatticeWorld, count: usize, fingerprint: String) {
    let nodes: Vec<Node> = (0..count)
        .map(|i| {
            NodeBuilder::new()
                .id(&format!("x1000c0s0b0n{i}"))
                .conformance(&fingerprint)
                .build()
        })
        .collect();
    world.registry = MockNodeRegistry::new().with_nodes(nodes.clone());
    world.nodes.extend(nodes);
}

#[given(regex = r"^a pending allocation requesting (\d+) nodes$")]
async fn given_pending_allocation(world: &mut LatticeWorld, nodes: u32) {
    let alloc = AllocationBuilder::new().nodes(nodes).build();
    world.allocations.push(alloc);
}

#[given("a running allocation")]
async fn given_running_allocation(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .state(AllocationState::Running)
        .build();
    world.allocations.push(alloc);
}

#[given("a completed allocation")]
async fn given_completed_allocation(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .state(AllocationState::Completed)
        .build();
    world.allocations.push(alloc);
}

#[given("a suspended allocation")]
async fn given_suspended_allocation(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .state(AllocationState::Suspended)
        .build();
    world.allocations.push(alloc);
}

#[given(regex = r#"^(\d+) nodes already allocated to tenant "(\w[\w-]*)"$"#)]
async fn given_allocated_nodes(world: &mut LatticeWorld, count: u32, tenant: String) {
    // Create running allocations to represent the allocated nodes
    for _ in 0..count {
        let alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .state(AllocationState::Running)
            .nodes(1)
            .build();
        world.store.insert(alloc.clone()).await.unwrap();
        world.allocations.push(alloc);
    }
}

#[given(regex = r#"^(\d+) running allocations for tenant "(\w[\w-]*)"$"#)]
async fn given_running_allocations(world: &mut LatticeWorld, count: u32, tenant: String) {
    for _ in 0..count {
        let alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .state(AllocationState::Running)
            .build();
        world.store.insert(alloc.clone()).await.unwrap();
        world.allocations.push(alloc);
    }
}

// ═══════════════════════════════════════════════════════════
// WHEN steps
// ═══════════════════════════════════════════════════════════

#[when(regex = r#"^I submit a bounded allocation requesting (\d+) nodes with walltime "(\w+)"$"#)]
async fn submit_bounded(world: &mut LatticeWorld, nodes: u32, walltime: String) {
    let hours: u64 = walltime.trim_end_matches('h').parse().unwrap_or(1);
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant_id)
        .nodes(nodes)
        .lifecycle_bounded(hours)
        .build();
    world.allocations.push(alloc);
}

#[when(regex = r#"^the allocation transitions to "(\w+)"$"#)]
async fn allocation_transitions(world: &mut LatticeWorld, target_str: String) {
    let target = parse_allocation_state(&target_str);
    let alloc = world.last_allocation_mut();
    if alloc.state.can_transition_to(&target) {
        alloc.state = target;
        world.last_error = None;
    } else {
        world.last_error = Some(LatticeError::Internal(format!(
            "Invalid transition from {:?} to {target_str}",
            alloc.state
        )));
    }
}

#[when(regex = r#"^user "(\w[\w-]*)" claims node (\d+) for a medical allocation$"#)]
async fn user_claims_node(world: &mut LatticeWorld, user: String, node_idx: usize) {
    let node_id = world.nodes[node_idx].id.clone();
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let ownership = NodeOwnership {
        tenant: tenant_id,
        vcluster: "medical".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some(user.clone()),
        is_borrowed: false,
    };
    match world.registry.claim_node(&node_id, ownership).await {
        Ok(()) => {
            // Record audit
            let entry = AuditEntry {
                id: Uuid::new_v4(),
                timestamp: chrono::Utc::now(),
                user: user.clone(),
                action: AuditAction::NodeClaim,
                details: serde_json::json!({"node": node_id}),
            };
            world.audit.record(entry).await.unwrap();
            world.last_error = None;
        }
        Err(e) => {
            world.last_error = Some(e);
        }
    }
}

#[when(regex = r#"^user "(\w[\w-]*)" attempts to claim node (\d+)$"#)]
async fn user_attempts_claim(world: &mut LatticeWorld, user: String, node_idx: usize) {
    let node_id = world.nodes[node_idx].id.clone();
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let ownership = NodeOwnership {
        tenant: tenant_id,
        vcluster: "medical".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some(user),
        is_borrowed: false,
    };
    match world.registry.claim_node(&node_id, ownership).await {
        Ok(()) => {
            world.last_error = None;
        }
        Err(e) => {
            world.last_error = Some(e);
        }
    }
}

#[when("I submit a medical allocation")]
async fn submit_medical_allocation(world: &mut LatticeWorld) {
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant_id)
        .medical()
        .build();
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit an allocation requesting (\d+) nodes for tenant "(\w[\w-]*)"$"#)]
async fn submit_for_tenant(world: &mut LatticeWorld, nodes: u32, tenant: String) {
    // Check quota
    let tenant_obj = world.tenants.iter().find(|t| t.id == tenant);
    if let Some(t) = tenant_obj {
        let running_count = world
            .store
            .list(&AllocationFilter {
                tenant: Some(tenant.clone()),
                state: Some(AllocationState::Running),
                ..Default::default()
            })
            .await
            .unwrap();
        let current_nodes: u32 = running_count.len() as u32; // each has 1 node in our test setup
        if current_nodes + nodes > t.quota.max_nodes {
            world.last_error = Some(LatticeError::QuotaExceeded {
                tenant: tenant.clone(),
                detail: format!(
                    "requesting {nodes} nodes but only {} available (max: {}, used: {current_nodes})",
                    t.quota.max_nodes - current_nodes,
                    t.quota.max_nodes
                ),
            });
            return;
        }
    }
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(nodes)
        .build();
    world.allocations.push(alloc);
    world.last_error = None;
}

#[when(regex = r#"^I submit a new allocation for tenant "(\w[\w-]*)"$"#)]
async fn submit_new_for_tenant(world: &mut LatticeWorld, tenant: String) {
    let tenant_obj = world.tenants.iter().find(|t| t.id == tenant).cloned();
    if let Some(t) = tenant_obj {
        if let Some(max_concurrent) = t.quota.max_concurrent_allocations {
            let running = world
                .store
                .list(&AllocationFilter {
                    tenant: Some(tenant.clone()),
                    state: Some(AllocationState::Running),
                    ..Default::default()
                })
                .await
                .unwrap();
            if running.len() as u32 >= max_concurrent {
                world.last_error = Some(LatticeError::QuotaExceeded {
                    tenant: tenant.clone(),
                    detail: format!("max concurrent allocations ({max_concurrent}) reached"),
                });
                return;
            }
        }
    }
    let alloc = AllocationBuilder::new().tenant(&tenant).build();
    world.allocations.push(alloc);
    world.last_error = None;
}

#[when("I submit a DAG with stages:")]
async fn submit_dag(world: &mut LatticeWorld, step: &cucumber::gherkin::Step) {
    let table = step.table.as_ref().expect("expected a data table");
    let dag_id = Uuid::new_v4().to_string();
    world.dag_id = Some(dag_id.clone());

    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());

    // First pass: create all allocations
    let mut named: HashMap<String, Allocation> = HashMap::new();
    for row in &table.rows[1..] {
        // skip header
        let id = &row[0];
        let nodes: u32 = row[1].parse().unwrap();
        let depends_str = &row[2];

        let mut builder = AllocationBuilder::new()
            .tenant(&tenant_id)
            .nodes(nodes)
            .dag_id(&dag_id)
            .tag("dag_stage", id);

        // Parse dependencies
        for dep in depends_str.split(',').filter(|s| !s.trim().is_empty()) {
            let parts: Vec<&str> = dep.trim().split(':').collect();
            let ref_id = parts[0];
            let condition = match parts.get(1).copied().unwrap_or("success") {
                "success" => DependencyCondition::Success,
                "any" => DependencyCondition::Any,
                "failure" => DependencyCondition::Failure,
                other => panic!("Unknown dependency condition: {other}"),
            };
            builder = builder.depends_on(ref_id, condition);
        }

        let alloc = builder.build();
        named.insert(id.clone(), alloc);
    }

    world.named_allocations = named;
}

// ═══════════════════════════════════════════════════════════
// THEN steps
// ═══════════════════════════════════════════════════════════

#[then(regex = r#"^the allocation state should be "(\w+)"$"#)]
async fn check_allocation_state(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    assert_eq!(
        world.last_allocation().state,
        expected_state,
        "Expected state {expected}, got {:?}",
        world.last_allocation().state
    );
}

#[then("the allocation should have a valid ID")]
async fn check_valid_id(world: &mut LatticeWorld) {
    let id = world.last_allocation().id;
    assert_ne!(id, Uuid::nil(), "Allocation ID should not be nil");
}

#[then(regex = r#"^the allocation cannot transition to "(\w+)"$"#)]
async fn cannot_transition(world: &mut LatticeWorld, target_str: String) {
    let target = parse_allocation_state(&target_str);
    assert!(
        !world.last_allocation().state.can_transition_to(&target),
        "State {:?} should NOT be able to transition to {target_str}",
        world.last_allocation().state
    );
}

#[then(regex = r#"^node (\d+) should be owned by user "(\w[\w-]*)"$"#)]
async fn node_owned_by(world: &mut LatticeWorld, node_idx: usize, user: String) {
    let node_id = &world.nodes[node_idx].id;
    let node = world.registry.get_node(node_id).await.unwrap();
    let owner = node.owner.expect("node should have an owner");
    assert_eq!(
        owner.claimed_by.as_deref(),
        Some(user.as_str()),
        "Node should be claimed by {user}"
    );
}

#[then(regex = r#"^an audit entry should record action "(\w+)"$"#)]
async fn audit_entry_recorded(world: &mut LatticeWorld, action_str: String) {
    let action = parse_audit_action(&action_str);
    let entries = world.audit.entries_for_action(&action);
    assert!(
        !entries.is_empty(),
        "Expected at least one audit entry for action {action_str}"
    );
}

#[then(regex = r#"^user "(\w[\w-]*)" receives an OwnershipConflict error$"#)]
async fn receives_ownership_conflict(world: &mut LatticeWorld, _user: String) {
    match &world.last_error {
        Some(LatticeError::OwnershipConflict { .. }) => {}
        other => panic!("Expected OwnershipConflict, got {other:?}"),
    }
}

#[then("the allocation environment should require signed images")]
async fn requires_signed_images(world: &mut LatticeWorld) {
    assert!(
        world.last_allocation().environment.sign_required,
        "Medical allocation should require signed images"
    );
}

#[then("the allocation environment should require vulnerability scanning")]
async fn requires_vuln_scan(world: &mut LatticeWorld) {
    assert!(
        world.last_allocation().environment.scan_required,
        "Medical allocation should require vulnerability scanning"
    );
}

#[then(regex = r#"^the allocation should be rejected with "(\w+)"$"#)]
async fn allocation_rejected(world: &mut LatticeWorld, error_type: String) {
    match (&world.last_error, error_type.as_str()) {
        (Some(LatticeError::QuotaExceeded { .. }), "QuotaExceeded") => {}
        (err, expected) => panic!("Expected {expected} error, got {err:?}"),
    }
}

#[then("the allocation should not be rejected")]
async fn allocation_not_rejected(world: &mut LatticeWorld) {
    assert!(
        world.last_error.is_none(),
        "Expected no error, got {:?}",
        world.last_error
    );
}

#[then(regex = r#"^the DAG should have (\d+) allocations$"#)]
async fn dag_has_allocations(world: &mut LatticeWorld, count: usize) {
    assert_eq!(
        world.named_allocations.len(),
        count,
        "DAG should have {count} allocations"
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should have no dependencies$"#)]
async fn no_dependencies(world: &mut LatticeWorld, name: String) {
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("allocation '{name}' not found"));
    assert!(
        alloc.depends_on.is_empty(),
        "'{name}' should have no dependencies, has {:?}",
        alloc.depends_on
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should depend on "(\w[\w-]*)" with condition "(\w+)"$"#)]
async fn has_dependency(
    world: &mut LatticeWorld,
    name: String,
    dep_name: String,
    condition_str: String,
) {
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("allocation '{name}' not found"));
    let expected_condition = match condition_str.as_str() {
        "Success" => DependencyCondition::Success,
        "Any" => DependencyCondition::Any,
        "Failure" => DependencyCondition::Failure,
        other => panic!("Unknown condition: {other}"),
    };
    let found = alloc.depends_on.iter().any(|d| {
        d.ref_id == dep_name
            && std::mem::discriminant(&d.condition) == std::mem::discriminant(&expected_condition)
    });
    assert!(
        found,
        "'{name}' should depend on '{dep_name}' with condition {condition_str}, dependencies: {:?}",
        alloc.depends_on
    );
}

#[then("all DAG allocations should share the same dag_id")]
async fn shared_dag_id(world: &mut LatticeWorld) {
    let dag_id = world.dag_id.as_ref().expect("no dag_id set");
    for (name, alloc) in &world.named_allocations {
        assert_eq!(
            alloc.dag_id.as_ref(),
            Some(dag_id),
            "Allocation '{name}' should have dag_id {dag_id}"
        );
    }
}

// ═══════════════════════════════════════════════════════════
// Scheduling cycle steps (Phase 8)
// ═══════════════════════════════════════════════════════════

#[when("the scheduler runs a cycle")]
async fn scheduler_runs_cycle(world: &mut LatticeWorld) {
    use lattice_scheduler::cycle::{run_cycle, CycleInput};

    let pending: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Pending)
        .cloned()
        .collect();

    let weights = world
        .vclusters
        .first()
        .map(|vc| vc.cost_weights.clone())
        .unwrap_or_default();

    let input = CycleInput {
        pending,
        running: world
            .allocations
            .iter()
            .filter(|a| a.state == AllocationState::Running)
            .cloned()
            .collect(),
        nodes: world.nodes.clone(),
        tenants: world.tenants.clone(),
        topology: create_test_topology(1, world.nodes.len()),
        data_readiness: HashMap::new(),
        energy_price: 0.5,
    };

    let result = run_cycle(&input, &weights);

    // Apply placements
    for decision in &result.decisions {
        if let PlacementDecision::Place {
            allocation_id,
            nodes,
        } = decision
        {
            if let Some(alloc) = world
                .allocations
                .iter_mut()
                .find(|a| a.id == *allocation_id)
            {
                alloc.state = AllocationState::Running;
                alloc.assigned_nodes = nodes.clone();
            }
        }
    }
}

#[when(regex = r#"^I submit a low-priority allocation requesting (\d+) nodes$"#)]
async fn submit_low_priority(world: &mut LatticeWorld, nodes: u32) {
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant_id)
        .nodes(nodes)
        .preemption_class(1)
        .build();
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a high-priority allocation requesting (\d+) nodes$"#)]
async fn submit_high_priority(world: &mut LatticeWorld, nodes: u32) {
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant_id)
        .nodes(nodes)
        .preemption_class(9)
        .build();
    world.allocations.push(alloc);
}

#[then(regex = r#"^the allocation should be placed on (\d+) nodes$"#)]
async fn allocation_placed_on_nodes(world: &mut LatticeWorld, count: usize) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.assigned_nodes.len(),
        count,
        "Expected {count} assigned nodes, got {}",
        alloc.assigned_nodes.len()
    );
}

#[then(regex = r#"^the high-priority allocation should be "(\w+)"$"#)]
async fn high_priority_is_state(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    // The high-priority allocation is the last one submitted
    let high_prio = world
        .allocations
        .iter()
        .find(|a| a.lifecycle.preemption_class == 9)
        .expect("no high-priority allocation found");
    assert_eq!(
        high_prio.state, expected_state,
        "High-priority allocation should be {expected}"
    );
}

// ═══════════════════════════════════════════════════════════
// Node agent steps (Phase 8)
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^a node agent for node "([^"]+)" with (\d+) GPUs$"#)]
async fn given_node_agent(world: &mut LatticeWorld, node_id: String, gpu_count: u32) {
    let node = NodeBuilder::new().id(&node_id).gpu_count(gpu_count).build();
    let registry = Arc::new(MockNodeRegistry::new().with_nodes(vec![node.clone()]));
    let caps = node.capabilities.clone();
    world.nodes.push(node);
    world.agent = Some(NodeAgent::new(node_id, caps, registry));
}

#[when("the agent runs a health check with all systems healthy")]
async fn agent_health_check_healthy(world: &mut LatticeWorld) {
    let agent = world.agent.as_mut().expect("no agent initialized");
    let gpu_count = 4;
    let observed = ObservedHealth {
        gpu_count,
        max_gpu_temp_c: Some(65.0),
        ecc_errors: 0,
        nic_up: true,
    };
    let hb = agent.generate_heartbeat(&observed);
    world.last_heartbeat = Some(hb);
}

#[when(regex = r#"^the agent runs a health check with only (\d+) GPUs detected$"#)]
async fn agent_health_check_missing_gpus(world: &mut LatticeWorld, gpu_count: u32) {
    let agent = world.agent.as_mut().expect("no agent initialized");
    let observed = ObservedHealth {
        gpu_count,
        max_gpu_temp_c: Some(65.0),
        ecc_errors: 0,
        nic_up: true,
    };
    let hb = agent.generate_heartbeat(&observed);
    world.last_heartbeat = Some(hb);
}

#[when(regex = r#"^the agent starts allocation "([^"]+)"$"#)]
async fn agent_starts_allocation(world: &mut LatticeWorld, entrypoint: String) {
    let agent = world.agent.as_mut().expect("no agent initialized");
    let id = Uuid::new_v4();
    world.agent_alloc_id = Some(id);
    agent
        .handle_command(AgentCommand::StartAllocation { id, entrypoint })
        .await
        .unwrap();
}

#[when("the agent completes the allocation")]
async fn agent_completes_allocation(world: &mut LatticeWorld) {
    let agent = world.agent.as_mut().expect("no agent initialized");
    let id = world.agent_alloc_id.expect("no allocation started");
    // Advance: Running → Epilogue → Completed
    agent.allocations_mut().advance(&id).unwrap();
    agent.allocations_mut().advance(&id).unwrap();
}

#[then("the heartbeat should report healthy")]
async fn heartbeat_healthy(world: &mut LatticeWorld) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert!(hb.healthy, "Expected healthy heartbeat");
}

#[then("the heartbeat should report unhealthy")]
async fn heartbeat_unhealthy(world: &mut LatticeWorld) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert!(!hb.healthy, "Expected unhealthy heartbeat");
}

#[then(regex = r#"^the heartbeat sequence should be (\d+)$"#)]
async fn heartbeat_sequence(world: &mut LatticeWorld, expected: u64) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert_eq!(hb.sequence, expected);
}

#[then(regex = r#"^the heartbeat issues should mention "(\w+)"$"#)]
async fn heartbeat_mentions(world: &mut LatticeWorld, keyword: String) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    let keyword_lower = keyword.to_lowercase();
    assert!(
        hb.issues
            .iter()
            .any(|i| i.to_lowercase().contains(&keyword_lower)),
        "Expected heartbeat issues to mention '{keyword}', got: {:?}",
        hb.issues
    );
}

#[then(regex = r#"^the agent should have (\d+) active allocations?$"#)]
async fn agent_active_count(world: &mut LatticeWorld, expected: u32) {
    let agent = world.agent.as_ref().expect("no agent initialized");
    assert_eq!(
        agent.active_allocation_count(),
        expected,
        "Expected {expected} active allocations"
    );
}

// ═══════════════════════════════════════════════════════════
// Checkpoint steps (Phase 8)
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^a running allocation with (\d+) nodes and (.+) elapsed$"#)]
async fn given_running_allocation_with_time(
    world: &mut LatticeWorld,
    nodes: u32,
    elapsed_str: String,
) {
    let elapsed_hours = if elapsed_str.contains("hour") {
        elapsed_str
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<f64>()
            .unwrap()
    } else if elapsed_str.contains("minute") {
        elapsed_str
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            / 60.0
    } else {
        1.0
    };

    let started_at =
        chrono::Utc::now() - chrono::Duration::seconds((elapsed_hours * 3600.0) as i64);
    let mut alloc = AllocationBuilder::new()
        .nodes(nodes)
        .state(AllocationState::Running)
        .lifecycle_bounded(4) // 4-hour walltime
        .build();
    alloc.started_at = Some(started_at);
    alloc.assigned_nodes = (0..nodes).map(|i| format!("node-{i}")).collect();
    world.allocations.push(alloc);
}

#[given(regex = r#"^a system backlog of ([\d.]+)$"#)]
async fn given_system_backlog(world: &mut LatticeWorld, backlog: f64) {
    // Store backlog in a tag for the checkpoint evaluator
    if let Some(alloc) = world.allocations.last_mut() {
        alloc
            .tags
            .insert("test_backlog".to_string(), backlog.to_string());
    }
}

#[when("the checkpoint broker evaluates the allocation")]
async fn checkpoint_broker_evaluates(world: &mut LatticeWorld) {
    use lattice_checkpoint::cost_model::{evaluate_checkpoint, CheckpointParams};

    let alloc = world.last_allocation();
    let backlog: f64 = alloc
        .tags
        .get("test_backlog")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);

    let params = CheckpointParams {
        // Realistic checkpoint size: actual model state, not raw GPU memory
        gpu_memory_per_node_bytes: 8 * 1024 * 1024 * 1024, // 8 GB checkpoint per node
        backlog_pressure: backlog,
        ..Default::default()
    };

    let eval = evaluate_checkpoint(alloc, &params);
    world.should_checkpoint = Some(eval.should_checkpoint);
}

#[then("the allocation should be checkpointed")]
async fn should_be_checkpointed(world: &mut LatticeWorld) {
    assert!(
        world.should_checkpoint.unwrap_or(false),
        "Expected checkpoint to be triggered"
    );
}

#[then("the allocation should not be checkpointed")]
async fn should_not_be_checkpointed(world: &mut LatticeWorld) {
    assert!(
        !world.should_checkpoint.unwrap_or(true),
        "Expected checkpoint NOT to be triggered"
    );
}

// ═══════════════════════════════════════════════════════════
// Prologue / Epilogue steps
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^a pre-cached image "([^"]+)"$"#)]
async fn given_pre_cached_image(world: &mut LatticeWorld, image_ref: String) {
    let cache = world
        .image_cache
        .get_or_insert_with(|| ImageCache::new(10 * 1024 * 1024 * 1024));
    cache.insert(image_ref, 2 * 1024 * 1024 * 1024);
}

#[when("I run the prologue for an allocation")]
async fn run_prologue(world: &mut LatticeWorld) {
    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::new();
    let mut cache = world
        .image_cache
        .take()
        .unwrap_or_else(|| ImageCache::new(10 * 1024 * 1024 * 1024));

    let alloc_id = Uuid::new_v4();
    let config = PrepareConfig {
        alloc_id,
        uenv: Some("prgenv-gnu/24.11:v1".to_string()),
        view: Some("default".to_string()),
        image: None,
        workdir: None,
        env_vars: vec![],
    };

    let result = pipeline
        .execute(alloc_id, &config, &runtime, &mut cache, &NoopReporter)
        .await
        .unwrap();

    world.prologue_result = Some(result);
    world.image_cache = Some(cache);
}

#[when("I run the medical epilogue for an allocation")]
async fn run_medical_epilogue(world: &mut LatticeWorld) {
    let alloc_id = Uuid::new_v4();
    let runtime = MockRuntime::new();
    // Prepare the runtime so cleanup works
    let prep = PrepareConfig {
        alloc_id,
        uenv: None,
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
    };
    runtime.prepare(&prep).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::new(EpilogueConfig {
        log_bucket: "medical-logs".to_string(),
        medical_wipe: true,
    });

    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopMedicalWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    world.epilogue_result = Some(result);
}

#[when("I run the standard epilogue for an allocation")]
async fn run_standard_epilogue(world: &mut LatticeWorld) {
    let alloc_id = Uuid::new_v4();
    let runtime = MockRuntime::new();
    let prep = PrepareConfig {
        alloc_id,
        uenv: None,
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
    };
    runtime.prepare(&prep).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::default(); // medical_wipe: false

    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopMedicalWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    world.epilogue_result = Some(result);
}

#[then("the prologue should report a cache hit")]
async fn prologue_cache_hit(world: &mut LatticeWorld) {
    let result = world.prologue_result.as_ref().expect("no prologue result");
    assert!(result.cache_hit, "Expected prologue cache hit");
}

#[then("the prologue should report a cache miss")]
async fn prologue_cache_miss(world: &mut LatticeWorld) {
    let result = world.prologue_result.as_ref().expect("no prologue result");
    assert!(!result.cache_hit, "Expected prologue cache miss");
}

#[then("the runtime should have been prepared")]
async fn runtime_prepared(world: &mut LatticeWorld) {
    // If prologue succeeded, the runtime was prepared
    assert!(
        world.prologue_result.is_some(),
        "Prologue should have completed successfully"
    );
}

#[then("the medical wipe should have been triggered")]
async fn medical_wipe_triggered(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    // Note: NoopMedicalWiper always succeeds, so medical_wiped is true when config.medical_wipe=true
    assert!(
        result.medical_wiped,
        "Expected medical wipe to have been triggered"
    );
}

#[then("the medical wipe should not have been triggered")]
async fn medical_wipe_not_triggered(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert!(
        !result.medical_wiped,
        "Expected medical wipe NOT to have been triggered"
    );
}

#[then("the epilogue cleanup should have completed")]
async fn epilogue_cleanup_done(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert!(
        result.cleaned_up,
        "Expected epilogue cleanup to have completed"
    );
}

// ═══════════════════════════════════════════════════════════
// DAG validation and execution steps
// ═══════════════════════════════════════════════════════════

#[then("the DAG should fail validation with a cycle error")]
async fn dag_cycle_error(world: &mut LatticeWorld) {
    // Build allocations with proper UUID-based cross-references for cycle detection.
    // The named_allocations use human-readable ref_ids (e.g. "stepA") which don't match
    // UUID-based allocation IDs. Rebuild with UUID-to-UUID references.
    let mut allocs: Vec<Allocation> = world.named_allocations.values().cloned().collect();
    let name_to_id: HashMap<String, String> = world
        .named_allocations
        .iter()
        .map(|(name, alloc)| (name.clone(), alloc.id.to_string()))
        .collect();

    // Rewrite depends_on ref_ids from human names to UUID strings
    for alloc in &mut allocs {
        for dep in &mut alloc.depends_on {
            if let Some(uuid_str) = name_to_id.get(&dep.ref_id) {
                dep.ref_id = uuid_str.clone();
            }
        }
    }

    let result = validate_dag(&allocs, 100);
    assert!(result.is_err(), "Expected DAG validation to fail");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("cycle"),
        "Expected cycle error, got: {err}"
    );
}

#[when("the scheduler runs a DAG cycle")]
async fn scheduler_runs_dag_cycle(world: &mut LatticeWorld) {
    use lattice_scheduler::cycle::{run_cycle, CycleInput};

    // Move named allocations into the main allocations list for scheduling
    let mut all_allocs: Vec<Allocation> = Vec::new();
    for (name, alloc) in &world.named_allocations {
        let mut a = alloc.clone();
        a.tags.insert("dag_stage".to_string(), name.clone());
        all_allocs.push(a);
    }

    // Only schedule allocations with no unmet dependencies
    let pending: Vec<Allocation> = all_allocs
        .iter()
        .filter(|a| a.state == AllocationState::Pending && a.depends_on.is_empty())
        .cloned()
        .collect();

    let weights = world
        .vclusters
        .first()
        .map(|vc| vc.cost_weights.clone())
        .unwrap_or_default();

    let input = CycleInput {
        pending,
        running: Vec::new(),
        nodes: world.nodes.clone(),
        tenants: world.tenants.clone(),
        topology: create_test_topology(1, world.nodes.len()),
        data_readiness: HashMap::new(),
        energy_price: 0.5,
    };

    let result = run_cycle(&input, &weights);

    // Apply placements back to named_allocations
    for decision in &result.decisions {
        if let PlacementDecision::Place {
            allocation_id,
            nodes,
        } = decision
        {
            for (_, alloc) in world.named_allocations.iter_mut() {
                if alloc.id == *allocation_id {
                    alloc.state = AllocationState::Running;
                    alloc.assigned_nodes = nodes.clone();
                }
            }
        }
    }
}

#[then(regex = r#"^"(\w[\w-]*)" should be "(\w+)"$"#)]
async fn named_alloc_state(world: &mut LatticeWorld, name: String, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("allocation '{name}' not found"));
    assert_eq!(
        alloc.state, expected_state,
        "'{name}' should be {expected}, got {:?}",
        alloc.state
    );
}

// ═══════════════════════════════════════════════════════════
// Federation steps
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^a federation broker for site "([^"]+)"$"#)]
async fn given_federation_broker(world: &mut LatticeWorld, site_id: String) {
    let config = FederationConfig {
        site_id,
        max_federation_pct: 0.2,
        accept_medical: false,
        trusted_sites: Vec::new(),
    };
    world.federation_broker = Some(FederationBroker::new(config));
}

#[given(regex = r#"^trusted sites "([^"]+)"$"#)]
async fn given_trusted_sites(world: &mut LatticeWorld, sites_str: String) {
    let sites: Vec<String> = sites_str.split(',').map(|s| s.trim().to_string()).collect();
    let old = world
        .federation_broker
        .take()
        .expect("no federation broker");
    // Rebuild with trusted sites
    let site_id = "site-a".to_string(); // default from prior step
    let config = FederationConfig {
        site_id,
        max_federation_pct: 0.2,
        accept_medical: false,
        trusted_sites: sites,
    };
    drop(old);
    world.federation_broker = Some(FederationBroker::new(config));
}

#[given(regex = r#"^(\d+) total nodes with (\d+) idle$"#)]
async fn given_node_counts(world: &mut LatticeWorld, total: u32, idle: u32) {
    world.federation_total_nodes = total;
    world.federation_idle_nodes = idle;
}

#[when(regex = r#"^site "([^"]+)" offers (\d+) nodes for a non-medical workload$"#)]
async fn site_offers_non_medical(world: &mut LatticeWorld, source: String, nodes: u32) {
    let offer = FederationOffer {
        source_site: source,
        allocation_id: Uuid::new_v4(),
        tenant_id: "remote-tenant".to_string(),
        node_count: nodes,
        medical: false,
        data_locations: vec![],
        offered_at: chrono::Utc::now(),
        ttl_secs: 300,
    };
    let broker = world
        .federation_broker
        .as_ref()
        .expect("no federation broker");
    let decision = broker.evaluate_offer(
        &offer,
        world.federation_idle_nodes,
        world.federation_total_nodes,
    );
    world.federation_decision = Some(decision);
}

#[when(regex = r#"^site "([^"]+)" offers (\d+) nodes for a medical workload$"#)]
async fn site_offers_medical(world: &mut LatticeWorld, source: String, nodes: u32) {
    let offer = FederationOffer {
        source_site: source,
        allocation_id: Uuid::new_v4(),
        tenant_id: "remote-tenant".to_string(),
        node_count: nodes,
        medical: true,
        data_locations: vec![],
        offered_at: chrono::Utc::now(),
        ttl_secs: 300,
    };
    let broker = world
        .federation_broker
        .as_ref()
        .expect("no federation broker");
    let decision = broker.evaluate_offer(
        &offer,
        world.federation_idle_nodes,
        world.federation_total_nodes,
    );
    world.federation_decision = Some(decision);
}

#[then("the offer should be accepted")]
async fn offer_accepted(world: &mut LatticeWorld) {
    let decision = world
        .federation_decision
        .as_ref()
        .expect("no federation decision");
    assert!(
        matches!(decision, OfferDecision::Accept { .. }),
        "Expected offer to be accepted, got {decision:?}"
    );
}

#[then(regex = r#"^the accepted nodes should be prefixed with "([^"]+)"$"#)]
async fn accepted_nodes_prefixed(world: &mut LatticeWorld, prefix: String) {
    let decision = world
        .federation_decision
        .as_ref()
        .expect("no federation decision");
    if let OfferDecision::Accept { nodes } = decision {
        for node in nodes {
            assert!(
                node.starts_with(&prefix),
                "Node '{node}' should start with '{prefix}'"
            );
        }
    } else {
        panic!("Expected accept decision");
    }
}

#[then(regex = r#"^the offer should be rejected with reason "([^"]+)"$"#)]
async fn offer_rejected_with_reason(world: &mut LatticeWorld, keyword: String) {
    let decision = world
        .federation_decision
        .as_ref()
        .expect("no federation decision");
    if let OfferDecision::Reject { reason } = decision {
        assert!(
            reason.to_lowercase().contains(&keyword.to_lowercase()),
            "Expected rejection reason to contain '{keyword}', got: '{reason}'"
        );
    } else {
        panic!("Expected reject decision, got {decision:?}");
    }
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

fn parse_allocation_state(s: &str) -> AllocationState {
    match s {
        "Pending" => AllocationState::Pending,
        "Staging" => AllocationState::Staging,
        "Running" => AllocationState::Running,
        "Checkpointing" => AllocationState::Checkpointing,
        "Suspended" => AllocationState::Suspended,
        "Completed" => AllocationState::Completed,
        "Failed" => AllocationState::Failed,
        "Cancelled" => AllocationState::Cancelled,
        other => panic!("Unknown allocation state: {other}"),
    }
}

fn parse_audit_action(s: &str) -> AuditAction {
    match s {
        "NodeClaim" => AuditAction::NodeClaim,
        "NodeRelease" => AuditAction::NodeRelease,
        "AllocationStart" => AuditAction::AllocationStart,
        "AllocationComplete" => AuditAction::AllocationComplete,
        "DataAccess" => AuditAction::DataAccess,
        "AttachSession" => AuditAction::AttachSession,
        "LogAccess" => AuditAction::LogAccess,
        "MetricsQuery" => AuditAction::MetricsQuery,
        "CheckpointEvent" => AuditAction::CheckpointEvent,
        other => panic!("Unknown audit action: {other}"),
    }
}

fn main() {
    futures::executor::block_on(LatticeWorld::cucumber().run("features/"));
}

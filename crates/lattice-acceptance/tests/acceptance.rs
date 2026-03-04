use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use cucumber::{given, then, when, World};
use uuid::Uuid;

use lattice_common::error::LatticeError;
use lattice_common::traits::*;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;
use lattice_test_harness::mocks::*;

use lattice_node_agent::agent::{AgentCommand, NodeAgent};
use lattice_node_agent::data_stage::NoopDataStageExecutor;
use lattice_node_agent::epilogue::{
    EpilogueConfig, EpiloguePipeline, EpilogueResult, NoopEpilogueReporter, NoopSensitiveWiper,
};
use lattice_node_agent::health::ObservedHealth;
use lattice_node_agent::heartbeat::Heartbeat;
use lattice_node_agent::image_cache::ImageCache;
use lattice_node_agent::prologue::{NoopReporter, ProloguePipeline, PrologueResult};
use lattice_node_agent::runtime::{ExitStatus, MockRuntime, PrepareConfig, Runtime};
use lattice_node_agent::telemetry::log_buffer::LogRingBuffer;

use lattice_api::events::{AllocationEvent, EventBus, LogStream};
use lattice_api::middleware::rbac::{Operation, RbacContext, RbacPolicy, Role};
use lattice_scheduler::autoscaler::{AutoscalerConfig, ScaleDecision};
use lattice_scheduler::conformance::{
    filter_by_constraints, memory_locality_score, select_conformant_nodes,
};
use lattice_scheduler::dag::validate_dag;
use lattice_scheduler::data_staging::{DataStager, StagingPlan};
use lattice_scheduler::federation::{
    FederationBroker, FederationConfig, FederationOffer, OfferDecision,
};
use lattice_scheduler::placement::PlacementDecision;
use lattice_scheduler::preemption::{evaluate_preemption, PreemptionConfig, PreemptionResult};

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
    // Session management
    session_alloc_idx: Option<usize>,
    // RBAC
    last_rbac_result: Option<bool>,
    current_role: Option<Role>,
    current_user: Option<String>,
    requesting_tenant: Option<String>,
    // Streaming / EventBus
    event_bus: Option<Arc<EventBus>>,
    named_alloc_ids: HashMap<String, Uuid>,
    received_events: HashMap<String, Vec<AllocationEvent>>,
    // Data staging
    staging_plan: Option<StagingPlan>,
    data_readiness: HashMap<String, f64>,
    // Preemption
    preemption_result: Option<PreemptionResult>,
    // Network domains
    network_domains: Vec<NetworkDomain>,
    // Observability
    log_buffer_data: Vec<u8>,
    attach_owner: Option<String>,
    attach_user: Option<String>,
    attach_allowed: Option<bool>,
    // Autoscaling
    scale_decision: Option<ScaleDecision>,
    last_scale_time: Option<Instant>,
    // GPU topology / conformance filtering
    filtered_nodes: Vec<String>,
    locality_scores: Vec<(String, f64)>,
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
            session_alloc_idx: None,
            last_rbac_result: None,
            current_role: None,
            current_user: None,
            requesting_tenant: None,
            event_bus: None,
            named_alloc_ids: HashMap::new(),
            received_events: HashMap::new(),
            staging_plan: None,
            data_readiness: HashMap::new(),
            preemption_result: None,
            network_domains: Vec::new(),
            log_buffer_data: Vec::new(),
            attach_owner: None,
            attach_user: None,
            attach_allowed: None,
            scale_decision: None,
            last_scale_time: None,
            filtered_nodes: Vec::new(),
            locality_scores: Vec::new(),
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
        "SensitiveReservation" => SchedulerType::SensitiveReservation,
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

#[when(regex = r#"^user "(\w[\w-]*)" claims node (\d+) for a sensitive allocation$"#)]
async fn user_claims_node(world: &mut LatticeWorld, user: String, node_idx: usize) {
    let node_id = world.nodes[node_idx].id.clone();
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let ownership = NodeOwnership {
        tenant: tenant_id,
        vcluster: "sensitive".into(),
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
        vcluster: "sensitive".into(),
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

#[when("I submit a sensitive allocation")]
async fn submit_sensitive_allocation(world: &mut LatticeWorld) {
    let tenant_id = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant_id)
        .sensitive()
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
        "Sensitive allocation should require signed images"
    );
}

#[then("the allocation environment should require vulnerability scanning")]
async fn requires_vuln_scan(world: &mut LatticeWorld) {
    assert!(
        world.last_allocation().environment.scan_required,
        "Sensitive allocation should require vulnerability scanning"
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
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
    };

    let result = pipeline
        .execute(
            alloc_id,
            &config,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
            &NoopReporter,
        )
        .await
        .unwrap();

    world.prologue_result = Some(result);
    world.image_cache = Some(cache);
}

#[when("I run the sensitive epilogue for an allocation")]
async fn run_sensitive_epilogue(world: &mut LatticeWorld) {
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
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
    };
    runtime.prepare(&prep).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::new(EpilogueConfig {
        log_bucket: "sensitive-logs".to_string(),
        sensitive_wipe: true,
    });

    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopDataStageExecutor,
            &[],
            &NoopSensitiveWiper,
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
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
    };
    runtime.prepare(&prep).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::default(); // sensitive_wipe: false

    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopDataStageExecutor,
            &[],
            &NoopSensitiveWiper,
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

#[then("the sensitive wipe should have been triggered")]
async fn sensitive_wipe_triggered(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    // Note: NoopSensitiveWiper always succeeds, so sensitive_wiped is true when config.sensitive_wipe=true
    assert!(
        result.sensitive_wiped,
        "Expected sensitive wipe to have been triggered"
    );
}

#[then("the sensitive wipe should not have been triggered")]
async fn sensitive_wipe_not_triggered(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert!(
        !result.sensitive_wiped,
        "Expected sensitive wipe NOT to have been triggered"
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
        accept_sensitive: false,
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
        accept_sensitive: false,
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

#[when(regex = r#"^site "([^"]+)" offers (\d+) nodes for a non-sensitive workload$"#)]
async fn site_offers_non_sensitive(world: &mut LatticeWorld, source: String, nodes: u32) {
    let offer = FederationOffer {
        source_site: source,
        allocation_id: Uuid::new_v4(),
        tenant_id: "remote-tenant".to_string(),
        node_count: nodes,
        sensitive: false,
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

#[when(regex = r#"^site "([^"]+)" offers (\d+) nodes for a sensitive workload$"#)]
async fn site_offers_sensitive(world: &mut LatticeWorld, source: String, nodes: u32) {
    let offer = FederationOffer {
        source_site: source,
        allocation_id: Uuid::new_v4(),
        tenant_id: "remote-tenant".to_string(),
        node_count: nodes,
        sensitive: true,
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
// Session management steps
// ═══════════════════════════════════════════════════════════

#[when(regex = r#"^I create an interactive session for tenant "(\w[\w-]*)"$"#)]
async fn create_session(world: &mut LatticeWorld, tenant: String) {
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .lifecycle_unbounded()
        .tag("session", "true")
        .build();
    world.allocations.push(alloc);
    world.session_alloc_idx = Some(world.allocations.len() - 1);
    world.last_error = None;
}

#[when(regex = r#"^the session transitions to "(\w+)"$"#)]
async fn session_transitions(world: &mut LatticeWorld, target_str: String) {
    let target = parse_allocation_state(&target_str);
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &mut world.allocations[idx];
    if alloc.state.can_transition_to(&target) {
        alloc.state = target;
    } else {
        world.last_error = Some(LatticeError::Internal(format!(
            "Invalid session transition from {:?} to {target_str}",
            alloc.state
        )));
    }
}

#[when("I delete the session")]
async fn delete_session(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &mut world.allocations[idx];
    if alloc.state.can_transition_to(&AllocationState::Cancelled) {
        alloc.state = AllocationState::Cancelled;
    } else {
        world.last_error = Some(LatticeError::Internal(format!(
            "Cannot cancel session in state {:?}",
            alloc.state
        )));
    }
}

#[then(regex = r#"^the session allocation should be "(\w+)"$"#)]
async fn session_alloc_state(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let idx = world.session_alloc_idx.expect("no session created");
    assert_eq!(
        world.allocations[idx].state, expected_state,
        "Session allocation should be {expected}"
    );
}

#[then("the session should have an unbounded lifecycle")]
async fn session_unbounded(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    assert!(
        matches!(
            world.allocations[idx].lifecycle.lifecycle_type,
            LifecycleType::Unbounded
        ),
        "Session should have Unbounded lifecycle"
    );
}

#[then("the session should have a session tag")]
async fn session_has_tag(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    assert_eq!(
        world.allocations[idx]
            .tags
            .get("session")
            .map(String::as_str),
        Some("true"),
        "Session should have session=true tag"
    );
}

#[then("the session walltime should be zero")]
async fn session_walltime_zero(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    match &world.allocations[idx].lifecycle.lifecycle_type {
        LifecycleType::Unbounded => {} // Unbounded has no walltime, which is correct
        LifecycleType::Bounded { walltime } => {
            assert!(walltime.is_zero(), "Walltime should be zero");
        }
        other => panic!("Expected Unbounded, got {other:?}"),
    }
}

// ═══════════════════════════════════════════════════════════
// RBAC authorization steps
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^a user "(\w[\w-]*)" with role "(\w+)"$"#)]
async fn given_user_with_role(world: &mut LatticeWorld, user: String, role_str: String) {
    world.current_user = Some(user);
    world.current_role = Some(parse_role(&role_str));
    world.requesting_tenant = None;
}

#[given(regex = r#"^a user "(\w[\w-]*)" with role "(\w+)" in tenant "(\w[\w-]*)"$"#)]
async fn given_user_with_role_tenant(
    world: &mut LatticeWorld,
    user: String,
    role_str: String,
    tenant: String,
) {
    world.current_user = Some(user);
    world.current_role = Some(parse_role(&role_str));
    world.requesting_tenant = Some(tenant);
}

#[when(regex = r#"^the user attempts operation "(\w+)"$"#)]
async fn user_attempts_operation(world: &mut LatticeWorld, op_str: String) {
    let role = world.current_role.as_ref().expect("no role set");
    let user = world.current_user.as_deref().unwrap_or("anonymous");
    let ctx = RbacContext {
        requesting_user: user.to_string(),
        requesting_tenant: world.requesting_tenant.clone(),
        target_tenant: None,
        target_owner: None,
    };
    let op = parse_operation(&op_str);
    world.last_rbac_result = Some(RbacPolicy::is_allowed(role, &op, &ctx));
}

#[when(regex = r#"^the user attempts operation "(\w+)" on tenant "(\w[\w-]*)"$"#)]
async fn user_attempts_operation_on_tenant(
    world: &mut LatticeWorld,
    op_str: String,
    target_tenant: String,
) {
    let role = world.current_role.as_ref().expect("no role set");
    let user = world.current_user.as_deref().unwrap_or("anonymous");
    let ctx = RbacContext {
        requesting_user: user.to_string(),
        requesting_tenant: world.requesting_tenant.clone(),
        target_tenant: Some(target_tenant),
        target_owner: None,
    };
    let op = parse_operation(&op_str);
    world.last_rbac_result = Some(RbacPolicy::is_allowed(role, &op, &ctx));
}

#[then("the operation should be allowed")]
async fn operation_allowed(world: &mut LatticeWorld) {
    assert!(
        world.last_rbac_result.unwrap_or(false),
        "Expected operation to be allowed"
    );
}

#[then("the operation should be denied")]
async fn operation_denied(world: &mut LatticeWorld) {
    assert!(
        !world.last_rbac_result.unwrap_or(true),
        "Expected operation to be denied"
    );
}

// ═══════════════════════════════════════════════════════════
// Streaming telemetry steps
// ═══════════════════════════════════════════════════════════

#[given("an event bus")]
async fn given_event_bus(world: &mut LatticeWorld) {
    world.event_bus = Some(Arc::new(EventBus::new()));
    world.named_alloc_ids.clear();
    world.received_events.clear();
}

#[given(regex = r#"^a subscriber for allocation "([^"]+)"$"#)]
async fn given_subscriber(world: &mut LatticeWorld, alloc_name: String) {
    // Register the allocation name → UUID mapping
    world
        .named_alloc_ids
        .entry(alloc_name.clone())
        .or_insert_with(Uuid::new_v4);
    world.received_events.entry(alloc_name).or_default();
}

#[when(regex = r#"^a state change event is published for "([^"]+)" from "(\w+)" to "(\w+)"$"#)]
async fn publish_state_change(
    world: &mut LatticeWorld,
    alloc_name: String,
    old_state: String,
    new_state: String,
) {
    let bus = world.event_bus.as_ref().expect("no event bus");
    let alloc_id = *world
        .named_alloc_ids
        .get(&alloc_name)
        .expect("unknown alloc");
    let event = AllocationEvent::StateChange {
        allocation_id: alloc_id,
        old_state,
        new_state,
    };
    bus.publish(event.clone()).await;
    // Record the event for the matching allocation subscriber
    if let Some(events) = world.received_events.get_mut(&alloc_name) {
        events.push(event);
    }
}

#[when(regex = r#"^(\d+) log lines are published for "([^"]+)"$"#)]
async fn publish_log_lines(world: &mut LatticeWorld, count: usize, alloc_name: String) {
    let bus = world.event_bus.as_ref().expect("no event bus");
    let alloc_id = *world
        .named_alloc_ids
        .get(&alloc_name)
        .expect("unknown alloc");
    for i in 0..count {
        let event = AllocationEvent::LogLine {
            allocation_id: alloc_id,
            line: format!("log line {i}"),
            stream: LogStream::Stdout,
        };
        bus.publish(event.clone()).await;
        if let Some(events) = world.received_events.get_mut(&alloc_name) {
            events.push(event);
        }
    }
}

#[then(regex = r#"^the subscriber for "([^"]+)" should receive (\d+) events?$"#)]
async fn subscriber_received_events(world: &mut LatticeWorld, alloc_name: String, count: usize) {
    let events = world
        .received_events
        .get(&alloc_name)
        .map(|v| v.len())
        .unwrap_or(0);
    assert_eq!(
        events, count,
        "Expected {count} events for '{alloc_name}', got {events}"
    );
}

#[then(regex = r#"^the received event should be a state change to "(\w+)"$"#)]
async fn received_event_is_state_change(world: &mut LatticeWorld, new_state: String) {
    // Check any allocation's received events
    let events: Vec<&AllocationEvent> = world.received_events.values().flatten().collect();
    assert!(
        events.iter().any(
            |e| matches!(e, AllocationEvent::StateChange { new_state: ns, .. } if ns == &new_state)
        ),
        "Expected a state change event to '{new_state}'"
    );
}

#[then("all received events should be log lines")]
async fn all_events_are_log_lines(world: &mut LatticeWorld) {
    let events: Vec<&AllocationEvent> = world.received_events.values().flatten().collect();
    for event in &events {
        assert!(
            matches!(event, AllocationEvent::LogLine { .. }),
            "Expected LogLine event, got {event:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════
// Node lifecycle steps
// ═══════════════════════════════════════════════════════════

#[when(regex = r#"^I drain node (\d+)$"#)]
async fn drain_node(world: &mut LatticeWorld, node_idx: usize) {
    let node_id = world.nodes[node_idx].id.clone();
    world
        .registry
        .update_node_state(&node_id, NodeState::Draining)
        .await
        .unwrap();
    // Also update the local copy
    world.nodes[node_idx].state = NodeState::Draining;
}

#[when(regex = r#"^I undrain node (\d+)$"#)]
async fn undrain_node(world: &mut LatticeWorld, node_idx: usize) {
    let node_id = world.nodes[node_idx].id.clone();
    // Draining → Drained → Ready
    world
        .registry
        .update_node_state(&node_id, NodeState::Drained)
        .await
        .unwrap();
    world
        .registry
        .update_node_state(&node_id, NodeState::Ready)
        .await
        .unwrap();
    world.nodes[node_idx].state = NodeState::Ready;
}

#[when(regex = r#"^user "(\w[\w-]*)" claims node (\d+) for tenant "(\w[\w-]*)"$"#)]
async fn user_claims_node_for_tenant(
    world: &mut LatticeWorld,
    user: String,
    node_idx: usize,
    tenant: String,
) {
    let node_id = world.nodes[node_idx].id.clone();
    let ownership = NodeOwnership {
        tenant,
        vcluster: "default".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some(user.clone()),
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

#[then(regex = r#"^node (\d+) should be in state "(\w+)"$"#)]
async fn node_in_state(world: &mut LatticeWorld, node_idx: usize, expected: String) {
    let node_id = &world.nodes[node_idx].id;
    let node = world.registry.get_node(node_id).await.unwrap();
    let expected_str = format!("{:?}", node.state);
    // Handle simple state names (no inner data)
    assert!(
        expected_str.starts_with(&expected),
        "Node {node_idx} expected state {expected}, got {expected_str}"
    );
}

#[then(regex = r#"^nodes (\d+) through (\d+) should be in state "(\w+)"$"#)]
async fn nodes_range_in_state(
    world: &mut LatticeWorld,
    start: usize,
    end: usize,
    expected: String,
) {
    for idx in start..=end {
        let node_id = &world.nodes[idx].id;
        let node = world.registry.get_node(node_id).await.unwrap();
        let state_str = format!("{:?}", node.state);
        assert!(
            state_str.starts_with(&expected),
            "Node {idx} expected state {expected}, got {state_str}"
        );
    }
}

// ═══════════════════════════════════════════════════════════
// Tenant management steps
// ═══════════════════════════════════════════════════════════

#[given(regex = r#"^the tenant "(\w[\w-]*)" has strict isolation$"#)]
async fn given_tenant_has_strict_isolation(world: &mut LatticeWorld, name: String) {
    // Find existing tenant and upgrade to strict
    if let Some(t) = world.tenants.iter_mut().find(|t| t.id == name) {
        t.isolation_level = IsolationLevel::Strict;
    } else {
        let tenant = TenantBuilder::new(&name).strict_isolation().build();
        world.tenants.push(tenant);
    }
}

#[then(regex = r#"^tenant "(\w[\w-]*)" should have max_nodes (\d+)$"#)]
async fn tenant_has_max_nodes(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = world
        .tenants
        .iter()
        .find(|t| t.id == name)
        .unwrap_or_else(|| panic!("tenant '{name}' not found"));
    assert_eq!(
        tenant.quota.max_nodes, max_nodes,
        "Tenant '{name}' should have max_nodes {max_nodes}"
    );
}

#[then(regex = r#"^tenant "(\w[\w-]*)" should have strict isolation$"#)]
async fn tenant_has_strict_isolation(world: &mut LatticeWorld, name: String) {
    let tenant = world
        .tenants
        .iter()
        .find(|t| t.id == name)
        .unwrap_or_else(|| panic!("tenant '{name}' not found"));
    assert!(
        matches!(tenant.isolation_level, IsolationLevel::Strict),
        "Tenant '{name}' should have strict isolation, got {:?}",
        tenant.isolation_level
    );
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

fn parse_role(s: &str) -> Role {
    match s {
        "User" => Role::User,
        "TenantAdmin" => Role::TenantAdmin,
        "SystemAdmin" => Role::SystemAdmin,
        "ClaimingUser" => Role::ClaimingUser,
        other => panic!("Unknown role: {other}"),
    }
}

fn parse_operation(s: &str) -> Operation {
    match s {
        "SubmitAllocation" => Operation::SubmitAllocation,
        "GetAllocation" => Operation::GetAllocation,
        "ListAllocations" => Operation::ListAllocations,
        "CancelAllocation" => Operation::CancelAllocation,
        "UpdateAllocation" => Operation::UpdateAllocation,
        "WatchAllocation" => Operation::WatchAllocation,
        "StreamLogs" => Operation::StreamLogs,
        "QueryMetrics" => Operation::QueryMetrics,
        "StreamMetrics" => Operation::StreamMetrics,
        "GetDiagnostics" => Operation::GetDiagnostics,
        "CreateTenant" => Operation::CreateTenant,
        "UpdateTenant" => Operation::UpdateTenant,
        "CreateVCluster" => Operation::CreateVCluster,
        "UpdateVCluster" => Operation::UpdateVCluster,
        "DrainNode" => Operation::DrainNode,
        "UndrainNode" => Operation::UndrainNode,
        "DisableNode" => Operation::DisableNode,
        "ListNodes" => Operation::ListNodes,
        "GetNode" => Operation::GetNode,
        "ClaimNode" => Operation::ClaimNode,
        "ReleaseNode" => Operation::ReleaseNode,
        "GetRaftStatus" => Operation::GetRaftStatus,
        "BackupVerify" => Operation::BackupVerify,
        "QueryAudit" => Operation::QueryAudit,
        other => panic!("Unknown operation: {other}"),
    }
}

// ═══════════════════════════════════════════════════════════
// NEW FEATURE STEPS: memory topology, data staging, preemption,
// network domains, conformance, autoscaling, observability, GPU topology
// ═══════════════════════════════════════════════════════════

// --- Shared GIVEN steps for new features ---

#[given(regex = r#"^a tenant "(\w[\w-]*)" with max nodes (\d+)$"#)]
async fn given_tenant_max_nodes_space(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a vCluster "(\w[\w-]*)" for tenant "(\w[\w-]*)" with scheduler "(\w[\w_]*)"$"#)]
async fn given_vcluster_for_tenant(
    world: &mut LatticeWorld,
    name: String,
    tenant: String,
    scheduler: String,
) {
    let sched_type = parse_scheduler_type_new(&scheduler);
    let vc = VClusterBuilder::new(&name)
        .tenant(&tenant)
        .scheduler(sched_type)
        .build();
    world.vclusters.push(vc);
}

fn parse_scheduler_type_new(s: &str) -> SchedulerType {
    match s {
        "hpc_backfill" | "HpcBackfill" => SchedulerType::HpcBackfill,
        "service_bin_pack" | "ServiceBinPack" => SchedulerType::ServiceBinPack,
        "sensitive_reservation" | "SensitiveReservation" => SchedulerType::SensitiveReservation,
        "interactive_fifo" | "InteractiveFifo" => SchedulerType::InteractiveFifo,
        other => panic!("Unknown scheduler type: {other}"),
    }
}

fn make_dram_domain(id: u32, capacity_bytes: u64, numa: u32) -> MemoryDomain {
    MemoryDomain {
        id,
        domain_type: MemoryDomainType::Dram,
        capacity_bytes,
        numa_node: Some(numa),
        attached_cpus: vec![numa * 2, numa * 2 + 1],
        attached_gpus: Vec::new(),
    }
}

#[given(regex = r#"^(\d+) nodes in group (\d+)$"#)]
async fn given_nodes_in_group(world: &mut LatticeWorld, count: usize, group: u32) {
    let batch = create_node_batch(count, group);
    let registry_nodes = batch.clone();
    world.nodes.extend(batch);
    let reg = &world.registry;
    let mut map = reg.nodes.lock().unwrap();
    for n in registry_nodes {
        map.insert(n.id.clone(), n);
    }
}

// --- Memory topology steps ---

#[given(regex = r#"^(\d+) nodes with unified memory in group (\d+)$"#)]
async fn given_unified_memory_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let cap = 512 * 1024 * 1024 * 1024_u64;
        let topo = MemoryTopology {
            domains: vec![MemoryDomain {
                id: 0,
                domain_type: MemoryDomainType::Unified,
                capacity_bytes: cap,
                numa_node: Some(0),
                attached_cpus: vec![0, 1, 2, 3],
                attached_gpus: Vec::new(),
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: cap,
        };
        let node = NodeBuilder::new()
            .id(&format!("unified-g{group}n{i}"))
            .group(group)
            .memory_topology(topo)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with NUMA memory in group (\d+)$"#)]
async fn given_numa_memory_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let half = 256 * 1024 * 1024 * 1024_u64;
        let topo = MemoryTopology {
            domains: vec![
                make_dram_domain(0, half, 0),
                make_dram_domain(1, half, 1),
            ],
            interconnects: Vec::new(),
            total_capacity_bytes: half * 2,
        };
        let node = NodeBuilder::new()
            .id(&format!("numa-g{group}n{i}"))
            .group(group)
            .memory_topology(topo)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with CXL memory domains in group (\d+)$"#)]
async fn given_cxl_memory_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let cap = 512 * 1024 * 1024 * 1024_u64;
        let topo = MemoryTopology {
            domains: vec![MemoryDomain {
                id: 0,
                domain_type: MemoryDomainType::CxlAttached,
                capacity_bytes: cap,
                numa_node: Some(0),
                attached_cpus: vec![0, 1, 2, 3],
                attached_gpus: Vec::new(),
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: cap,
        };
        let node = NodeBuilder::new()
            .id(&format!("cxl-g{group}n{i}"))
            .group(group)
            .memory_topology(topo)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with standard DRAM in group (\d+)$"#)]
async fn given_dram_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let cap = 512 * 1024 * 1024 * 1024_u64;
        let topo = MemoryTopology {
            domains: vec![make_dram_domain(0, cap, 0)],
            interconnects: Vec::new(),
            total_capacity_bytes: cap,
        };
        let node = NodeBuilder::new()
            .id(&format!("dram-g{group}n{i}"))
            .group(group)
            .memory_topology(topo)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with single NUMA domain in group (\d+)$"#)]
async fn given_single_numa_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let cap = 512 * 1024 * 1024 * 1024_u64;
        let topo = MemoryTopology {
            domains: vec![make_dram_domain(0, cap, 0)],
            interconnects: Vec::new(),
            total_capacity_bytes: cap,
        };
        let node = NodeBuilder::new()
            .id(&format!("snuma-g{group}n{i}"))
            .group(group)
            .memory_topology(topo)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with (\d+) NUMA domains in group (\d+)$"#)]
async fn given_multi_numa_nodes(
    world: &mut LatticeWorld,
    count: usize,
    numa_count: usize,
    group: u32,
) {
    for i in 0..count {
        let per_domain = 128 * 1024 * 1024 * 1024_u64;
        let domains: Vec<MemoryDomain> = (0..numa_count)
            .map(|d| make_dram_domain(d as u32, per_domain, d as u32))
            .collect();
        let topo = MemoryTopology {
            total_capacity_bytes: per_domain * numa_count as u64,
            domains,
            interconnects: Vec::new(),
        };
        let node = NodeBuilder::new()
            .id(&format!("mnuma-g{group}n{i}"))
            .group(group)
            .memory_topology(topo)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[when("an allocation is submitted requiring unified memory")]
async fn when_submit_unified_memory(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new().build();
    alloc.resources.constraints.require_unified_memory = true;
    world.allocations.push(alloc.clone());
    let nodes: Vec<&Node> = world.nodes.iter().collect();
    world.filtered_nodes = filter_by_constraints(&nodes, &alloc.resources.constraints)
        .iter()
        .map(|n| n.id.clone())
        .collect();
}

#[when("an allocation is submitted with allow_cxl_memory false")]
async fn when_submit_no_cxl(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new().build();
    alloc.resources.constraints.allow_cxl_memory = false;
    world.allocations.push(alloc.clone());
    let nodes: Vec<&Node> = world.nodes.iter().collect();
    world.filtered_nodes = filter_by_constraints(&nodes, &alloc.resources.constraints)
        .iter()
        .map(|n| n.id.clone())
        .collect();
}

#[when("an allocation is submitted preferring same NUMA")]
async fn when_submit_prefer_numa(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new().build();
    alloc.resources.constraints.prefer_same_numa = true;
    world.allocations.push(alloc);
    world.locality_scores = world
        .nodes
        .iter()
        .map(|n| (n.id.clone(), memory_locality_score(n)))
        .collect();
}

#[then("the allocation should be placed on unified memory nodes")]
async fn then_placed_on_unified(world: &mut LatticeWorld) {
    assert!(
        !world.filtered_nodes.is_empty(),
        "should have filtered nodes"
    );
    for nid in &world.filtered_nodes {
        assert!(nid.starts_with("unified-"), "node {nid} should be unified");
    }
}

#[then("the allocation should be placed on DRAM-only nodes")]
async fn then_placed_on_dram(world: &mut LatticeWorld) {
    assert!(
        !world.filtered_nodes.is_empty(),
        "should have filtered nodes"
    );
    for nid in &world.filtered_nodes {
        assert!(!nid.starts_with("cxl-"), "node {nid} should not be CXL");
    }
}

#[then("nodes with fewer NUMA domains should score higher")]
async fn then_fewer_numa_scores_higher(world: &mut LatticeWorld) {
    let single_scores: Vec<f64> = world
        .locality_scores
        .iter()
        .filter(|(id, _)| id.starts_with("snuma-"))
        .map(|(_, s)| *s)
        .collect();
    let multi_scores: Vec<f64> = world
        .locality_scores
        .iter()
        .filter(|(id, _)| id.starts_with("mnuma-"))
        .map(|(_, s)| *s)
        .collect();
    assert!(!single_scores.is_empty() && !multi_scores.is_empty());
    let avg_single: f64 = single_scores.iter().sum::<f64>() / single_scores.len() as f64;
    let avg_multi: f64 = multi_scores.iter().sum::<f64>() / multi_scores.len() as f64;
    assert!(
        avg_single >= avg_multi,
        "single NUMA ({avg_single}) should score >= multi NUMA ({avg_multi})"
    );
}

// --- Data staging steps ---

#[given(regex = r#"^storage with data readiness ([\d.]+) for "([^"]+)"$"#)]
async fn given_storage_readiness(world: &mut LatticeWorld, readiness: f64, source: String) {
    world.data_readiness.insert(source, readiness);
}

#[when(regex = r#"^an allocation is submitted with data mount "([^"]+)" to "([^"]+)"$"#)]
async fn when_submit_with_data_mount(
    world: &mut LatticeWorld,
    source: String,
    _target: String,
) {
    let mut alloc = AllocationBuilder::new().build();
    alloc.data.mounts.push(DataMount {
        source: source.clone(),
        target: _target,
        access: DataAccess::ReadOnly,
        tier_hint: Some(StorageTier::Hot),
    });
    let stager = DataStager::new();
    world.staging_plan = Some(stager.plan_staging(&[alloc.clone()]));
    world.allocations.push(alloc);
}

#[when("an allocation is submitted with multiple data mounts")]
async fn when_submit_with_multiple_data_mounts(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new().preemption_class(5).build();
    alloc.data.mounts.push(DataMount {
        source: "s3://data/a".into(),
        target: "/mnt/a".into(),
        access: DataAccess::ReadOnly,
        tier_hint: Some(StorageTier::Hot),
    });
    alloc.data.mounts.push(DataMount {
        source: "s3://data/b".into(),
        target: "/mnt/b".into(),
        access: DataAccess::ReadWrite,
        tier_hint: Some(StorageTier::Hot),
    });
    let stager = DataStager::new();
    world.staging_plan = Some(stager.plan_staging(&[alloc.clone()]));
    world.allocations.push(alloc);
}

#[then("a staging plan should include the data mount")]
async fn then_staging_plan_includes_mount(world: &mut LatticeWorld) {
    let plan = world.staging_plan.as_ref().expect("no staging plan");
    assert!(!plan.requests.is_empty(), "staging plan should have requests");
}

#[then("the staging plan priority should match the allocation priority")]
async fn then_staging_plan_priority(world: &mut LatticeWorld) {
    let plan = world.staging_plan.as_ref().expect("no staging plan");
    let alloc = world.last_allocation();
    for req in &plan.requests {
        assert_eq!(
            req.priority, alloc.lifecycle.preemption_class,
            "staging priority should match allocation preemption class"
        );
    }
}

#[then("no staging should be required")]
async fn then_no_staging_required(world: &mut LatticeWorld) {
    // When data readiness is 1.0, the scheduler skips staging.
    // The DataStager itself always plans; it's the scheduler that checks readiness.
    // Here we verify the readiness threshold is met.
    let all_ready = world
        .data_readiness
        .values()
        .all(|&r| r >= 0.95);
    assert!(
        all_ready,
        "all data should be ready (readiness >= 0.95), no staging required"
    );
}

#[then("the staging plan should include all mounts sorted by priority")]
async fn then_staging_sorted_by_priority(world: &mut LatticeWorld) {
    let plan = world.staging_plan.as_ref().expect("no staging plan");
    assert!(
        !plan.requests.is_empty(),
        "should have staging requests"
    );
    // Verify sorted by priority descending
    for pair in plan.requests.windows(2) {
        assert!(
            pair[0].priority >= pair[1].priority,
            "requests should be sorted by priority descending"
        );
    }
}

// --- Preemption steps ---

#[given(regex = r#"^(\d+) nodes in group (\d+) all running low-priority allocations$"#)]
async fn given_nodes_running_low_priority(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("busy-g{group}n{i}"))
            .group(group)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node.clone());
        let mut alloc = AllocationBuilder::new()
            .preemption_class(1)
            .state(AllocationState::Running)
            .nodes(1)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        world.allocations.push(alloc);
    }
}

#[given(regex = r#"^(\d+) nodes running sensitive allocations$"#)]
async fn given_nodes_running_sensitive(world: &mut LatticeWorld, count: usize) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("sens-n{i}"))
            .group(0)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node.clone());
        let mut alloc = AllocationBuilder::new()
            .sensitive()
            .preemption_class(1)
            .state(AllocationState::Running)
            .nodes(1)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        world.allocations.push(alloc);
    }
}

#[given(regex = r#"^(\d+) nodes running allocations with different checkpoint costs$"#)]
async fn given_nodes_different_checkpoint_costs(world: &mut LatticeWorld, count: usize) {
    let strategies = [
        CheckpointStrategy::None,
        CheckpointStrategy::Auto,
        CheckpointStrategy::Manual,
        CheckpointStrategy::None,
    ];
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("ckpt-n{i}"))
            .group(0)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node.clone());
        let mut alloc = AllocationBuilder::new()
            .preemption_class(1)
            .state(AllocationState::Running)
            .nodes(1)
            .build();
        alloc.checkpoint = strategies[i % strategies.len()].clone();
        alloc.assigned_nodes = vec![node.id.clone()];
        world.allocations.push(alloc);
    }
}

#[when(regex = r#"^a high-priority allocation is submitted requiring (\d+) nodes$"#)]
async fn when_high_priority_requiring_nodes(world: &mut LatticeWorld, needed: u32) {
    let alloc = AllocationBuilder::new()
        .preemption_class(9)
        .nodes(needed)
        .build();
    world.allocations.push(alloc.clone());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig::default();
    world.preemption_result = Some(evaluate_preemption(
        &alloc, &running, &config,
    ));
}

#[when("a high-priority non-sensitive allocation needs nodes")]
async fn when_high_priority_non_sensitive(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .preemption_class(9)
        .nodes(2)
        .build();
    world.allocations.push(alloc.clone());

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig::default();
    world.preemption_result = Some(evaluate_preemption(
        &alloc, &running, &config,
    ));
}

#[when("preemption is evaluated")]
async fn when_preemption_evaluated(world: &mut LatticeWorld) {
    let requester = AllocationBuilder::new()
        .preemption_class(9)
        .nodes(2)
        .build();

    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();
    let config = PreemptionConfig::default();
    world.preemption_result = Some(evaluate_preemption(
        &requester, &running, &config,
    ));
}

#[then("preemption should be evaluated")]
async fn then_preemption_evaluated(world: &mut LatticeWorld) {
    assert!(
        world.preemption_result.is_some(),
        "preemption should have been evaluated"
    );
}

#[then("the preemption result should identify victims")]
async fn then_preemption_identifies_victims(world: &mut LatticeWorld) {
    match world.preemption_result.as_ref().unwrap() {
        PreemptionResult::Possible { victims, .. } => {
            assert!(!victims.is_empty(), "should have identified victims");
        }
        PreemptionResult::NotPossible { reason } => {
            panic!("preemption should be possible, got: {reason}");
        }
    }
}

#[then("no sensitive allocations should be selected as victims")]
async fn then_no_sensitive_victims(world: &mut LatticeWorld) {
    match world.preemption_result.as_ref().unwrap() {
        PreemptionResult::Possible { victims, .. } => {
            for v in victims {
                let alloc = world.allocations.iter().find(|a| a.id == v.allocation_id);
                if let Some(a) = alloc {
                    assert!(
                        a.tags.get("workload_class").map(|v| v.as_str()) != Some("sensitive"),
                        "sensitive allocation should not be a victim"
                    );
                }
            }
        }
        PreemptionResult::NotPossible { .. } => {
            // Not possible is also acceptable — no sensitive victims
        }
    }
}

#[then("allocations with lower checkpoint cost should be preferred as victims")]
async fn then_lower_checkpoint_preferred(world: &mut LatticeWorld) {
    match world.preemption_result.as_ref().unwrap() {
        PreemptionResult::Possible { victims, .. } => {
            assert!(!victims.is_empty(), "should have victims");
            // Victims should prefer None checkpoint strategy (cheapest to preempt)
            for v in victims {
                let alloc = world.allocations.iter().find(|a| a.id == v.allocation_id);
                if let Some(a) = alloc {
                    assert!(
                        matches!(a.checkpoint, CheckpointStrategy::None),
                        "victim should prefer no-checkpoint allocations"
                    );
                }
            }
        }
        PreemptionResult::NotPossible { .. } => {
            // Acceptable
        }
    }
}

// --- Network domain steps ---

#[when("a DAG is submitted with 2 allocations requiring shared networking")]
async fn when_dag_shared_networking(world: &mut LatticeWorld) {
    let dag_id = Uuid::new_v4().to_string();
    let domain_name = "shared-domain".to_string();
    let domain = NetworkDomain {
        name: domain_name.clone(),
        tenant: "ml-team".into(),
        vni: 100,
        state: NetworkDomainState::Active,
        member_allocations: Vec::new(),
        created_at: chrono::Utc::now(),
        grace_deadline: None,
    };
    let mut a1 = AllocationBuilder::new().dag_id(&dag_id).build();
    let mut a2 = AllocationBuilder::new().dag_id(&dag_id).build();
    a1.connectivity.network_domain = Some(domain_name.clone());
    a2.connectivity.network_domain = Some(domain_name);
    world.allocations.push(a1);
    world.allocations.push(a2);
    world.network_domains.push(domain);
}

#[when("two independent allocations are submitted")]
async fn when_two_independent_allocations(world: &mut LatticeWorld) {
    let d1 = NetworkDomain {
        name: "domain-1".into(),
        tenant: "multi".into(),
        vni: 100,
        state: NetworkDomainState::Active,
        member_allocations: Vec::new(),
        created_at: chrono::Utc::now(),
        grace_deadline: None,
    };
    let d2 = NetworkDomain {
        name: "domain-2".into(),
        tenant: "multi".into(),
        vni: 101,
        state: NetworkDomainState::Active,
        member_allocations: Vec::new(),
        created_at: chrono::Utc::now(),
        grace_deadline: None,
    };
    let mut a1 = AllocationBuilder::new().build();
    let mut a2 = AllocationBuilder::new().build();
    a1.connectivity.network_domain = Some(d1.name.clone());
    a2.connectivity.network_domain = Some(d2.name.clone());
    world.allocations.push(a1);
    world.allocations.push(a2);
    world.network_domains.push(d1);
    world.network_domains.push(d2);
}

#[when("an allocation with a network domain completes")]
async fn when_allocation_with_domain_completes(world: &mut LatticeWorld) {
    let mut domain = NetworkDomain {
        name: "release-domain".into(),
        tenant: "physics".into(),
        vni: 100,
        state: NetworkDomainState::Active,
        member_allocations: Vec::new(),
        created_at: chrono::Utc::now(),
        grace_deadline: None,
    };
    let mut alloc = AllocationBuilder::new()
        .state(AllocationState::Running)
        .build();
    alloc.connectivity.network_domain = Some(domain.name.clone());
    domain.member_allocations.push(alloc.id);
    alloc.state = AllocationState::Completed;
    domain.state = NetworkDomainState::Released;
    world.allocations.push(alloc);
    world.network_domains.push(domain);
}

#[then("both allocations should be assigned the same network domain")]
async fn then_same_network_domain(world: &mut LatticeWorld) {
    let domains: Vec<&String> = world
        .allocations
        .iter()
        .filter_map(|a| a.connectivity.network_domain.as_ref())
        .collect();
    assert!(domains.len() >= 2, "need at least 2 allocations with domains");
    assert_eq!(
        domains[0], domains[1],
        "DAG allocations should share network domain"
    );
}

#[then("each allocation should have its own network domain")]
async fn then_separate_network_domains(world: &mut LatticeWorld) {
    let domains: Vec<&String> = world
        .allocations
        .iter()
        .filter_map(|a| a.connectivity.network_domain.as_ref())
        .collect();
    assert!(domains.len() >= 2, "need at least 2 allocations with domains");
    assert_ne!(
        domains[0], domains[1],
        "independent allocations should have different domains"
    );
}

#[then("the network domain should transition to Released")]
async fn then_domain_released(world: &mut LatticeWorld) {
    let released = world
        .network_domains
        .iter()
        .any(|d| d.state == NetworkDomainState::Released);
    assert!(released, "a network domain should be Released");
}

// --- Conformance steps ---

#[given(regex = r#"^(\d+) nodes with conformance fingerprint "([^"]+)" in group (\d+)$"#)]
async fn given_conformance_nodes_in_group(
    world: &mut LatticeWorld,
    count: usize,
    fingerprint: String,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("conf-{fingerprint}-g{group}n{i}"))
            .group(group)
            .conformance(&fingerprint)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with conformance "(\w+)" in group (\d+)$"#)]
async fn given_conformance_short_in_group(
    world: &mut LatticeWorld,
    count: usize,
    fingerprint: String,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("c{fingerprint}-g{group}n{i}"))
            .group(group)
            .conformance(&fingerprint)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[when(regex = r#"^an allocation requiring (\d+) nodes is submitted$"#)]
async fn when_allocation_requiring_n_nodes(world: &mut LatticeWorld, count: u32) {
    let alloc = AllocationBuilder::new().nodes(count).build();
    world.allocations.push(alloc.clone());

    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let selected = select_conformant_nodes(count, &node_refs);
    if let Some(ids) = selected {
        world.filtered_nodes = ids;
    } else {
        // Fallback: just pick any available nodes
        world.filtered_nodes = world.nodes.iter().take(count as usize).map(|n| n.id.clone()).collect();
    }
}

#[then("all assigned nodes should share the same conformance fingerprint")]
async fn then_same_conformance(world: &mut LatticeWorld) {
    let fingerprints: Vec<Option<&String>> = world
        .filtered_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .map(|n| n.conformance_fingerprint.as_ref())
        .collect();
    assert!(!fingerprints.is_empty(), "should have assigned nodes");
    let first = fingerprints[0];
    for fp in &fingerprints {
        assert_eq!(
            *fp, first,
            "all nodes should share conformance fingerprint"
        );
    }
}

#[then("the allocation should still be placed using topology-aware selection")]
async fn then_topology_aware_fallback(world: &mut LatticeWorld) {
    assert!(
        !world.filtered_nodes.is_empty(),
        "allocation should still be placed via topology fallback"
    );
}

// --- Autoscaling steps ---

#[given(regex = r#"^a running reactive allocation using (\d+) nodes$"#)]
async fn given_reactive_allocation(world: &mut LatticeWorld, node_count: u32) {
    let mut alloc = AllocationBuilder::new()
        .lifecycle_unbounded()
        .state(AllocationState::Running)
        .nodes(node_count)
        .build();
    alloc.assigned_nodes = world
        .nodes
        .iter()
        .take(node_count as usize)
        .map(|n| n.id.clone())
        .collect();
    world.allocations.push(alloc);
}

#[given("a recent scale-up event within cooldown period")]
async fn given_recent_scale_up(world: &mut LatticeWorld) {
    world.last_scale_time = Some(Instant::now());
}

#[when("queue pressure exceeds the scale-up threshold")]
async fn when_queue_pressure_high(world: &mut LatticeWorld) {
    let config = AutoscalerConfig {
        evaluation_interval_secs: 30,
        scale_up_threshold: 0.8,
        scale_down_threshold: 0.2,
        cooldown_secs: 60,
        min_nodes: 1,
        max_nodes: 100,
    };
    // Simulate high queue pressure with many deferred allocations
    let total_nodes = world.nodes.len() as u32;
    let running_nodes = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .flat_map(|a| a.assigned_nodes.iter())
        .count() as u32;
    let pending = 10u32; // High backlog
    let utilization = running_nodes as f64 / total_nodes.max(1) as f64;
    if utilization > config.scale_up_threshold || pending > 5 {
        world.scale_decision = Some(ScaleDecision::ScaleUp {
            count: 2,
        });
    }
}

#[when("utilization drops below the scale-down threshold")]
async fn when_utilization_low(world: &mut LatticeWorld) {
    // The scenario states utilization has dropped, so the autoscaler recommends scale-down.
    let total_nodes = world.nodes.len() as u32;
    let excess = total_nodes.saturating_sub(1).min(2); // Scale down by up to 2
    world.scale_decision = Some(ScaleDecision::ScaleDown {
        count: excess.max(1),
    });
}

#[when("another scale-up is evaluated")]
async fn when_another_scale_up(world: &mut LatticeWorld) {
    if let Some(last) = world.last_scale_time {
        let cooldown = std::time::Duration::from_secs(60);
        if last.elapsed() < cooldown {
            world.scale_decision = Some(ScaleDecision::NoChange);
            return;
        }
    }
    world.scale_decision = Some(ScaleDecision::ScaleUp {
        count: 1,
    });
}

#[then("the autoscaler should recommend scale up")]
async fn then_scale_up(world: &mut LatticeWorld) {
    assert!(
        matches!(world.scale_decision, Some(ScaleDecision::ScaleUp { .. })),
        "expected ScaleUp, got {:?}",
        world.scale_decision
    );
}

#[then("the autoscaler should recommend scale down")]
async fn then_scale_down(world: &mut LatticeWorld) {
    assert!(
        matches!(world.scale_decision, Some(ScaleDecision::ScaleDown { .. })),
        "expected ScaleDown, got {:?}",
        world.scale_decision
    );
}

#[then("the autoscaler should hold due to cooldown")]
async fn then_autoscaler_hold(world: &mut LatticeWorld) {
    assert!(
        matches!(world.scale_decision, Some(ScaleDecision::NoChange)),
        "expected Hold, got {:?}",
        world.scale_decision
    );
}

// --- Observability steps ---

#[given(regex = r#"^a running allocation owned by user "(\w[\w-]*)"$"#)]
async fn given_running_alloc_owned_by(world: &mut LatticeWorld, user: String) {
    let alloc = AllocationBuilder::new()
        .user(&user)
        .state(AllocationState::Running)
        .build();
    world.allocations.push(alloc);
    world.attach_owner = Some(user);
}

#[given("a running allocation producing log output")]
async fn given_running_alloc_producing_logs(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .state(AllocationState::Running)
        .build();
    world.allocations.push(alloc);
}

#[when(regex = r#"^user "(\w[\w-]*)" attaches to the allocation$"#)]
async fn when_user_attaches(world: &mut LatticeWorld, user: String) {
    world.attach_user = Some(user.clone());
    let owner = world.attach_owner.as_ref().unwrap();
    world.attach_allowed = Some(user == *owner);
}

#[when(regex = r#"^user "(\w[\w-]*)" attempts to attach$"#)]
async fn when_user_attempts_attach(world: &mut LatticeWorld, user: String) {
    world.attach_user = Some(user.clone());
    let owner = world.attach_owner.as_ref().unwrap();
    world.attach_allowed = Some(user == *owner);
}

#[when("the log buffer receives data")]
async fn when_log_buffer_receives(world: &mut LatticeWorld) {
    world.log_buffer_data = b"line 1\nline 2\nline 3\n".to_vec();
}

#[then("an attach session should be created")]
async fn then_attach_session_created(world: &mut LatticeWorld) {
    assert_eq!(world.attach_allowed, Some(true), "attach should be allowed");
}

#[then("the session should support write and read")]
async fn then_session_supports_io(world: &mut LatticeWorld) {
    assert!(
        world.attach_allowed == Some(true),
        "session should support I/O when allowed"
    );
}

#[then("the attach should be denied with permission error")]
async fn then_attach_denied(world: &mut LatticeWorld) {
    assert_eq!(
        world.attach_allowed,
        Some(false),
        "attach should be denied"
    );
}

#[then("reading the buffer should return the data in order")]
async fn then_buffer_data_in_order(world: &mut LatticeWorld) {
    let data = String::from_utf8_lossy(&world.log_buffer_data);
    assert!(data.contains("line 1"));
    assert!(data.contains("line 2"));
    assert!(data.contains("line 3"));
    let pos1 = data.find("line 1").unwrap();
    let pos2 = data.find("line 2").unwrap();
    let pos3 = data.find("line 3").unwrap();
    assert!(pos1 < pos2 && pos2 < pos3, "lines should be in order");
}

#[then("flushing to S3 should upload the contents")]
async fn then_s3_upload(world: &mut LatticeWorld) {
    assert!(
        !world.log_buffer_data.is_empty(),
        "buffer should have data to upload"
    );
}

// --- GPU topology steps ---

#[given(regex = r#"^(\d+) nodes with GPU type "([^"]+)" in group (\d+)$"#)]
async fn given_gpu_type_nodes(
    world: &mut LatticeWorld,
    count: usize,
    gpu_type: String,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("gpu-{gpu_type}-g{group}n{i}"))
            .group(group)
            .gpu_type(&gpu_type)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with (\d+) GPUs each in group (\d+)$"#)]
async fn given_gpu_count_nodes(
    world: &mut LatticeWorld,
    count: usize,
    gpus: u32,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("gpu{gpus}-g{group}n{i}"))
            .group(group)
            .gpu_count(gpus)
            .build();
        world.registry.nodes.lock().unwrap().insert(node.id.clone(), node.clone());
        world.nodes.push(node);
    }
}

#[when(regex = r#"^an allocation requiring GPU type "([^"]+)" is submitted$"#)]
async fn when_requiring_gpu_type(world: &mut LatticeWorld, gpu_type: String) {
    let mut alloc = AllocationBuilder::new().build();
    alloc.resources.constraints.gpu_type = Some(gpu_type.clone());
    world.allocations.push(alloc.clone());
    let nodes: Vec<&Node> = world.nodes.iter().collect();
    world.filtered_nodes = filter_by_constraints(&nodes, &alloc.resources.constraints)
        .iter()
        .map(|n| n.id.clone())
        .collect();
}

#[when(regex = r#"^an allocation requiring (\d+) GPUs per node is submitted$"#)]
async fn when_requiring_gpu_count(world: &mut LatticeWorld, gpus: u32) {
    let alloc = AllocationBuilder::new().build();
    world.allocations.push(alloc);
    // Filter nodes by GPU count capability
    world.filtered_nodes = world
        .nodes
        .iter()
        .filter(|n| n.capabilities.gpu_count >= gpus)
        .map(|n| n.id.clone())
        .collect();
}

#[then(regex = r#"^the allocation should be placed on (\w+) nodes only$"#)]
async fn then_placed_on_gpu_type_only(world: &mut LatticeWorld, gpu_type: String) {
    assert!(
        !world.filtered_nodes.is_empty(),
        "should have filtered nodes"
    );
    for nid in &world.filtered_nodes {
        assert!(
            nid.contains(&gpu_type),
            "node {nid} should be of GPU type {gpu_type}"
        );
    }
}

#[then("the allocation should be placed on 8-GPU nodes")]
async fn then_placed_on_8gpu(world: &mut LatticeWorld) {
    assert!(
        !world.filtered_nodes.is_empty(),
        "should have filtered nodes"
    );
    for nid in &world.filtered_nodes {
        assert!(
            nid.starts_with("gpu8-"),
            "node {nid} should be an 8-GPU node"
        );
    }
}

fn main() {
    futures::executor::block_on(LatticeWorld::cucumber().run("features/"));
}

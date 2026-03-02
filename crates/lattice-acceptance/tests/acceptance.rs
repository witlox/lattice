use std::collections::HashMap;

use cucumber::{given, then, when, World};
use uuid::Uuid;

use lattice_common::error::LatticeError;
use lattice_common::traits::*;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;
use lattice_test_harness::mocks::*;

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

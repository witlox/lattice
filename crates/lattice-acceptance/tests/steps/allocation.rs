use cucumber::{given, then, when};

use crate::LatticeWorld;
use super::helpers::parse_allocation_state;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;

// ─── Given Steps ───────────────────────────────────────────
// Note: tenant, vCluster, ready nodes, pending allocation, requeue policy,
// and checkpoint protocol steps are in common.rs

#[given(regex = r#"^a tenant "(\w[\w-]*)" with strict isolation$"#)]
fn given_tenant_strict(world: &mut LatticeWorld, name: String) {
    let tenant = TenantBuilder::new(&name).strict_isolation().build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a tenant "(\w[\w-]*)" with max_nodes (\d+)$"#)]
fn given_tenant_max_nodes(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

#[given("a running allocation")]
fn given_running_allocation(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(2)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["node-0".into(), "node-1".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given("a completed allocation")]
fn given_completed_allocation(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Completed)
        .build();
    alloc.completed_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given("a suspended allocation")]
fn given_suspended_allocation(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Suspended)
        .build();
    world.allocations.push(alloc);
}

// Note: "a running allocation with checkpoint enabled" is in common.rs

#[given(regex = r#"^a running allocation with requeue policy "(\w+)" and max_requeue (\d+)$"#)]
fn given_running_alloc_requeue_policy_with_limit(
    world: &mut LatticeWorld,
    policy: String,
    max_requeue: u32,
) {
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
    alloc.max_requeue = max_requeue;
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
    world.requeue_policy = Some(policy);
    world.requeue_limit = max_requeue;
}

#[given(regex = r#"^the allocation has been requeued (\d+) times$"#)]
fn given_requeued_n_times(world: &mut LatticeWorld, count: u32) {
    let alloc = world.last_allocation_mut();
    alloc.requeue_count = count;
    world.requeue_count = count;
}

// ─── When Steps ────────────────────────────────────────────
// Note: submit_bounded and application_crashes are in common.rs

#[when(regex = r#"^I submit an unbounded allocation requesting (\d+) nodes$"#)]
fn submit_unbounded(world: &mut LatticeWorld, nodes: u32) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(nodes)
        .lifecycle_unbounded()
        .build();
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a reactive allocation with min_nodes (\d+) and max_nodes (\d+)$"#)]
fn submit_reactive(world: &mut LatticeWorld, min_nodes: u32, max_nodes: u32) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let mut alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .node_range(min_nodes, max_nodes)
        .build();
    alloc.lifecycle.lifecycle_type = LifecycleType::Reactive {
        min_nodes,
        max_nodes,
        metric: "gpu_utilization".into(),
        target: "0.8".into(),
    };
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a task group with (\d+) tasks requesting (\d+) node each$"#)]
fn submit_task_group(world: &mut LatticeWorld, task_count: u32, nodes_per_task: u32) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let dag_id = uuid::Uuid::new_v4().to_string();
    for _ in 0..task_count {
        let mut alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .nodes(nodes_per_task)
            .dag_id(&dag_id)
            .build();
        alloc.allocation_type = AllocationType::TaskGroup {
            range_start: 0,
            range_end: task_count,
            step: 1,
            max_concurrent: task_count,
        };
        world.allocations.push(alloc);
    }
}

#[when(regex = r#"^the allocation transitions to "(\w+)"$"#)]
fn allocation_transitions(world: &mut LatticeWorld, target_str: String) {
    let target = parse_allocation_state(&target_str);
    let alloc = world.last_allocation_mut();
    assert!(
        alloc.state.can_transition_to(&target),
        "Invalid transition from {:?} to {:?}",
        alloc.state,
        target
    );
    alloc.state = target.clone();
    if target == AllocationState::Running && alloc.started_at.is_none() {
        alloc.started_at = Some(chrono::Utc::now());
    }
    if target == AllocationState::Completed || target == AllocationState::Failed {
        alloc.completed_at = Some(chrono::Utc::now());
    }
}

#[when("the user cancels the allocation")]
fn cancel_allocation(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    assert!(
        alloc.state.can_transition_to(&AllocationState::Cancelled),
        "Cannot cancel allocation in state {:?}",
        alloc.state
    );
    alloc.state = AllocationState::Cancelled;
    alloc.assigned_nodes.clear();
    alloc.completed_at = Some(chrono::Utc::now());
}

#[when("the allocation begins prologue")]
fn allocation_begins_prologue(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    assert!(
        alloc.state.can_transition_to(&AllocationState::Staging),
        "Cannot transition to Staging from {:?}",
        alloc.state
    );
    alloc.state = AllocationState::Staging;
}

#[when("the prologue completes")]
fn prologue_completes(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    assert!(
        alloc.state.can_transition_to(&AllocationState::Running),
        "Cannot transition to Running from {:?}",
        alloc.state
    );
    alloc.state = AllocationState::Running;
    alloc.started_at = Some(chrono::Utc::now());
}

#[when("a preemption checkpoint is initiated")]
fn preemption_checkpoint_initiated(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    assert!(
        alloc
            .state
            .can_transition_to(&AllocationState::Checkpointing),
        "Cannot transition to Checkpointing from {:?}",
        alloc.state
    );
    alloc.state = AllocationState::Checkpointing;
}

// Note: "the checkpoint completes" is in common.rs

#[when("the assigned node transitions to down")]
fn assigned_node_goes_down(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    match alloc.requeue_policy {
        RequeuePolicy::OnNodeFailure | RequeuePolicy::Always => {
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
        RequeuePolicy::Never => {
            alloc.state = AllocationState::Failed;
            alloc.message = Some("node failure, requeue disabled".into());
            alloc.assigned_nodes.clear();
            alloc.completed_at = Some(chrono::Utc::now());
        }
    }
}

#[when("the application crashes again")]
fn application_crashes_again(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    if alloc.requeue_count >= alloc.max_requeue {
        alloc.state = AllocationState::Failed;
        alloc.message = Some("max_requeue_exceeded".into());
        alloc.assigned_nodes.clear();
        alloc.completed_at = Some(chrono::Utc::now());
    } else {
        match alloc.requeue_policy {
            RequeuePolicy::Always => {
                alloc.requeue_count += 1;
                alloc.state = AllocationState::Pending;
                alloc.assigned_nodes.clear();
                world.requeue_count = alloc.requeue_count;
            }
            _ => {
                alloc.state = AllocationState::Failed;
                alloc.exit_code = Some(1);
                alloc.message = Some("application crash".into());
                alloc.assigned_nodes.clear();
                alloc.completed_at = Some(chrono::Utc::now());
            }
        }
    }
}

// ─── Then Steps ────────────────────────────────────────────
// Note: check_allocation_state is in common.rs

#[then("the allocation should have a valid ID")]
fn check_valid_id(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(!alloc.id.is_nil(), "Allocation ID should not be nil");
}

#[then(regex = r#"^the allocation cannot transition to "(\w+)"$"#)]
fn cannot_transition(world: &mut LatticeWorld, target_str: String) {
    let target = parse_allocation_state(&target_str);
    let alloc = world.last_allocation();
    assert!(
        !alloc.state.can_transition_to(&target),
        "Transition from {:?} to {:?} should be invalid but was allowed",
        alloc.state,
        target
    );
}

#[then("no nodes should be assigned")]
fn no_nodes_assigned(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        alloc.assigned_nodes.is_empty(),
        "Expected no assigned nodes, but found {:?}",
        alloc.assigned_nodes
    );
}

#[then("assigned nodes should be released")]
fn assigned_nodes_released(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        alloc.assigned_nodes.is_empty(),
        "Expected assigned nodes to be released, but found {:?}",
        alloc.assigned_nodes
    );
}

#[then("the allocation should have an unbounded lifecycle")]
fn check_unbounded_lifecycle(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        matches!(alloc.lifecycle.lifecycle_type, LifecycleType::Unbounded),
        "Expected Unbounded lifecycle, got {:?}",
        alloc.lifecycle.lifecycle_type
    );
}

#[then("the allocation walltime should be zero")]
fn check_walltime_zero(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    match &alloc.lifecycle.lifecycle_type {
        LifecycleType::Unbounded => {} // no walltime — passes
        LifecycleType::Bounded { walltime } => {
            assert!(
                walltime.is_zero(),
                "Expected zero walltime for unbounded, got {walltime:?}"
            );
        }
        other => panic!("Expected Unbounded or Bounded, got {:?}", other),
    }
}

#[then("the allocation does not expire automatically")]
fn check_no_auto_expiry(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        matches!(alloc.lifecycle.lifecycle_type, LifecycleType::Unbounded),
        "Unbounded allocations should not expire automatically"
    );
}

#[then("the allocation should have a reactive lifecycle")]
fn check_reactive_lifecycle(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        matches!(
            alloc.lifecycle.lifecycle_type,
            LifecycleType::Reactive { .. }
        ),
        "Expected Reactive lifecycle, got {:?}",
        alloc.lifecycle.lifecycle_type
    );
}

#[then(regex = r#"^the allocation min_nodes should be (\d+)$"#)]
fn check_min_nodes(world: &mut LatticeWorld, expected: u32) {
    let alloc = world.last_allocation();
    match &alloc.lifecycle.lifecycle_type {
        LifecycleType::Reactive { min_nodes, .. } => {
            assert_eq!(
                *min_nodes, expected,
                "Expected min_nodes {expected}, got {min_nodes}"
            );
        }
        other => panic!("Expected Reactive lifecycle, got {:?}", other),
    }
}

#[then(regex = r#"^the allocation max_nodes should be (\d+)$"#)]
fn check_max_nodes(world: &mut LatticeWorld, expected: u32) {
    let alloc = world.last_allocation();
    match &alloc.lifecycle.lifecycle_type {
        LifecycleType::Reactive { max_nodes, .. } => {
            assert_eq!(
                *max_nodes, expected,
                "Expected max_nodes {expected}, got {max_nodes}"
            );
        }
        other => panic!("Expected Reactive lifecycle, got {:?}", other),
    }
}

#[then(regex = r#"^(\d+) allocations should be created$"#)]
fn check_allocation_count(world: &mut LatticeWorld, expected: usize) {
    assert_eq!(
        world.allocations.len(),
        expected,
        "Expected {expected} allocations, got {}",
        world.allocations.len()
    );
}

#[then("all allocations should share the same task_group_id")]
fn check_shared_task_group_id(world: &mut LatticeWorld) {
    assert!(
        !world.allocations.is_empty(),
        "No allocations to check task_group_id"
    );
    let first_dag_id = world.allocations[0].dag_id.clone();
    assert!(
        first_dag_id.is_some(),
        "First allocation should have a dag_id (task_group_id)"
    );
    for alloc in &world.allocations {
        assert_eq!(
            alloc.dag_id, first_dag_id,
            "All allocations should share the same dag_id"
        );
    }
}

#[then(regex = r#"^the requeue count should be (\d+)$"#)]
fn check_requeue_count(world: &mut LatticeWorld, expected: u32) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.requeue_count, expected,
        "Expected requeue count {expected}, got {}",
        alloc.requeue_count
    );
}

#[then(regex = r#"^the failure reason should include "(\w+)"$"#)]
fn check_failure_reason(world: &mut LatticeWorld, expected_fragment: String) {
    let alloc = world.last_allocation();
    let message = alloc
        .message
        .as_deref()
        .expect("Expected a failure message but found None");
    assert!(
        message.contains(&expected_fragment),
        "Expected failure reason to contain '{expected_fragment}', got '{message}'"
    );
}

use cucumber::{given, then, when};

use crate::LatticeWorld;
use super::helpers::parse_allocation_state;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^a max of (\d+) concurrent sessions per user$"#)]
fn given_max_sessions(world: &mut LatticeWorld, max: u32) {
    world.session_max_per_user = Some(max);
}

#[given(regex = r#"^user "(\w[\w-]*)" has (\d+) active sessions$"#)]
fn given_user_active_sessions(world: &mut LatticeWorld, user: String, count: u32) {
    world.session_user = Some(user.clone());
    world.active_sessions_per_user.insert(user, count);
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^I create an interactive session for tenant "(\w[\w-]*)"$"#)]
fn create_session(world: &mut LatticeWorld, tenant: String) {
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(1)
        .lifecycle_unbounded()
        .tag("session", "true")
        .build();
    let idx = world.allocations.len();
    world.allocations.push(alloc);
    world.session_alloc_idx = Some(idx);
    world.session_indices.push(idx);
}

#[when(regex = r#"^the session transitions to "(\w+)"$"#)]
fn session_transitions(world: &mut LatticeWorld, target_str: String) {
    let target = parse_allocation_state(&target_str);
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &mut world.allocations[idx];
    assert!(
        alloc.state.can_transition_to(&target),
        "Invalid session transition from {:?} to {:?}",
        alloc.state,
        target
    );
    alloc.state = target.clone();
    if target == AllocationState::Running && alloc.started_at.is_none() {
        alloc.started_at = Some(chrono::Utc::now());
        alloc.assigned_nodes = vec![format!("node-{idx}")];
    }
}

#[when("I delete the session")]
fn delete_session(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &mut world.allocations[idx];
    assert!(
        alloc.state.can_transition_to(&AllocationState::Cancelled),
        "Cannot cancel session in state {:?}",
        alloc.state
    );
    alloc.state = AllocationState::Cancelled;
    alloc.assigned_nodes.clear();
    alloc.completed_at = Some(chrono::Utc::now());
}

#[when(regex = r#"^user "(\w[\w-]*)" attempts to create another session$"#)]
fn user_attempts_session(world: &mut LatticeWorld, user: String) {
    let max = world
        .session_max_per_user
        .expect("max_per_user not set");
    let active = world
        .active_sessions_per_user
        .get(&user)
        .copied()
        .unwrap_or(0);
    if active >= max {
        world.last_error = Some(lattice_common::error::LatticeError::QuotaExceeded {
            tenant: user.clone(),
            detail: "max_sessions_exceeded".into(),
        });
    } else {
        let alloc = AllocationBuilder::new()
            .user(&user)
            .nodes(1)
            .lifecycle_unbounded()
            .tag("session", "true")
            .build();
        let idx = world.allocations.len();
        world.allocations.push(alloc);
        world.session_alloc_idx = Some(idx);
        world.session_indices.push(idx);
        let count = world
            .active_sessions_per_user
            .entry(user)
            .or_insert(0);
        *count += 1;
    }
}

#[when("the client disconnects")]
fn client_disconnects(world: &mut LatticeWorld) {
    // Session remains running on disconnect; record disconnect state.
    let idx = world.session_alloc_idx.expect("no session created");
    assert_eq!(
        world.allocations[idx].state,
        AllocationState::Running,
        "Session must be running before disconnect"
    );
    world.attach_allowed = Some(false);
}

#[when("the client reattaches to the session")]
fn client_reattaches(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    assert_eq!(
        world.allocations[idx].state,
        AllocationState::Running,
        "Session must still be running to reattach"
    );
    world.attach_allowed = Some(true);
}

#[when(regex = r#"^I create (\d+) interactive sessions for tenant "(\w[\w-]*)"$"#)]
fn create_multiple_sessions(world: &mut LatticeWorld, count: usize, tenant: String) {
    for i in 0..count {
        let mut alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .nodes(1)
            .lifecycle_unbounded()
            .tag("session", "true")
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = vec![format!("node-{}", world.allocations.len())];
        alloc.started_at = Some(chrono::Utc::now());
        let idx = world.allocations.len();
        world.allocations.push(alloc);
        world.session_indices.push(idx);
        if i == 0 {
            world.session_alloc_idx = Some(idx);
        }
    }
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^the session allocation should be "(\w+)"$"#)]
fn session_alloc_state(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &world.allocations[idx];
    assert_eq!(
        alloc.state, expected_state,
        "Expected session state {:?}, got {:?}",
        expected_state, alloc.state
    );
}

#[then("the session should have an unbounded lifecycle")]
fn session_unbounded(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &world.allocations[idx];
    assert!(
        matches!(alloc.lifecycle.lifecycle_type, LifecycleType::Unbounded),
        "Expected Unbounded lifecycle, got {:?}",
        alloc.lifecycle.lifecycle_type
    );
}

#[then("the session should have a session tag")]
fn session_has_tag(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &world.allocations[idx];
    assert_eq!(
        alloc.tags.get("session"),
        Some(&"true".to_string()),
        "Session should have tag session=true"
    );
}

#[then("the session walltime should be zero")]
fn session_walltime_zero(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    let alloc = &world.allocations[idx];
    match &alloc.lifecycle.lifecycle_type {
        LifecycleType::Unbounded => {} // no walltime — passes
        LifecycleType::Bounded { walltime } => {
            assert!(
                walltime.is_zero(),
                "Expected zero walltime for session, got {walltime:?}"
            );
        }
        other => panic!("Expected Unbounded or Bounded, got {:?}", other),
    }
}

#[then(regex = r#"^listing sessions for tenant "(\w[\w-]*)" should return (\d+) sessions?$"#)]
fn listing_sessions_for_tenant(world: &mut LatticeWorld, tenant: String, expected: usize) {
    let count = world
        .allocations
        .iter()
        .filter(|a| {
            a.tenant == tenant
                && a.tags.get("session").map(|v| v.as_str()) == Some("true")
        })
        .count();
    assert_eq!(
        count, expected,
        "Expected {expected} sessions for tenant {tenant}, got {count}"
    );
}

#[then(regex = r#"^the session creation should be rejected with "(\w[\w_-]*)"$"#)]
fn session_rejected_with(world: &mut LatticeWorld, expected_fragment: String) {
    let err = world
        .last_error
        .as_ref()
        .expect("Expected session creation error, but none occurred");
    let msg = err.to_string();
    assert!(
        msg.contains(&expected_fragment),
        "Expected error to contain '{expected_fragment}', got '{msg}'"
    );
}

#[then("the session should remain running")]
fn session_remain_running(world: &mut LatticeWorld) {
    let idx = world.session_alloc_idx.expect("no session created");
    assert_eq!(
        world.allocations[idx].state,
        AllocationState::Running,
        "Session should remain running after disconnect"
    );
}

#[then("the attach should succeed")]
fn attach_should_succeed(world: &mut LatticeWorld) {
    assert_eq!(
        world.attach_allowed,
        Some(true),
        "Reattach should have succeeded"
    );
}

#[then("both sessions should have separate allocations")]
fn both_sessions_separate(world: &mut LatticeWorld) {
    assert!(
        world.session_indices.len() >= 2,
        "Expected at least 2 sessions, got {}",
        world.session_indices.len()
    );
    let id0 = world.allocations[world.session_indices[0]].id;
    let id1 = world.allocations[world.session_indices[1]].id;
    assert_ne!(
        id0, id1,
        "Sessions should have different allocation IDs"
    );
}

#[then("each session should be on a different node")]
fn sessions_different_nodes(world: &mut LatticeWorld) {
    assert!(
        world.session_indices.len() >= 2,
        "Expected at least 2 sessions"
    );
    let nodes0 = &world.allocations[world.session_indices[0]].assigned_nodes;
    let nodes1 = &world.allocations[world.session_indices[1]].assigned_nodes;
    assert!(
        !nodes0.is_empty() && !nodes1.is_empty(),
        "Both sessions should have assigned nodes"
    );
    assert_ne!(
        nodes0, nodes1,
        "Sessions should be on different nodes, but both are on {:?}",
        nodes0
    );
}

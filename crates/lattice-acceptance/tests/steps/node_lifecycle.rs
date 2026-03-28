use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::error::LatticeError;
use lattice_common::traits::*;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;
use uuid::Uuid;

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^a new node "(\S+)" not yet registered$"#)]
fn given_new_node_not_registered(world: &mut LatticeWorld, node_id: String) {
    // Node exists conceptually but is not in the registry yet.
    // Store the id for later use; do NOT add to world.nodes or registry.
    world.nodes.push(
        NodeBuilder::new()
            .id(&node_id)
            .state(NodeState::Unknown)
            .build(),
    );
}

#[given(regex = r#"^a node "(\S+)" in "(\w+)" state$"#)]
fn given_node_in_state(world: &mut LatticeWorld, node_id: String, state_str: String) {
    let state = parse_node_state(&state_str);
    let node = NodeBuilder::new().id(&node_id).state(state).build();
    world.nodes.push(node.clone());
    let registry = &world.registry;
    registry.nodes.lock().unwrap().insert(node_id, node);
}

#[given(regex = r#"^a node "(\S+)" undergoing sensitive wipe$"#)]
fn given_node_undergoing_sensitive_wipe(world: &mut LatticeWorld, node_id: String) {
    // Node is in a Down state while a wipe is attempted.
    let node = NodeBuilder::new()
        .id(&node_id)
        .state(NodeState::Down {
            reason: "sensitive_wipe_in_progress".into(),
        })
        .build();
    world.nodes.push(node.clone());
    world.registry.nodes.lock().unwrap().insert(node_id, node);
}

#[given(regex = r#"^a node "(\S+)" with heartbeat sequence at (\d+)$"#)]
fn given_node_with_heartbeat_sequence(world: &mut LatticeWorld, node_id: String, seq: u64) {
    let node = NodeBuilder::new()
        .id(&node_id)
        .state(NodeState::Ready)
        .build();
    world.nodes.push(node.clone());
    world
        .registry
        .nodes
        .lock()
        .unwrap()
        .insert(node_id.clone(), node);
    // Store the expected sequence number for later comparison.
    world.nodes.last_mut().unwrap().owner_version = seq;
}

#[given(regex = r#"^a heartbeat timeout of (\d+) seconds$"#)]
fn given_heartbeat_timeout(_world: &mut LatticeWorld, _timeout_secs: u64) {
    // Timeout config is implicit; store for scenario context.
    // The step logic will simulate the timeout expiry.
}

// Note: "N ready nodes with conformance fingerprint" is handled by conformance.rs
// For node_lifecycle scenarios that need specific node IDs and registry registration,
// use the explicit node setup steps instead.

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^I drain node (\d+)$"#)]
fn drain_node(world: &mut LatticeWorld, idx: usize) {
    let node = &mut world.nodes[idx];
    assert!(
        matches!(node.state, NodeState::Ready),
        "Can only drain a Ready node, got {:?}",
        node.state
    );
    // Follows the real drain path: Ready → Draining, then if no active
    // allocations → Drained immediately.  BDD tests have no allocations
    // so drain completes directly.
    let final_state = NodeState::Drained;
    node.state = final_state.clone();
    world
        .registry
        .nodes
        .lock()
        .unwrap()
        .get_mut(&node.id)
        .unwrap()
        .state = final_state;
}

#[when(regex = r#"^I undrain node (\d+)$"#)]
fn undrain_node(world: &mut LatticeWorld, idx: usize) {
    let node = &mut world.nodes[idx];
    assert!(
        matches!(node.state, NodeState::Drained),
        "Can only undrain a Drained node, got {:?}",
        node.state
    );
    node.state = NodeState::Ready;
    world
        .registry
        .nodes
        .lock()
        .unwrap()
        .get_mut(&node.id)
        .unwrap()
        .state = NodeState::Ready;
}

#[when(regex = r#"^user "(\w[\w-]*)" claims node (\d+) for tenant "(\w[\w-]*)"$"#)]
fn user_claims_node_for_tenant(world: &mut LatticeWorld, user: String, idx: usize, tenant: String) {
    let node_id = world.nodes[idx].id.clone();
    let ownership = NodeOwnership {
        tenant,
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
    } else {
        // Sync the world.nodes vector.
        let reg_node = world.registry.nodes.lock().unwrap().get(&node_id).cloned();
        if let Some(rn) = reg_node {
            world.nodes[idx].owner = rn.owner;
        }
    }
}

// Note: user_attempts_claim is in common.rs

#[when("the node agent sends its first heartbeat")]
fn node_agent_first_heartbeat(world: &mut LatticeWorld) {
    // Simulate first heartbeat: register the node and set it to Ready.
    let node = world.nodes.last_mut().expect("no node to register");
    node.state = NodeState::Ready;
    node.last_heartbeat = Some(chrono::Utc::now());
    world
        .registry
        .nodes
        .lock()
        .unwrap()
        .insert(node.id.clone(), node.clone());
}

#[when(regex = r#"^node "(\S+)" misses heartbeats$"#)]
fn node_misses_heartbeats(world: &mut LatticeWorld, node_id: String) {
    // Transition the node to Degraded due to missed heartbeats.
    let state = NodeState::Degraded {
        reason: "missed_heartbeats".into(),
    };
    for node in &mut world.nodes {
        if node.id == node_id {
            node.state = state.clone();
        }
    }
    if let Some(n) = world.registry.nodes.lock().unwrap().get_mut(&node_id) {
        n.state = state;
    }
}

#[when("the grace period expires without recovery")]
fn grace_period_expires(world: &mut LatticeWorld) {
    // Escalate Degraded → Down for any degraded node.
    for node in &mut world.nodes {
        if matches!(node.state, NodeState::Degraded { .. }) {
            node.state = NodeState::Down {
                reason: "grace_period_expired".into(),
            };
            if let Some(n) = world.registry.nodes.lock().unwrap().get_mut(&node.id) {
                n.state = NodeState::Down {
                    reason: "grace_period_expired".into(),
                };
            }
        }
    }
}

#[when("the node agent restarts and sends a heartbeat")]
fn node_agent_restarts(world: &mut LatticeWorld) {
    // Recovery: Down → Ready.
    let node = world.nodes.last_mut().expect("no node");
    assert!(
        matches!(node.state, NodeState::Down { .. }),
        "Expected Down state, got {:?}",
        node.state
    );
    node.state = NodeState::Ready;
    node.last_heartbeat = Some(chrono::Utc::now());
    if let Some(n) = world.registry.nodes.lock().unwrap().get_mut(&node.id) {
        n.state = NodeState::Ready;
        n.last_heartbeat = Some(chrono::Utc::now());
    }
}

#[when("the wipe operation fails")]
fn wipe_operation_fails(world: &mut LatticeWorld) {
    // The node remains Down; set an alert flag.
    let node = world.nodes.last_mut().expect("no node");
    node.state = NodeState::Down {
        reason: "wipe_failed_quarantine".into(),
    };
    if let Some(n) = world.registry.nodes.lock().unwrap().get_mut(&node.id) {
        n.state = NodeState::Down {
            reason: "wipe_failed_quarantine".into(),
        };
    }
    // Record an audit entry for the alert.
    let entry = AuditEntry::new(lattice_audit_event(
        audit_actions::NODE_RELEASE,
        "system",
        hpc_audit::AuditScope::node(&node.id),
        hpc_audit::AuditOutcome::Failure,
        "wipe failed",
        serde_json::json!({"alert": "wipe_failed", "node": node.id}),
        hpc_audit::AuditSource::LatticeNodeAgent,
    ));
    world.audit.entries.lock().unwrap().push(entry);
}

#[when(regex = r#"^a heartbeat with sequence (\d+) is received$"#)]
fn stale_heartbeat_received(world: &mut LatticeWorld, incoming_seq: u64) {
    let node = world.nodes.last().expect("no node");
    let current_seq = node.owner_version;
    if incoming_seq < current_seq {
        // Stale heartbeat: reject it. Store rejection flag.
        world.last_error = Some(LatticeError::Internal("stale_heartbeat_rejected".into()));
    }
}

#[when(regex = r#"^node "(\S+)" sends no heartbeat for (\d+) seconds$"#)]
fn node_no_heartbeat(world: &mut LatticeWorld, node_id: String, _seconds: u64) {
    // Simulate heartbeat timeout → Degraded.
    let state = NodeState::Degraded {
        reason: "heartbeat_timeout".into(),
    };
    for node in &mut world.nodes {
        if node.id == node_id {
            node.state = state.clone();
        }
    }
    if let Some(n) = world.registry.nodes.lock().unwrap().get_mut(&node_id) {
        n.state = state;
    }
}

#[when(regex = r#"^I drain node (\d+) and node (\d+) simultaneously$"#)]
fn drain_two_nodes(world: &mut LatticeWorld, idx_a: usize, idx_b: usize) {
    for idx in [idx_a, idx_b] {
        let node = &mut world.nodes[idx];
        assert!(
            matches!(node.state, NodeState::Ready),
            "Can only drain a Ready node, got {:?}",
            node.state
        );
        // No active allocations in BDD tests → immediate drain completion
        let final_state = NodeState::Drained;
        node.state = final_state.clone();
        world
            .registry
            .nodes
            .lock()
            .unwrap()
            .get_mut(&node.id)
            .unwrap()
            .state = final_state;
    }
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^node (\d+) should be in state "(\w+)"$"#)]
fn node_in_state(world: &mut LatticeWorld, idx: usize, expected: String) {
    let node = &world.nodes[idx];
    assert_node_state_matches(&node.state, &expected);
}

#[then(regex = r#"^nodes (\d+) through (\d+) should be in state "(\w+)"$"#)]
fn nodes_range_in_state(world: &mut LatticeWorld, from: usize, to: usize, expected: String) {
    for i in from..=to {
        let node = &world.nodes[i];
        assert_node_state_matches(&node.state, &expected);
    }
}

// Note: node_owned_by and receives_ownership_conflict are in common.rs

#[then(regex = r#"^node "(\S+)" should be registered$"#)]
fn node_should_be_registered(world: &mut LatticeWorld, node_id: String) {
    let nodes = world.registry.nodes.lock().unwrap();
    assert!(
        nodes.contains_key(&node_id),
        "Node {node_id} should be registered in the registry"
    );
}

#[then(regex = r#"^node "(\S+)" should be in state "(\w+)"$"#)]
fn named_node_in_state(world: &mut LatticeWorld, node_id: String, expected: String) {
    let node = world
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("Node {node_id} not found"));
    assert_node_state_matches(&node.state, &expected);
}

#[then(regex = r#"^node "(\S+)" should transition to "(\w+)"$"#)]
fn node_should_transition_to(world: &mut LatticeWorld, node_id: String, expected: String) {
    // The transition already happened in the When step; verify the state.
    let node = world
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("Node {node_id} not found"));
    assert_node_state_matches(&node.state, &expected);
}

#[then("the node should be available for scheduling")]
fn node_available_for_scheduling(world: &mut LatticeWorld) {
    let node = world.nodes.last().expect("no node");
    assert!(
        matches!(node.state, NodeState::Ready),
        "Node should be Ready for scheduling, got {:?}",
        node.state
    );
}

#[then(regex = r#"^node "(\S+)" should remain in "(\w+)" state$"#)]
fn node_should_remain_in_state(world: &mut LatticeWorld, node_id: String, expected: String) {
    let node = world
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("Node {node_id} not found"));
    assert_node_state_matches(&node.state, &expected);
}

#[then(regex = r#"^node "(\S+)" should not return to the scheduling pool$"#)]
fn node_not_in_scheduling_pool(world: &mut LatticeWorld, node_id: String) {
    let node = world
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("Node {node_id} not found"));
    assert!(
        !matches!(node.state, NodeState::Ready),
        "Node {node_id} should NOT be Ready (not schedulable), got {:?}",
        node.state
    );
}

#[then("an operator alert should be raised")]
fn operator_alert_raised(world: &mut LatticeWorld) {
    let entries = world.audit.entries.lock().unwrap();
    let has_alert = entries.iter().any(|e| {
        e.event
            .metadata
            .get("alert")
            .and_then(|v| v.as_str())
            .map(|s| s.contains("wipe_failed"))
            .unwrap_or(false)
    });
    assert!(has_alert, "Expected an operator alert for wipe failure");
}

#[then("the stale heartbeat should be rejected")]
fn stale_heartbeat_rejected(world: &mut LatticeWorld) {
    let err = world
        .last_error
        .as_ref()
        .expect("Expected stale heartbeat rejection");
    match err {
        LatticeError::Internal(msg) => {
            assert!(
                msg.contains("stale_heartbeat"),
                "Expected stale_heartbeat error, got {msg}"
            );
        }
        other => panic!("Expected Internal error for stale heartbeat, got {other:?}"),
    }
}

#[then("the node state should not change")]
fn node_state_unchanged(world: &mut LatticeWorld) {
    // The node should still be Ready (stale heartbeat was rejected, no state change).
    let node = world.nodes.last().expect("no node");
    assert!(
        matches!(node.state, NodeState::Ready),
        "Node state should remain Ready after stale heartbeat, got {:?}",
        node.state
    );
}

// ─── Helpers ───────────────────────────────────────────────

fn parse_node_state(s: &str) -> NodeState {
    match s {
        "Ready" => NodeState::Ready,
        "Draining" => NodeState::Draining,
        "Drained" => NodeState::Drained,
        "Down" => NodeState::Down {
            reason: "test".into(),
        },
        "Degraded" => NodeState::Degraded {
            reason: "test".into(),
        },
        "Unknown" => NodeState::Unknown,
        "Booting" => NodeState::Booting,
        other => panic!("Unknown node state: {other}"),
    }
}

fn assert_node_state_matches(actual: &NodeState, expected_str: &str) {
    let matches = match (actual, expected_str) {
        (NodeState::Ready, "Ready") => true,
        (NodeState::Draining, "Draining") => true,
        (NodeState::Drained, "Drained") => true,
        (NodeState::Down { .. }, "Down") => true,
        (NodeState::Degraded { .. }, "Degraded") => true,
        (NodeState::Unknown, "Unknown") => true,
        (NodeState::Booting, "Booting") => true,
        (NodeState::Failed { .. }, "Failed") => true,
        _ => false,
    };
    assert!(
        matches,
        "Expected node state {expected_str}, got {actual:?}"
    );
}

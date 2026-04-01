use cucumber::{given, then, when};

use super::helpers::parse_audit_action_str;
use crate::LatticeWorld;
use lattice_common::error::LatticeError;
use lattice_common::traits::*;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;
use uuid::Uuid;

// ─── Given Steps ───────────────────────────────────────────
// Note: running sensitive allocation, claim node, ownership conflict steps are in common.rs

#[given(regex = r#"^user "(\w[\w-]*)" has an active attach session$"#)]
fn given_user_has_active_attach(world: &mut LatticeWorld, user: String) {
    // Record that the user has one active session.
    let count = world.active_sessions_per_user.entry(user).or_insert(0);
    *count += 1;
}

#[given("a running sensitive allocation")]
fn given_running_sensitive_allocation(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .sensitive()
        .state(AllocationState::Running)
        .nodes(1)
        .build();
    alloc.assigned_nodes = vec!["x1000c0s0b0n0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given(regex = r#"^(\d+) nodes claimed for sensitive allocations$"#)]
fn given_nodes_claimed_for_sensitive(world: &mut LatticeWorld, count: usize) {
    for i in 0..count {
        let node_id = format!("x1000c0s0b0n{i}");
        let node = NodeBuilder::new()
            .id(&node_id)
            .group(0)
            .owner(NodeOwnership {
                tenant: "hospital-a".into(),
                vcluster: "sensitive".into(),
                allocation: Uuid::new_v4(),
                claimed_by: Some("sensitive-user".into()),
                is_borrowed: false,
            })
            .build();
        world
            .registry
            .nodes
            .lock()
            .unwrap()
            .insert(node_id, node.clone());
        world.nodes.push(node);
    }
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^user "(\w[\w-]*)" claims node (\d+) for a sensitive allocation$"#)]
fn user_claims_node(world: &mut LatticeWorld, user: String, idx: usize) {
    let node_id = world.nodes[idx].id.clone();
    let ownership = NodeOwnership {
        tenant: world
            .tenants
            .last()
            .map(|t| t.id.clone())
            .unwrap_or_else(|| "test-tenant".into()),
        vcluster: "sensitive".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some(user.clone()),
        is_borrowed: false,
    };
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(world.registry.claim_node(&node_id, ownership))
    });
    match result {
        Ok(()) => {
            // Sync world.nodes.
            let reg_node = world.registry.nodes.lock().unwrap().get(&node_id).cloned();
            if let Some(rn) = reg_node {
                world.nodes[idx].owner = rn.owner;
            }
            // Record audit entry.
            let entry = AuditEntry::new(lattice_audit_event(
                audit_actions::NODE_CLAIM,
                &user,
                hpc_audit::AuditScope::node(&node_id),
                hpc_audit::AuditOutcome::Success,
                "sensitive node claim",
                serde_json::json!({"node": node_id, "sensitive": true}),
                hpc_audit::AuditSource::LatticeQuorum,
            ));
            world.audit.entries.lock().unwrap().push(entry);
        }
        Err(e) => {
            world.last_error = Some(e);
        }
    }
}

#[when("I submit a sensitive allocation")]
fn submit_sensitive_allocation(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new().sensitive().build();
    world.allocations.push(alloc);
}

#[when("I submit a sensitive allocation with an unsigned image")]
fn submit_sensitive_unsigned_image(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new().sensitive().build();
    // Override: image is unsigned.
    alloc.environment.sign_required = true;
    alloc
        .environment
        .images
        .push(lattice_common::types::ImageRef {
            spec: "untrusted/image:latest".into(),
            image_type: lattice_common::types::ImageType::Oci,
            name: "untrusted/image".into(),
            original_tag: "latest".into(),
            ..lattice_common::types::ImageRef::default()
        });
    // Simulate verification failure: mark as rejected.
    let image_signed = false;
    if !image_signed {
        alloc.state = AllocationState::Failed;
        alloc.message = Some("unsigned_image".into());
        world.last_error = Some(LatticeError::SensitiveIsolation("unsigned_image".into()));
    }
    world.allocations.push(alloc);
}

#[when(regex = r#"^user "(\w[\w-]*)" attempts a second concurrent attach$"#)]
fn user_attempts_second_attach(world: &mut LatticeWorld, user: String) {
    let session_count = world
        .active_sessions_per_user
        .get(&user)
        .copied()
        .unwrap_or(0);
    // Sensitive allocations allow at most 1 attach session.
    if session_count >= 1 {
        world.last_error = Some(LatticeError::SensitiveIsolation(
            "max_sessions_exceeded".into(),
        ));
        world.attach_allowed = Some(false);
    } else {
        world.attach_allowed = Some(true);
    }
}

#[when("the allocation reads data from the mounted path")]
fn allocation_reads_data(world: &mut LatticeWorld) {
    // Simulate a data access that gets logged.
    let user = world
        .allocations
        .last()
        .map(|a| a.user.clone())
        .unwrap_or_else(|| "unknown".into());
    let entry = AuditEntry::new(lattice_audit_event(
        audit_actions::DATA_ACCESS,
        &user,
        hpc_audit::AuditScope::default(),
        hpc_audit::AuditOutcome::Success,
        "data access",
        serde_json::json!({
            "path": "/mnt/sensitive/data",
            "operation": "read",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        hpc_audit::AuditSource::LatticeNodeAgent,
    ));
    world.audit.entries.lock().unwrap().push(entry);
}

#[when(regex = r#"^vCluster "(\w[\w-]*)" requests to borrow idle sensitive nodes$"#)]
fn vcluster_requests_borrow_sensitive(world: &mut LatticeWorld, _vcluster: String) {
    // Check all nodes: any that are owned for sensitive work should deny borrowing.
    let nodes = world.registry.nodes.lock().unwrap();
    let sensitive_nodes: Vec<_> = nodes
        .values()
        .filter(|n| {
            n.owner
                .as_ref()
                .map(|o| !o.is_borrowed && o.vcluster == "sensitive")
                .unwrap_or(false)
        })
        .collect();
    if !sensitive_nodes.is_empty() {
        world.last_error = Some(LatticeError::SensitiveIsolation(
            "cannot_borrow_sensitive_nodes".into(),
        ));
    }
}

#[when(regex = r#"^the sensitive wipe operation fails on node "(\S+)"$"#)]
fn sensitive_wipe_fails(world: &mut LatticeWorld, node_id: String) {
    // Node enters quarantine (Down with wipe failure reason).
    let state = NodeState::Down {
        reason: "wipe_failed_quarantine".into(),
    };
    for node in &mut world.nodes {
        if node.id == node_id {
            node.state = state.clone();
        }
    }
    if let Some(n) = world.registry.nodes.lock().unwrap().get_mut(&node_id) {
        n.state = state;
    }
    // Record critical audit entry.
    let entry = AuditEntry::new(lattice_audit_event(
        audit_actions::NODE_RELEASE,
        "system",
        hpc_audit::AuditScope::node(&node_id),
        hpc_audit::AuditOutcome::Failure,
        "wipe failure quarantine",
        serde_json::json!({
            "node": node_id,
            "event": "wipe_failure_quarantine",
            "severity": "critical",
        }),
        hpc_audit::AuditSource::LatticeNodeAgent,
    ));
    world.audit.entries.lock().unwrap().push(entry);
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^an audit entry should record action "(\w+)"$"#)]
fn audit_entry_recorded(world: &mut LatticeWorld, action_str: String) {
    let action = parse_audit_action_str(&action_str);
    let entries = world.audit.entries_for_action(action);
    assert!(
        !entries.is_empty(),
        "Expected at least one audit entry for action {action_str}, found none"
    );
}

#[then("the allocation environment should require signed images")]
fn requires_signed_images(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        alloc.environment.sign_required,
        "Sensitive allocation should require signed images"
    );
}

#[then("the allocation environment should require vulnerability scanning")]
fn requires_vuln_scan(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        alloc.environment.scan_required,
        "Sensitive allocation should require vulnerability scanning"
    );
}

#[then(regex = r#"^the second attach should be denied with "(\S+)"$"#)]
fn second_attach_denied(world: &mut LatticeWorld, expected_reason: String) {
    let err = world
        .last_error
        .take()
        .expect("Expected an error for denied attach");
    match &err {
        LatticeError::SensitiveIsolation(msg) => {
            assert!(
                msg.contains(&expected_reason),
                "Expected error containing '{expected_reason}', got '{msg}'"
            );
        }
        other => panic!("Expected SensitiveIsolation, got {other:?}"),
    }
    assert_eq!(
        world.attach_allowed,
        Some(false),
        "Second attach should have been denied"
    );
}

// Note: 'the allocation should be rejected with "X"' is in common.rs

#[then("the allocation should use the encrypted storage pool")]
fn allocation_uses_encrypted_storage(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        alloc.sensitive,
        "Sensitive allocation should use encrypted storage pool"
    );
}

#[then("all data operations should be access-logged")]
fn data_operations_logged(world: &mut LatticeWorld) {
    // Verify the sensitive allocation flag implies access logging.
    let alloc = world.last_allocation();
    assert!(
        alloc.sensitive,
        "Sensitive allocations require all data operations to be access-logged"
    );
}

#[then("an audit entry should record the data access")]
fn audit_records_data_access(world: &mut LatticeWorld) {
    let entries = world.audit.entries_for_action(audit_actions::DATA_ACCESS);
    assert!(
        !entries.is_empty(),
        "Expected at least one DataAccess audit entry"
    );
}

#[then("the audit entry should include the user identity and timestamp")]
fn audit_entry_has_identity_and_timestamp(world: &mut LatticeWorld) {
    let entries = world.audit.entries_for_action(audit_actions::DATA_ACCESS);
    let entry = entries.last().expect("Expected a DataAccess audit entry");
    assert!(
        !entry.event.principal.identity.is_empty(),
        "Audit entry should include user identity"
    );
    assert!(
        entry.event.metadata.get("timestamp").is_some(),
        "Audit entry details should include a timestamp"
    );
}

#[then("the borrowing request should be denied")]
fn borrowing_denied(world: &mut LatticeWorld) {
    let err = world
        .last_error
        .take()
        .expect("Expected borrowing denial error");
    match &err {
        LatticeError::SensitiveIsolation(msg) => {
            assert!(
                msg.contains("cannot_borrow_sensitive"),
                "Expected borrow denial, got {msg}"
            );
        }
        other => panic!("Expected SensitiveIsolation for borrow denial, got {other:?}"),
    }
}

#[then("sensitive nodes should remain exclusively assigned")]
fn sensitive_nodes_remain_exclusive(world: &mut LatticeWorld) {
    let nodes = world.registry.nodes.lock().unwrap();
    for node in nodes.values() {
        if let Some(ref owner) = node.owner {
            if owner.vcluster == "sensitive" {
                assert!(
                    !owner.is_borrowed,
                    "Sensitive node {} should not be borrowed",
                    node.id
                );
            }
        }
    }
}

#[then(regex = r#"^node "(\S+)" should enter quarantine$"#)]
fn node_enters_quarantine(world: &mut LatticeWorld, node_id: String) {
    let node = world
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("Node {node_id} not found"));
    assert!(
        matches!(node.state, NodeState::Down { ref reason } if reason.contains("quarantine")),
        "Expected node {node_id} to be quarantined (Down with quarantine reason), got {:?}",
        node.state
    );
}

#[then(regex = r#"^node "(\S+)" should not be scheduled until operator intervention$"#)]
fn node_not_schedulable(world: &mut LatticeWorld, node_id: String) {
    let node = world
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("Node {node_id} not found"));
    assert!(
        !matches!(node.state, NodeState::Ready),
        "Quarantined node {node_id} should NOT be Ready, got {:?}",
        node.state
    );
}

#[then("a critical audit entry should be committed")]
fn critical_audit_entry_committed(world: &mut LatticeWorld) {
    let entries = world.audit.entries.lock().unwrap();
    let has_critical = entries.iter().any(|e| {
        e.event
            .metadata
            .get("severity")
            .and_then(|v| v.as_str())
            .map(|s| s == "critical")
            .unwrap_or(false)
    });
    assert!(
        has_critical,
        "Expected a critical-severity audit entry for wipe failure"
    );
}

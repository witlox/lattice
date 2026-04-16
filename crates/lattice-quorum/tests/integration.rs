//! Integration tests for the lattice-quorum crate.
//!
//! These tests exercise the Raft-based quorum using single-node and
//! multi-node cluster configurations.

use lattice_common::traits::*;
use lattice_common::types::*;
use lattice_quorum::*;
use lattice_test_harness::fixtures::*;
use uuid::Uuid;

// ─── Test 1: Single-node CRUD lifecycle ──────────────────────
// Insert → update state → get → verify full lifecycle.

#[tokio::test]
async fn single_node_crud_lifecycle() {
    let client = create_test_quorum().await.unwrap();

    let alloc = AllocationBuilder::new().tenant("physics").build();
    let id = alloc.id;
    client.insert(alloc).await.unwrap();

    // Verify initial state
    let got = client.get(&id).await.unwrap();
    assert_eq!(got.state, AllocationState::Pending);
    assert_eq!(got.tenant, "physics");

    // Update to Running
    client
        .update_state(&id, AllocationState::Running)
        .await
        .unwrap();
    let got = client.get(&id).await.unwrap();
    assert_eq!(got.state, AllocationState::Running);
    assert!(got.started_at.is_some());

    // Update to Completed
    client
        .update_state(&id, AllocationState::Completed)
        .await
        .unwrap();
    let got = client.get(&id).await.unwrap();
    assert_eq!(got.state, AllocationState::Completed);
    assert!(got.completed_at.is_some());
}

// ─── Test 2: Tenant + vCluster creation ──────────────────────
// Create via Command, verify in GlobalState.

#[tokio::test]
async fn tenant_and_vcluster_creation() {
    let client = create_test_quorum().await.unwrap();

    let tenant = TenantBuilder::new("ml-team")
        .max_nodes(50)
        .fair_share(0.25)
        .build();
    client
        .propose(commands::Command::CreateTenant(tenant))
        .await
        .unwrap();

    let vc = VClusterBuilder::new("ml-hpc").tenant("ml-team").build();
    client
        .propose(commands::Command::CreateVCluster(vc))
        .await
        .unwrap();

    // Verify from state
    let state = client.state().read().await;
    assert!(state.tenants.contains_key("ml-team"));
    assert_eq!(state.tenants["ml-team"].quota.max_nodes, 50);
    assert!(state.vclusters.contains_key("ml-hpc"));
    assert_eq!(state.vclusters["ml-hpc"].tenant, "ml-team");
}

// ─── Test 3: Node claim/release ownership ────────────────────
// Claim → verify → release → verify → re-claim.

#[tokio::test]
async fn node_claim_release_ownership() {
    let client = create_test_quorum().await.unwrap();

    let node = NodeBuilder::new().id("x1000c0s0b0n0").build();
    client
        .propose(commands::Command::RegisterNode(node))
        .await
        .unwrap();

    // Claim the node
    let ownership = NodeOwnership {
        tenant: "med-tenant".into(),
        vcluster: "med-vc".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some("dr-smith".into()),
        is_borrowed: false,
    };
    client
        .claim_node(&"x1000c0s0b0n0".to_string(), ownership)
        .await
        .unwrap();

    // Verify ownership
    let got = client.get_node(&"x1000c0s0b0n0".to_string()).await.unwrap();
    assert!(got.owner.is_some());
    assert_eq!(
        got.owner.as_ref().unwrap().claimed_by.as_deref(),
        Some("dr-smith")
    );

    // Release
    client
        .release_node(&"x1000c0s0b0n0".to_string())
        .await
        .unwrap();

    // Verify released
    let got = client.get_node(&"x1000c0s0b0n0".to_string()).await.unwrap();
    assert!(got.owner.is_none());

    // Re-claim with different owner should succeed
    let ownership2 = NodeOwnership {
        tenant: "med-tenant".into(),
        vcluster: "med-vc".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some("dr-jones".into()),
        is_borrowed: false,
    };
    client
        .claim_node(&"x1000c0s0b0n0".to_string(), ownership2)
        .await
        .unwrap();
    let got = client.get_node(&"x1000c0s0b0n0".to_string()).await.unwrap();
    assert_eq!(
        got.owner.as_ref().unwrap().claimed_by.as_deref(),
        Some("dr-jones")
    );
}

// ─── Test 4: 3-node cluster replication ──────────────────────
// Write on leader, read all 3, verify consistent.

#[tokio::test]
#[ignore = "slow: spins up 3-node Raft cluster"]
async fn three_node_cluster_replication() {
    let clients = create_test_cluster(3).await.unwrap();
    let leader = &clients[0];

    // Write allocation on leader
    let alloc = AllocationBuilder::new().tenant("bio").build();
    let id = alloc.id;
    leader.insert(alloc).await.unwrap();

    // Wait for replication
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify on leader
    let got = leader.get(&id).await.unwrap();
    assert_eq!(got.id, id);
    assert_eq!(got.tenant, "bio");

    // Verify replicated to all followers
    for (i, client) in clients.iter().enumerate() {
        let state = client.state().read().await;
        assert!(
            state.allocations.contains_key(&id),
            "allocation should be replicated to node {i}"
        );
    }
}

// ─── Test 5: Concurrent writes ──────────────────────────────
// 10 parallel inserts, verify all present.

#[tokio::test]
async fn concurrent_writes() {
    let client = create_test_quorum().await.unwrap();

    let mut handles = Vec::new();
    let mut ids = Vec::new();

    for _ in 0..10 {
        let alloc = AllocationBuilder::new().tenant("concurrent").build();
        let id = alloc.id;
        ids.push(id);

        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.insert(alloc).await.unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all 10 exist
    let filter = AllocationFilter {
        tenant: Some("concurrent".to_string()),
        ..Default::default()
    };
    let all = client.list(&filter).await.unwrap();
    assert_eq!(all.len(), 10, "all 10 concurrent inserts should succeed");

    for id in &ids {
        let got = client.get(id).await.unwrap();
        assert_eq!(got.tenant, "concurrent");
    }
}

// ─── Test 6: Audit log append and query ──────────────────────
// Record entries with different users, query with filters.

#[tokio::test]
async fn audit_log_append_and_query() {
    let client = create_test_quorum().await.unwrap();

    use lattice_common::traits::{audit_actions, lattice_audit_event};

    // Record 3 entries
    let entry1 = AuditEntry::new(lattice_audit_event(
        audit_actions::NODE_CLAIM,
        "dr-smith",
        hpc_audit::AuditScope::default(),
        hpc_audit::AuditOutcome::Success,
        "node claim",
        serde_json::json!({"node": "x1000c0s0b0n0"}),
        hpc_audit::AuditSource::LatticeQuorum,
    ));
    let entry2 = AuditEntry::new(lattice_audit_event(
        audit_actions::NODE_RELEASE,
        "dr-jones",
        hpc_audit::AuditScope::default(),
        hpc_audit::AuditOutcome::Success,
        "node release",
        serde_json::json!({"node": "x1000c0s0b0n1"}),
        hpc_audit::AuditSource::LatticeQuorum,
    ));
    let entry3 = AuditEntry::new(lattice_audit_event(
        audit_actions::NODE_RELEASE,
        "dr-smith",
        hpc_audit::AuditScope::default(),
        hpc_audit::AuditOutcome::Success,
        "node release",
        serde_json::json!({"node": "x1000c0s0b0n0"}),
        hpc_audit::AuditSource::LatticeQuorum,
    ));

    client.record(entry1).await.unwrap();
    client.record(entry2).await.unwrap();
    client.record(entry3).await.unwrap();

    // Query for dr-smith only
    let smith_entries = client
        .query(&AuditFilter {
            principal: Some("dr-smith".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(smith_entries.len(), 2);

    // Query with no filter → all entries
    let all_entries = client.query(&AuditFilter::default()).await.unwrap();
    assert_eq!(all_entries.len(), 3);

    // Query for dr-jones
    let jones_entries = client
        .query(&AuditFilter {
            principal: Some("dr-jones".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(jones_entries.len(), 1);
    assert_eq!(jones_entries[0].event.action, audit_actions::NODE_RELEASE);
}

// ─── Test 7: Quota enforcement ───────────────────────────────
// Max 2 concurrent, insert 3 → rejected, cancel + retry → ok.

#[tokio::test]
async fn quota_enforcement() {
    let client = create_test_quorum().await.unwrap();

    let tenant = TenantBuilder::new("limited")
        .max_nodes(100)
        .max_concurrent(2)
        .build();
    client
        .propose(commands::Command::CreateTenant(tenant))
        .await
        .unwrap();

    // Insert 2 allocations — both should succeed
    let alloc1 = AllocationBuilder::new().tenant("limited").build();
    let id1 = alloc1.id;
    client.insert(alloc1).await.unwrap();

    let alloc2 = AllocationBuilder::new().tenant("limited").build();
    client.insert(alloc2).await.unwrap();

    // 3rd should fail with quota error
    let alloc3 = AllocationBuilder::new().tenant("limited").build();
    let result = client.insert(alloc3).await;
    assert!(
        result.is_err(),
        "3rd allocation should be rejected by quota"
    );

    // Cancel the first allocation
    client
        .update_state(&id1, AllocationState::Cancelled)
        .await
        .unwrap();

    // Now a new allocation should succeed
    let alloc4 = AllocationBuilder::new().tenant("limited").build();
    let result = client.insert(alloc4).await;
    assert!(
        result.is_ok(),
        "should succeed after cancelling one allocation"
    );
}

// ─── Test 8: Invalid allocation state transition ─────────────
// Completed→Running is invalid and should return an error.

#[tokio::test]
async fn allocation_state_transitions_validated() {
    let client = create_test_quorum().await.unwrap();

    let alloc = AllocationBuilder::new().tenant("physics").build();
    let id = alloc.id;
    client.insert(alloc).await.unwrap();

    // Move Pending → Running → Completed (valid path)
    client
        .update_state(&id, AllocationState::Running)
        .await
        .unwrap();
    client
        .update_state(&id, AllocationState::Completed)
        .await
        .unwrap();

    // Verify it's completed
    let got = client.get(&id).await.unwrap();
    assert_eq!(got.state, AllocationState::Completed);

    // Completed → Running should fail (invalid transition)
    let result = client.update_state(&id, AllocationState::Running).await;
    assert!(
        result.is_err(),
        "Completed→Running should be rejected as invalid transition"
    );

    // Also verify Pending → Completed is invalid (must go through Running first)
    let alloc2 = AllocationBuilder::new().tenant("physics").build();
    let id2 = alloc2.id;
    client.insert(alloc2).await.unwrap();

    let result2 = client.update_state(&id2, AllocationState::Completed).await;
    assert!(
        result2.is_err(),
        "Pending→Completed should be rejected as invalid transition"
    );
}

// ─── Test 9: Node registration and heartbeat ─────────────────
// Register node, verify it appears, then update heartbeat.

#[tokio::test]
async fn node_registration_and_heartbeat() {
    let client = create_test_quorum().await.unwrap();

    let node = NodeBuilder::new().id("x2000c0s1b0n0").build();
    client
        .propose(commands::Command::RegisterNode(node))
        .await
        .unwrap();

    // Verify node is in state
    let got = client.get_node(&"x2000c0s1b0n0".to_string()).await.unwrap();
    assert_eq!(got.id, "x2000c0s1b0n0");
    assert_eq!(got.state, NodeState::Ready);
    let initial_heartbeat = got.last_heartbeat;

    // Record a heartbeat
    let hb_time = chrono::Utc::now();
    client
        .propose(commands::Command::RecordHeartbeat {
            id: "x2000c0s1b0n0".into(),
            timestamp: hb_time,
            owner_version: 0,
            reattach_in_progress: false,
        })
        .await
        .unwrap();

    // Verify heartbeat was updated
    let got = client.get_node(&"x2000c0s1b0n0".to_string()).await.unwrap();
    assert_eq!(got.last_heartbeat, Some(hb_time));
    assert!(Some(hb_time) >= initial_heartbeat);

    // Record a second heartbeat — should update
    let hb_time2 = chrono::Utc::now();
    client
        .propose(commands::Command::RecordHeartbeat {
            id: "x2000c0s1b0n0".into(),
            timestamp: hb_time2,
            owner_version: 0,
            reattach_in_progress: false,
        })
        .await
        .unwrap();

    let got = client.get_node(&"x2000c0s1b0n0".to_string()).await.unwrap();
    assert_eq!(got.last_heartbeat, Some(hb_time2));
    assert!(hb_time2 >= hb_time);
}

// ─── Test 10: Concurrent tenant creation ─────────────────────
// Create 5 tenants concurrently, verify all exist.

#[tokio::test]
async fn concurrent_tenant_creation() {
    let client = create_test_quorum().await.unwrap();

    let mut handles = Vec::new();
    let tenant_names: Vec<String> = (0..5).map(|i| format!("tenant-{i}")).collect();

    for name in &tenant_names {
        let tenant = TenantBuilder::new(name)
            .max_nodes(10)
            .fair_share(0.2)
            .build();
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.propose(commands::Command::CreateTenant(tenant))
                .await
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all 5 tenants exist
    let state = client.state().read().await;
    for name in &tenant_names {
        assert!(
            state.tenants.contains_key(name.as_str()),
            "tenant {name} should exist after concurrent creation"
        );
        assert_eq!(state.tenants[name.as_str()].quota.max_nodes, 10);
    }
    assert_eq!(state.tenants.len(), 5);
}

// ─── Test 11: Concurrent node claim conflict ─────────────────
// Two tasks try to claim the same node simultaneously; exactly one succeeds.

#[tokio::test]
async fn concurrent_node_claim_conflict() {
    let client = create_test_quorum().await.unwrap();

    // Register a node first
    let node = NodeBuilder::new().id("contested-node").build();
    client
        .propose(commands::Command::RegisterNode(node))
        .await
        .unwrap();

    let c1 = client.clone();
    let c2 = client.clone();

    let ownership1 = NodeOwnership {
        tenant: "tenant-a".into(),
        vcluster: "vc-a".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some("user-alpha".into()),
        is_borrowed: false,
    };
    let ownership2 = NodeOwnership {
        tenant: "tenant-b".into(),
        vcluster: "vc-b".into(),
        allocation: Uuid::new_v4(),
        claimed_by: Some("user-beta".into()),
        is_borrowed: false,
    };

    let h1 = tokio::spawn(async move {
        c1.claim_node(&"contested-node".to_string(), ownership1)
            .await
    });
    let h2 = tokio::spawn(async move {
        c2.claim_node(&"contested-node".to_string(), ownership2)
            .await
    });

    let r1 = h1.await.unwrap();
    let r2 = h2.await.unwrap();

    // Exactly one should succeed, one should fail
    let successes = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    let failures = [&r1, &r2].iter().filter(|r| r.is_err()).count();
    assert_eq!(
        successes, 1,
        "exactly one claim should succeed, got {} successes",
        successes
    );
    assert_eq!(
        failures, 1,
        "exactly one claim should fail, got {} failures",
        failures
    );

    // Verify the node has exactly one owner
    let got = client
        .get_node(&"contested-node".to_string())
        .await
        .unwrap();
    assert!(got.owner.is_some(), "node should have an owner");
    let owner = got.owner.unwrap();
    assert!(
        owner.claimed_by == Some("user-alpha".into())
            || owner.claimed_by == Some("user-beta".into()),
        "owner should be one of the two claimants"
    );
}

// ─── Test 12: Many operations and state consistency ──────────
// Insert many allocations, verify state is consistent.

#[tokio::test]
#[ignore = "slow: 100+ Raft operations"]
async fn many_operations_trigger_snapshot() {
    let client = create_test_quorum().await.unwrap();

    // Create a tenant with large quota to allow many allocations
    let tenant = TenantBuilder::new("bulk-tenant").max_nodes(10000).build();
    client
        .propose(commands::Command::CreateTenant(tenant))
        .await
        .unwrap();

    // Insert 50 allocations (well above any reasonable snapshot threshold)
    let mut ids = Vec::new();
    for _ in 0..50 {
        let alloc = AllocationBuilder::new().tenant("bulk-tenant").build();
        let id = alloc.id;
        ids.push(id);
        client.insert(alloc).await.unwrap();
    }

    // Move first 25 to Running, then Completed
    for id in &ids[..25] {
        client
            .update_state(id, AllocationState::Running)
            .await
            .unwrap();
        client
            .update_state(id, AllocationState::Completed)
            .await
            .unwrap();
    }

    // Verify all 50 allocations are present in state
    let state = client.state().read().await;
    assert_eq!(
        state.allocations.len(),
        50,
        "all 50 allocations should be in state"
    );

    // Verify the first 25 are Completed
    for id in &ids[..25] {
        let alloc = &state.allocations[id];
        assert_eq!(
            alloc.state,
            AllocationState::Completed,
            "first 25 should be Completed"
        );
        assert!(alloc.completed_at.is_some());
    }

    // Verify the last 25 are still Pending
    for id in &ids[25..] {
        let alloc = &state.allocations[id];
        assert_eq!(
            alloc.state,
            AllocationState::Pending,
            "last 25 should still be Pending"
        );
    }

    // Also register nodes and verify they coexist with allocations
    for i in 0..10 {
        let node = NodeBuilder::new().id(&format!("bulk-node-{i}")).build();
        client
            .propose(commands::Command::RegisterNode(node))
            .await
            .unwrap();
    }

    let state = client.state().read().await;
    assert_eq!(state.nodes.len(), 10, "all 10 nodes should be present");
    assert_eq!(
        state.allocations.len(),
        50,
        "allocations should be unaffected by node registrations"
    );
    assert!(state.tenants.contains_key("bulk-tenant"));
}

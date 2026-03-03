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

    // Record 3 entries
    let entry1 = AuditEntry {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        user: "dr-smith".into(),
        action: AuditAction::NodeClaim,
        details: serde_json::json!({"node": "x1000c0s0b0n0"}),
    };
    let entry2 = AuditEntry {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        user: "dr-jones".into(),
        action: AuditAction::NodeRelease,
        details: serde_json::json!({"node": "x1000c0s0b0n1"}),
    };
    let entry3 = AuditEntry {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        user: "dr-smith".into(),
        action: AuditAction::NodeRelease,
        details: serde_json::json!({"node": "x1000c0s0b0n0"}),
    };

    client.record(entry1).await.unwrap();
    client.record(entry2).await.unwrap();
    client.record(entry3).await.unwrap();

    // Query for dr-smith only
    let smith_entries = client
        .query(&AuditFilter {
            user: Some("dr-smith".into()),
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
            user: Some("dr-jones".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(jones_entries.len(), 1);
    assert_eq!(jones_entries[0].action, AuditAction::NodeRelease);
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

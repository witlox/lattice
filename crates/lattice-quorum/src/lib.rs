//! # lattice-quorum
//!
//! Raft consensus layer for the Lattice scheduler. Manages the authoritative
//! cluster state: node ownership, allocations, tenants, vClusters, topology,
//! and the medical audit log.
//!
//! Built on [openraft](https://docs.rs/openraft) with:
//! - In-memory log + state machine (suitable for testing / single-process)
//! - Channel-based in-process networking for testing
//! - JSON-serialized snapshots
//!
//! ## Architecture (per docs/architecture/system-architecture.md)
//!
//! - **Strong consistency** (Raft-committed): node ownership, medical audit
//! - **Eventually consistent**: job queues, telemetry, quota accounting

pub mod client;
pub mod commands;
pub mod global_state;
pub mod network;
pub mod state_machine;
pub mod store;

use std::io::Cursor;

use crate::commands::{Command, CommandResponse};

// The openraft type configuration for Lattice.
openraft::declare_raft_types!(
    pub TypeConfig:
        D = Command,
        R = CommandResponse,
        NodeId = u64,
        Node = openraft::impls::BasicNode,
        SnapshotData = Cursor<Vec<u8>>,
);

/// Convenience type alias for the Raft instance.
pub type LatticeRaft = openraft::Raft<TypeConfig>;

pub use client::QuorumClient;
pub use commands::{Command as QuorumCommand, CommandResponse as QuorumResponse};
pub use global_state::GlobalState;
pub use network::MemNetworkFactory;
pub use state_machine::LatticeStateMachine;
pub use store::MemLogStore;

/// Create a single-node quorum for testing.
///
/// Returns a `QuorumClient` with a fully initialized single-node Raft cluster.
pub async fn create_test_quorum() -> Result<QuorumClient, anyhow::Error> {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let state = Arc::new(RwLock::new(GlobalState::new()));
    let config = Arc::new(
        openraft::Config {
            heartbeat_interval: 200,
            election_timeout_min: 500,
            election_timeout_max: 1000,
            ..Default::default()
        }
        .validate()?,
    );

    let log_store = MemLogStore::new();
    let sm = LatticeStateMachine::new(Arc::clone(&state));
    let network = MemNetworkFactory::new();

    let raft = openraft::Raft::new(1, config, network.clone(), log_store, sm).await?;

    // Initialize as single-node cluster
    let mut members = BTreeMap::new();
    members.insert(1u64, openraft::impls::BasicNode::new("127.0.0.1:0"));
    raft.initialize(members).await?;

    // Wait for leader election
    raft.wait(None)
        .metrics(|m| m.current_leader == Some(1), "leader elected")
        .await?;

    Ok(QuorumClient::new(raft, state))
}

/// Create a multi-node quorum for testing.
///
/// Returns a vector of `QuorumClient`s for each node.
pub async fn create_test_cluster(node_count: u64) -> Result<Vec<QuorumClient>, anyhow::Error> {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let network_factory = MemNetworkFactory::new();
    let mut clients = Vec::new();
    let mut members = BTreeMap::new();

    for id in 1..=node_count {
        members.insert(
            id,
            openraft::impls::BasicNode::new(format!("127.0.0.1:{}", 5000 + id)),
        );
    }

    for id in 1..=node_count {
        let state = Arc::new(RwLock::new(GlobalState::new()));
        let config = Arc::new(
            openraft::Config {
                heartbeat_interval: 200,
                election_timeout_min: 500,
                election_timeout_max: 1000,
                ..Default::default()
            }
            .validate()?,
        );

        let log_store = MemLogStore::new();
        let sm = LatticeStateMachine::new(Arc::clone(&state));

        let raft = openraft::Raft::new(id, config, network_factory.clone(), log_store, sm).await?;

        network_factory.register(id, raft.clone()).await;
        clients.push(QuorumClient::new(raft, state));
    }

    // Initialize from first node
    clients[0].raft().initialize(members).await?;

    // Wait for leader election
    clients[0]
        .raft()
        .wait(None)
        .metrics(|m| m.current_leader.is_some(), "leader elected")
        .await?;

    Ok(clients)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::traits::{AllocationStore, AuditLog, NodeRegistry};
    use lattice_common::types::*;
    use lattice_test_harness::fixtures::{AllocationBuilder, NodeBuilder, TenantBuilder};
    use uuid::Uuid;

    #[tokio::test]
    async fn single_node_quorum_works() {
        let client = create_test_quorum().await.unwrap();

        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        client.insert(alloc).await.unwrap();

        let got = client.get(&id).await.unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.state, AllocationState::Pending);
    }

    #[tokio::test]
    async fn three_node_cluster_works() {
        let clients = create_test_cluster(3).await.unwrap();
        let leader = &clients[0];

        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        leader.insert(alloc).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let got = leader.get(&id).await.unwrap();
        assert_eq!(got.id, id);
    }

    #[tokio::test]
    async fn node_registry_via_quorum() {
        let client = create_test_quorum().await.unwrap();

        let node = NodeBuilder::new().id("x1000c0s0b0n0").build();
        client
            .propose(commands::Command::RegisterNode(node))
            .await
            .unwrap();

        let got = client.get_node(&"x1000c0s0b0n0".to_string()).await.unwrap();
        assert_eq!(got.id, "x1000c0s0b0n0");
        assert_eq!(got.state, NodeState::Ready);
    }

    #[tokio::test]
    async fn audit_log_via_quorum() {
        use lattice_common::traits::{AuditAction, AuditFilter};

        let client = create_test_quorum().await.unwrap();

        let entry = lattice_common::traits::AuditEntry {
            id: Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user: "dr-smith".into(),
            action: AuditAction::NodeClaim,
            details: serde_json::json!({"node": "x1000c0s0b0n0"}),
        };

        client.record(entry).await.unwrap();

        let results = client
            .query(&AuditFilter {
                user: Some("dr-smith".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, AuditAction::NodeClaim);
    }

    #[tokio::test]
    async fn quota_enforcement_via_quorum() {
        let client = create_test_quorum().await.unwrap();

        let tenant = TenantBuilder::new("limited")
            .max_nodes(1)
            .max_concurrent(1)
            .build();
        client
            .propose(commands::Command::CreateTenant(tenant))
            .await
            .unwrap();

        let alloc1 = AllocationBuilder::new().tenant("limited").build();
        client.insert(alloc1).await.unwrap();

        let alloc2 = AllocationBuilder::new().tenant("limited").build();
        let result = client.insert(alloc2).await;
        assert!(result.is_err());
    }
}

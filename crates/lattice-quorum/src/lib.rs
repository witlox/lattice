//! # lattice-quorum
//!
//! Raft consensus layer for the Lattice scheduler. Manages the authoritative
//! cluster state: node ownership, allocations, tenants, vClusters, topology,
//! and the sensitive audit log.
//!
//! Built on [openraft](https://docs.rs/openraft) with:
//! - In-memory log + state machine (suitable for testing / single-process)
//! - Channel-based in-process networking for testing
//! - JSON-serialized snapshots
//!
//! ## Architecture (per docs/architecture/system-architecture.md)
//!
//! - **Strong consistency** (Raft-committed): node ownership, sensitive audit
//! - **Eventually consistent**: job queues, telemetry, quota accounting

pub mod backup;
pub mod client;
pub mod commands;
pub mod global_state;
pub mod log_store_variant;
pub mod network;
pub mod persistent_store;
pub mod state_machine;
pub mod store;
pub mod transport;
pub mod transport_server;

/// Generated protobuf types for the Raft transport service.
pub mod proto {
    pub mod raft {
        tonic::include_proto!("lattice.v1.raft");
    }
    pub use raft::*;
}

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

pub use backup::{export_backup, restore_backup, verify_backup, BackupMetadata};
pub use client::QuorumClient;
pub use commands::{Command as QuorumCommand, CommandResponse as QuorumResponse};
pub use global_state::GlobalState;
pub use log_store_variant::LogStoreVariant;
pub use network::MemNetworkFactory;
pub use persistent_store::FileLogStore;
pub use state_machine::LatticeStateMachine;
pub use store::MemLogStore;
pub use transport::GrpcNetworkFactory;
pub use transport_server::RaftTransportServer;

/// Create a quorum from configuration.
///
/// - If `peers` is empty and `data_dir` is None → single-node in-memory quorum (test/dev mode).
/// - If `peers` is non-empty → multi-node gRPC-based quorum with real Raft transport.
/// - If `data_dir` is set → uses persistent file-backed log store and snapshots.
///
/// Returns `(QuorumClient, Option<JoinHandle>)`. The handle is the Raft
/// transport server task; `None` for single-node mode.
pub async fn create_quorum_from_config(
    config: &lattice_common::config::QuorumConfig,
) -> Result<(QuorumClient, Option<tokio::task::JoinHandle<()>>), anyhow::Error> {
    if config.peers.is_empty() && config.data_dir.is_none() {
        // Single-node in-memory mode — delegate to test quorum
        let client = create_test_quorum().await?;
        return Ok((client, None));
    }

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let node_id = config.node_id;

    let network_factory = GrpcNetworkFactory::new();
    let mut members = BTreeMap::new();

    // Register self
    members.insert(
        node_id,
        openraft::impls::BasicNode::new(config.raft_listen_address.clone()),
    );
    network_factory
        .register(node_id, config.raft_listen_address.clone())
        .await;

    // Register peers
    for peer in &config.peers {
        members.insert(
            peer.id,
            openraft::impls::BasicNode::new(peer.address.clone()),
        );
        network_factory
            .register(peer.id, peer.address.clone())
            .await;
    }

    let state = Arc::new(RwLock::new(GlobalState::new()));

    // Build log store and state machine based on data_dir
    let (log_store, sm) = if let Some(ref data_dir) = config.data_dir {
        let snapshot_dir = data_dir.join("raft").join("snapshots");
        let file_store = FileLogStore::new(data_dir)?;
        let sm = LatticeStateMachine::with_snapshot_dir(Arc::clone(&state), snapshot_dir)?;
        (LogStoreVariant::File(file_store), sm)
    } else {
        (
            LogStoreVariant::Memory(MemLogStore::new()),
            LatticeStateMachine::new(Arc::clone(&state)),
        )
    };

    let mut raft_config_builder = openraft::Config {
        heartbeat_interval: config.heartbeat_interval_ms,
        election_timeout_min: config.election_timeout_ms,
        election_timeout_max: config.election_timeout_ms * 2,
        ..Default::default()
    };

    // Enable automatic snapshotting when persistent storage is configured
    if config.data_dir.is_some() {
        raft_config_builder.snapshot_policy =
            openraft::SnapshotPolicy::LogsSinceLast(config.snapshot_threshold);
    }

    let raft_config = Arc::new(raft_config_builder.validate()?);

    let raft =
        openraft::Raft::new(node_id, raft_config, network_factory.clone(), log_store, sm).await?;

    // Start Raft transport server
    let listener = tokio::net::TcpListener::bind(&config.raft_listen_address).await?;
    let server = RaftTransportServer::new(raft.clone());
    let handle = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let _ = tonic::transport::Server::builder()
            .add_service(proto::raft_service_server::RaftServiceServer::new(server))
            .serve_with_incoming(incoming)
            .await;
    });

    // Give server time to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Node 1 initializes the cluster; others wait for membership
    if node_id == 1 {
        raft.initialize(members).await?;
    }

    // Wait for leader election (with timeout)
    raft.wait(Some(std::time::Duration::from_secs(10)))
        .metrics(|m| m.current_leader.is_some(), "leader elected")
        .await?;

    Ok((QuorumClient::new(raft, state), Some(handle)))
}

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

/// Create a multi-node gRPC-based quorum for testing.
///
/// Starts real tonic servers on localhost with random ports and connects nodes
/// via `GrpcNetworkFactory`. Returns `(Vec<QuorumClient>, Vec<JoinHandle>)`.
/// The join handles are the server tasks (drop to shut down).
pub async fn create_test_grpc_cluster(
    node_count: u64,
) -> Result<
    (
        Vec<QuorumClient>,
        Vec<tokio::task::JoinHandle<()>>,
        Vec<String>,
    ),
    anyhow::Error,
> {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // Bind listeners first to get assigned ports
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for _ in 0..node_count {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        addresses.push(addr.to_string());
        listeners.push(listener);
    }

    let network_factory = GrpcNetworkFactory::new();
    let mut members = BTreeMap::new();
    let mut clients = Vec::new();
    let mut server_handles = Vec::new();

    for (i, addr) in addresses.iter().enumerate() {
        let id = (i + 1) as u64;
        members.insert(id, openraft::impls::BasicNode::new(addr.clone()));
        network_factory.register(id, addr.clone()).await;
    }

    for (i, listener) in listeners.into_iter().enumerate() {
        let id = (i + 1) as u64;
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

        // Start tonic server
        let server = RaftTransportServer::new(raft.clone());
        let handle = tokio::spawn(async move {
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            let _ = tonic::transport::Server::builder()
                .add_service(proto::raft_service_server::RaftServiceServer::new(server))
                .serve_with_incoming(incoming)
                .await;
        });
        server_handles.push(handle);

        clients.push(QuorumClient::new(raft, state));
    }

    // Give servers time to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Initialize from first node
    clients[0].raft().initialize(members).await?;

    // Wait for leader election
    clients[0]
        .raft()
        .wait(None)
        .metrics(|m| m.current_leader.is_some(), "leader elected")
        .await?;

    Ok((clients, server_handles, addresses))
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
    #[ignore = "slow: spins up 3-node Raft cluster"]
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
    async fn from_config_no_peers() {
        use lattice_common::config::QuorumConfig;
        let config = QuorumConfig::default();
        let (client, handle) = create_quorum_from_config(&config).await.unwrap();
        assert!(
            handle.is_none(),
            "single-node mode should return None handle"
        );

        // Verify it works
        let node = NodeBuilder::new().id("cfg-test").build();
        client
            .propose(commands::Command::RegisterNode(node))
            .await
            .unwrap();
        let got = client.get_node(&"cfg-test".to_string()).await.unwrap();
        assert_eq!(got.id, "cfg-test");
    }

    // Note: The multi-node config test is covered by `grpc_three_node_cluster_*`
    // tests which use the lower-level `create_test_grpc_cluster`. The
    // `create_quorum_from_config` multi-node path uses the same underlying
    // mechanisms (GrpcNetworkFactory + RaftTransportServer) so we test it
    // indirectly. A full integration test requires Docker (see tests/e2e/).

    #[tokio::test]
    #[ignore = "slow: spins up 3-node gRPC Raft cluster"]
    async fn grpc_three_node_cluster_leader_election() {
        let (clients, handles, _addrs) = create_test_grpc_cluster(3).await.unwrap();

        // Verify leader elected (already waited during cluster creation)
        // Write a value to prove the cluster is functional
        let node = NodeBuilder::new().id("grpc-test-node").build();
        clients[0]
            .propose(commands::Command::RegisterNode(node))
            .await
            .unwrap();

        let got = clients[0]
            .get_node(&"grpc-test-node".to_string())
            .await
            .unwrap();
        assert_eq!(got.id, "grpc-test-node");

        for h in handles {
            h.abort();
        }
    }

    #[tokio::test]
    #[ignore = "slow: spins up 3-node gRPC Raft cluster"]
    async fn grpc_three_node_cluster_log_replication() {
        let (clients, handles, _addrs) = create_test_grpc_cluster(3).await.unwrap();

        // Write through leader
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        clients[0].insert(alloc).await.unwrap();

        // Give time for replication
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Read from leader's state
        let got = clients[0].get(&id).await.unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.state, AllocationState::Pending);

        // Verify state replicated to followers
        for client in &clients[1..] {
            let state = client.state().read().await;
            assert!(
                state.allocations.contains_key(&id),
                "Allocation should be replicated to all nodes"
            );
        }

        for h in handles {
            h.abort();
        }
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

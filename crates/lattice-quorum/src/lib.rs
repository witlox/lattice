//! # lattice-quorum
//!
//! Raft consensus layer for the Lattice scheduler. Manages the authoritative
//! cluster state: node ownership, allocations, tenants, vClusters, topology,
//! and the sensitive audit log.
//!
//! Built on [raft-hpc-core](https://crates.io/crates/raft-hpc-core) for the
//! generic Raft infrastructure (log stores, state machine, transport, backup),
//! with Lattice-specific state and commands.
//!
//! ## Architecture (per docs/architecture/system-architecture.md)
//!
//! - **Strong consistency** (Raft-committed): node ownership, sensitive audit
//! - **Eventually consistent**: job queues, telemetry, quota accounting

pub mod backup;
pub mod client;
pub mod commands;
pub mod global_state;

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

/// Type alias for the Lattice state machine backed by raft-hpc-core.
pub type LatticeStateMachine =
    raft_hpc_core::HpcStateMachine<TypeConfig, global_state::GlobalState>;

// Re-export raft-hpc-core types parameterized for Lattice's TypeConfig
pub type MemLogStore = raft_hpc_core::MemLogStore<TypeConfig>;
pub type FileLogStore = raft_hpc_core::FileLogStore<TypeConfig>;
pub type LogStoreVariant = raft_hpc_core::LogStoreVariant<TypeConfig>;
pub type GrpcNetworkFactory = raft_hpc_core::GrpcNetworkFactory;
pub type MemNetworkFactory = raft_hpc_core::MemNetworkFactory<TypeConfig>;
pub type RaftTransportServer = raft_hpc_core::RaftTransportServer<TypeConfig>;
pub type PeerTlsConfig = raft_hpc_core::PeerTlsConfig;

pub use backup::{export_backup, restore_backup, verify_backup, BackupMetadata};
pub use client::QuorumClient;
pub use commands::{Command as QuorumCommand, CommandResponse as QuorumResponse};
pub use global_state::GlobalState;

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

    let raft: LatticeRaft =
        openraft::Raft::new(node_id, raft_config, network_factory.clone(), log_store, sm).await?;

    // Start Raft transport server
    let listener = tokio::net::TcpListener::bind(&config.raft_listen_address).await?;
    let server = RaftTransportServer::new(raft.clone());
    let handle = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let _ = tonic::transport::Server::builder()
            .add_service(raft_hpc_core::proto::raft_service_server::RaftServiceServer::new(server))
            .serve_with_incoming(incoming)
            .await;
    });

    // Give server time to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Initialize the Raft cluster only when explicitly requested via the
    // bootstrap flag.  This MUST only be set on the very first startup of the
    // first node.  Subsequent restarts skip initialization — the persisted
    // Raft state (WAL + snapshots) is sufficient to rejoin the cluster.
    if config.bootstrap {
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

        let raft: LatticeRaft =
            openraft::Raft::new(id, config, network_factory.clone(), log_store, sm).await?;

        // Start tonic server
        let server = RaftTransportServer::new(raft.clone());
        let handle = tokio::spawn(async move {
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            let _ = tonic::transport::Server::builder()
                .add_service(
                    raft_hpc_core::proto::raft_service_server::RaftServiceServer::new(server),
                )
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
    use lattice_common::traits::{AllocationStore, NodeRegistry};
    use lattice_common::types::*;
    use lattice_test_harness::fixtures::{AllocationBuilder, NodeBuilder};

    // Note: single_node_quorum, three_node_cluster, node_registry, audit_log,
    // and quota_enforcement are covered more thoroughly by tests/integration.rs.
    // These tests cover factory functions and gRPC clusters only.

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

    /// Verify that a persistent single-node quorum can be bootstrapped, stopped,
    /// and restarted without the bootstrap flag — the exact crash-loop scenario
    /// that was broken before the fix.
    #[tokio::test]
    async fn persistent_restart_without_bootstrap_succeeds() {
        use lattice_common::config::QuorumConfig;

        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener); // release port so quorum can bind

        // First boot: bootstrap=true to initialize
        let config = QuorumConfig {
            node_id: 1,
            data_dir: Some(dir.path().to_path_buf()),
            raft_listen_address: addr.clone(),
            bootstrap: true,
            ..Default::default()
        };
        let (client, handle) = create_quorum_from_config(&config).await.unwrap();

        // Write something so the Raft log is non-empty
        let node = NodeBuilder::new().id("persist-test").build();
        client
            .propose(commands::Command::RegisterNode(node))
            .await
            .unwrap();

        // Shut down
        drop(client);
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Second boot: bootstrap=false (simulating systemd restart).
        // Before the fix, this would fail with "not allowed to initialize".
        let config2 = QuorumConfig {
            node_id: 1,
            data_dir: Some(dir.path().to_path_buf()),
            raft_listen_address: addr,
            bootstrap: false,
            ..Default::default()
        };
        let result = create_quorum_from_config(&config2).await;
        assert!(
            result.is_ok(),
            "Restart without bootstrap must succeed; got: {:?}",
            result.err()
        );

        let (client2, handle2) = result.unwrap();
        drop(client2);
        if let Some(h) = handle2 {
            h.abort();
        }
    }

    /// Verify that re-bootstrapping an already-initialized cluster fails
    /// (the Raft engine rejects double-init).
    #[tokio::test]
    async fn re_bootstrap_existing_cluster_fails() {
        use lattice_common::config::QuorumConfig;

        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);

        // First boot: bootstrap
        let config = QuorumConfig {
            node_id: 1,
            data_dir: Some(dir.path().to_path_buf()),
            raft_listen_address: addr.clone(),
            bootstrap: true,
            ..Default::default()
        };
        let (client, handle) = create_quorum_from_config(&config).await.unwrap();

        // Write data so log is non-empty
        let node = NodeBuilder::new().id("double-init").build();
        client
            .propose(commands::Command::RegisterNode(node))
            .await
            .unwrap();

        drop(client);
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Second boot with bootstrap=true again — should fail
        let config2 = QuorumConfig {
            node_id: 1,
            data_dir: Some(dir.path().to_path_buf()),
            raft_listen_address: addr,
            bootstrap: true,
            ..Default::default()
        };
        let result = create_quorum_from_config(&config2).await;
        assert!(
            result.is_err(),
            "Re-bootstrapping an existing cluster must fail"
        );
    }

    #[tokio::test]
    #[ignore = "slow: spins up 3-node gRPC Raft cluster"]
    async fn grpc_three_node_cluster_leader_election() {
        let (clients, handles, _addrs) = create_test_grpc_cluster(3).await.unwrap();

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

        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        clients[0].insert(alloc).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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
}

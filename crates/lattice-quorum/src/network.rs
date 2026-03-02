//! Raft network implementation.
//!
//! For in-process testing, provides a channel-based network.
//! Production would use gRPC (via tonic) for Raft message transport.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use openraft::error::{RPCError, StreamingError, Unreachable};
use openraft::network::v2::RaftNetworkV2;
use openraft::network::{Backoff, RPCOption, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::storage::Snapshot;
use openraft::Raft;
use tokio::sync::RwLock;

use crate::TypeConfig;

type VoteOf = openraft::vote::Vote<TypeConfig>;

/// A network factory that routes messages through in-memory channels.
/// Each node registers its `Raft` handle, and messages are dispatched directly.
#[derive(Clone)]
pub struct MemNetworkFactory {
    pub routers: Arc<RwLock<HashMap<u64, Raft<TypeConfig>>>>,
}

impl MemNetworkFactory {
    pub fn new() -> Self {
        Self {
            routers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(&self, id: u64, raft: Raft<TypeConfig>) {
        let mut routers = self.routers.write().await;
        routers.insert(id, raft);
    }
}

impl Default for MemNetworkFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl RaftNetworkFactory<TypeConfig> for MemNetworkFactory {
    type Network = MemNetwork;

    async fn new_client(
        &mut self,
        target: u64,
        _node: &openraft::impls::BasicNode,
    ) -> Self::Network {
        MemNetwork {
            target,
            routers: Arc::clone(&self.routers),
        }
    }
}

/// A network connection to a specific peer node, routing through the in-memory router table.
pub struct MemNetwork {
    target: u64,
    routers: Arc<RwLock<HashMap<u64, Raft<TypeConfig>>>>,
}

impl RaftNetworkV2<TypeConfig> for MemNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<TypeConfig>, RPCError<TypeConfig>> {
        let routers = self.routers.read().await;
        let raft = routers.get(&self.target).ok_or_else(|| {
            RPCError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::NotConnected,
                format!("Node {} not found in router table", self.target),
            )))
        })?;

        raft.append_entries(rpc)
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        let routers = self.routers.read().await;
        let raft = routers.get(&self.target).ok_or_else(|| {
            RPCError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::NotConnected,
                format!("Node {} not found in router table", self.target),
            )))
        })?;

        raft.vote(rpc)
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))
    }

    async fn full_snapshot(
        &mut self,
        vote: VoteOf,
        snapshot: Snapshot<TypeConfig>,
        _cancel: impl futures::Future<Output = openraft::errors::ReplicationClosed> + Send + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<TypeConfig>, StreamingError<TypeConfig>> {
        let routers = self.routers.read().await;
        let raft = routers.get(&self.target).ok_or_else(|| {
            StreamingError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::NotConnected,
                format!("Node {} not found in router table", self.target),
            )))
        })?;

        raft.install_full_snapshot(vote, snapshot)
            .await
            .map_err(|e| StreamingError::Unreachable(Unreachable::new(&e)))
    }

    fn backoff(&self) -> Backoff {
        Backoff::new(std::iter::repeat(std::time::Duration::from_millis(100)))
    }
}

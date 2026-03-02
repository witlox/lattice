//! gRPC-based Raft network transport for multi-process clusters.
//!
//! Implements openraft's `RaftNetworkFactory` and `RaftNetworkV2` traits
//! using tonic gRPC for inter-node communication. Each Raft message is
//! JSON-serialized into a bytes payload and transported via the `RaftService` proto.

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
use tokio::sync::RwLock;
use tonic::transport::Channel;
use tracing::{debug, warn};

use crate::proto::raft_service_client::RaftServiceClient;
use crate::proto::{RaftPayload, SnapshotRequest};
use crate::TypeConfig;

type VoteOf = openraft::vote::Vote<TypeConfig>;

/// Maps node IDs to their gRPC addresses for connection management.
#[derive(Clone)]
pub struct GrpcNetworkFactory {
    addresses: Arc<RwLock<HashMap<u64, String>>>,
}

impl GrpcNetworkFactory {
    pub fn new() -> Self {
        Self {
            addresses: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a node's address for gRPC connections.
    pub async fn register(&self, id: u64, address: String) {
        let mut addrs = self.addresses.write().await;
        addrs.insert(id, address);
    }
}

impl Default for GrpcNetworkFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl RaftNetworkFactory<TypeConfig> for GrpcNetworkFactory {
    type Network = GrpcNetwork;

    async fn new_client(
        &mut self,
        target: u64,
        node: &openraft::impls::BasicNode,
    ) -> Self::Network {
        // Prefer the address from the BasicNode (which comes from cluster membership),
        // fall back to the registered address.
        let address = {
            let addr = node.addr.clone();
            if addr.is_empty() {
                let addrs = self.addresses.read().await;
                addrs.get(&target).cloned().unwrap_or_default()
            } else {
                addr
            }
        };

        GrpcNetwork {
            target,
            address,
            client: None,
        }
    }
}

/// A gRPC connection to a specific Raft peer node.
pub struct GrpcNetwork {
    target: u64,
    address: String,
    client: Option<RaftServiceClient<Channel>>,
}

impl GrpcNetwork {
    async fn get_client(
        &mut self,
    ) -> Result<&mut RaftServiceClient<Channel>, RPCError<TypeConfig>> {
        if self.client.is_none() {
            let endpoint = if self.address.starts_with("http") {
                self.address.clone()
            } else {
                format!("http://{}", self.address)
            };

            debug!("Connecting to Raft peer {} at {}", self.target, endpoint);

            let channel = Channel::from_shared(endpoint)
                .map_err(|e| {
                    RPCError::Unreachable(Unreachable::new(&io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid address for node {}: {}", self.target, e),
                    )))
                })?
                .connect()
                .await
                .map_err(|e| {
                    RPCError::Unreachable(Unreachable::new(&io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("Cannot connect to node {}: {}", self.target, e),
                    )))
                })?;

            self.client = Some(RaftServiceClient::new(channel));
        }

        Ok(self.client.as_mut().unwrap())
    }

    /// Reset the connection so the next call reconnects.
    fn reset_client(&mut self) {
        self.client = None;
    }
}

impl RaftNetworkV2<TypeConfig> for GrpcNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<TypeConfig>, RPCError<TypeConfig>> {
        let data = serde_json::to_vec(&rpc).map_err(|e| {
            RPCError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize AppendEntries: {e}"),
            )))
        })?;

        let client = self.get_client().await?;
        let resp = client
            .append_entries(RaftPayload { data })
            .await
            .map_err(|e| {
                warn!("AppendEntries RPC to node {} failed: {}", self.target, e);
                self.reset_client();
                RPCError::Unreachable(Unreachable::new(&io::Error::other(format!(
                    "AppendEntries RPC failed: {e}"
                ))))
            })?;

        serde_json::from_slice(&resp.into_inner().data).map_err(|e| {
            RPCError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize AppendEntries response: {e}"),
            )))
        })
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        let data = serde_json::to_vec(&rpc).map_err(|e| {
            RPCError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize Vote: {e}"),
            )))
        })?;

        let client = self.get_client().await?;
        let resp = client.vote(RaftPayload { data }).await.map_err(|e| {
            warn!("Vote RPC to node {} failed: {}", self.target, e);
            self.reset_client();
            RPCError::Unreachable(Unreachable::new(&io::Error::other(format!(
                "Vote RPC failed: {e}"
            ))))
        })?;

        serde_json::from_slice(&resp.into_inner().data).map_err(|e| {
            RPCError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize Vote response: {e}"),
            )))
        })
    }

    async fn full_snapshot(
        &mut self,
        vote: VoteOf,
        snapshot: Snapshot<TypeConfig>,
        _cancel: impl futures::Future<Output = openraft::errors::ReplicationClosed> + Send + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<TypeConfig>, StreamingError<TypeConfig>> {
        let vote_bytes = serde_json::to_vec(&vote).map_err(|e| {
            StreamingError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize vote: {e}"),
            )))
        })?;

        let meta_bytes = serde_json::to_vec(&snapshot.meta).map_err(|e| {
            StreamingError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to serialize snapshot meta: {e}"),
            )))
        })?;

        let snapshot_data = snapshot.snapshot.into_inner();

        let client = self.get_client().await.map_err(|e| match e {
            RPCError::Unreachable(u) => StreamingError::Unreachable(u),
            _ => StreamingError::Unreachable(Unreachable::new(&io::Error::other(
                "Connection failed",
            ))),
        })?;

        let resp = client
            .install_snapshot(SnapshotRequest {
                vote: vote_bytes,
                meta: meta_bytes,
                snapshot_data,
            })
            .await
            .map_err(|e| {
                warn!("InstallSnapshot RPC to node {} failed: {}", self.target, e);
                self.reset_client();
                StreamingError::Unreachable(Unreachable::new(&io::Error::other(format!(
                    "InstallSnapshot RPC failed: {e}"
                ))))
            })?;

        serde_json::from_slice(&resp.into_inner().data).map_err(|e| {
            StreamingError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize snapshot response: {e}"),
            )))
        })
    }

    fn backoff(&self) -> Backoff {
        Backoff::new(std::iter::repeat(std::time::Duration::from_millis(200)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::raft::{AppendEntriesRequest, VoteRequest};

    #[test]
    fn append_entries_request_roundtrip() {
        let req: AppendEntriesRequest<TypeConfig> = AppendEntriesRequest {
            vote: openraft::vote::Vote::new(1, 1),
            prev_log_id: None,
            entries: vec![],
            leader_commit: None,
        };
        let data = serde_json::to_vec(&req).unwrap();
        let decoded: AppendEntriesRequest<TypeConfig> = serde_json::from_slice(&data).unwrap();
        assert_eq!(decoded.vote, req.vote);
    }

    #[test]
    fn vote_request_roundtrip() {
        let req: VoteRequest<TypeConfig> = VoteRequest {
            vote: openraft::vote::Vote::new(2, 3),
            last_log_id: None,
        };
        let data = serde_json::to_vec(&req).unwrap();
        let decoded: VoteRequest<TypeConfig> = serde_json::from_slice(&data).unwrap();
        assert_eq!(decoded.vote, req.vote);
    }

    #[test]
    fn grpc_network_factory_default() {
        let factory = GrpcNetworkFactory::default();
        assert!(Arc::strong_count(&factory.addresses) == 1);
    }

    #[tokio::test]
    async fn register_peer_address() {
        let factory = GrpcNetworkFactory::new();
        factory.register(1, "127.0.0.1:9000".to_string()).await;
        factory.register(2, "127.0.0.1:9001".to_string()).await;

        let addrs = factory.addresses.read().await;
        assert_eq!(addrs.get(&1).unwrap(), "127.0.0.1:9000");
        assert_eq!(addrs.get(&2).unwrap(), "127.0.0.1:9001");
    }
}

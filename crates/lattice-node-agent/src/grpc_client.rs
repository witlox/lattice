//! gRPC clients for connecting the node agent to the quorum.
//!
//! Provides `GrpcHeartbeatSink` (implements `HeartbeatSink`) and
//! `GrpcNodeRegistry` (implements `NodeRegistry`) that forward
//! calls over gRPC to the Lattice API server.

use std::time::Duration;

use async_trait::async_trait;
use tonic::transport::Channel;
use tracing::{debug, warn};

use lattice_common::error::LatticeError;
use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::node_service_client::NodeServiceClient;
use lattice_common::traits::{NodeFilter, NodeRegistry};
use lattice_common::types::{Node, NodeCapabilities, NodeId, NodeOwnership, NodeState};

use crate::heartbeat::Heartbeat;
use crate::heartbeat_loop::HeartbeatSink;

// ─── GrpcHeartbeatSink ──────────────────────────────────────────────────────

/// Delivers heartbeats to the quorum via the `NodeService.Heartbeat` RPC.
pub struct GrpcHeartbeatSink {
    client: NodeServiceClient<Channel>,
}

impl GrpcHeartbeatSink {
    /// Connect to the quorum endpoint with retry.
    pub async fn connect(endpoint: &str) -> Result<Self, String> {
        let channel = connect_with_retry(endpoint).await?;
        Ok(Self {
            client: NodeServiceClient::new(channel),
        })
    }
}

#[async_trait]
impl HeartbeatSink for GrpcHeartbeatSink {
    async fn send(&self, heartbeat: Heartbeat) -> Result<(), String> {
        let req = pb::HeartbeatRequest {
            node_id: heartbeat.node_id,
            healthy: heartbeat.healthy,
            issues: heartbeat.issues,
            running_allocations: heartbeat.running_allocations,
            conformance_fingerprint: heartbeat.conformance_fingerprint.unwrap_or_default(),
            sequence: heartbeat.sequence,
        };

        let mut client = self.client.clone();
        let resp = client
            .heartbeat(req)
            .await
            .map_err(|e| format!("heartbeat RPC failed: {e}"))?;

        if resp.into_inner().accepted {
            debug!("heartbeat accepted");
            Ok(())
        } else {
            Err("heartbeat rejected by server".to_string())
        }
    }
}

// ─── GrpcNodeRegistry ───────────────────────────────────────────────────────

/// Implements `NodeRegistry` by forwarding calls to the quorum's `NodeService`.
///
/// Only agent-relevant methods are implemented: `get_node`, `list_nodes`,
/// `update_node_state`. `claim_node`/`release_node` return `Unimplemented`
/// since only the scheduler calls those.
pub struct GrpcNodeRegistry {
    client: NodeServiceClient<Channel>,
}

impl GrpcNodeRegistry {
    /// Connect to the quorum endpoint with retry.
    pub async fn connect(endpoint: &str) -> Result<Self, String> {
        let channel = connect_with_retry(endpoint).await?;
        Ok(Self {
            client: NodeServiceClient::new(channel),
        })
    }

    /// Register this node with the quorum.
    pub async fn register_node(
        &self,
        node_id: &str,
        capabilities: &NodeCapabilities,
    ) -> Result<(), String> {
        let req = pb::RegisterNodeRequest {
            node_id: node_id.to_string(),
            gpu_type: capabilities.gpu_type.clone().unwrap_or_default(),
            gpu_count: capabilities.gpu_count,
            cpu_cores: capabilities.cpu_cores,
            memory_gb: capabilities.memory_gb,
            features: capabilities.features.clone(),
        };

        let mut client = self.client.clone();
        client
            .register_node(req)
            .await
            .map_err(|e| format!("register_node RPC failed: {e}"))?;

        Ok(())
    }
}

#[async_trait]
impl NodeRegistry for GrpcNodeRegistry {
    async fn get_node(&self, id: &NodeId) -> Result<Node, LatticeError> {
        let mut client = self.client.clone();
        let resp = client
            .get_node(pb::GetNodeRequest {
                node_id: id.clone(),
            })
            .await
            .map_err(|e| LatticeError::Internal(format!("get_node RPC failed: {e}")))?;

        Ok(node_from_status(resp.into_inner()))
    }

    async fn list_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, LatticeError> {
        let mut client = self.client.clone();
        let resp = client
            .list_nodes(pb::ListNodesRequest {
                state: filter
                    .state
                    .as_ref()
                    .map(|s| format!("{s:?}").to_lowercase())
                    .unwrap_or_default(),
                group: filter.group.map(|g| g.to_string()).unwrap_or_default(),
                tenant: filter.tenant.clone().unwrap_or_default(),
                vcluster: String::new(),
                limit: 0,
                cursor: String::new(),
            })
            .await
            .map_err(|e| LatticeError::Internal(format!("list_nodes RPC failed: {e}")))?;

        Ok(resp
            .into_inner()
            .nodes
            .into_iter()
            .map(node_from_status)
            .collect())
    }

    async fn update_node_state(&self, id: &NodeId, state: NodeState) -> Result<(), LatticeError> {
        let mut client = self.client.clone();
        match state {
            NodeState::Draining => {
                client
                    .drain_node(pb::DrainNodeRequest {
                        node_id: id.clone(),
                        reason: String::new(),
                    })
                    .await
                    .map_err(|e| LatticeError::Internal(format!("drain_node RPC failed: {e}")))?;
            }
            NodeState::Ready => {
                client
                    .undrain_node(pb::UndrainNodeRequest {
                        node_id: id.clone(),
                    })
                    .await
                    .map_err(|e| LatticeError::Internal(format!("undrain_node RPC failed: {e}")))?;
            }
            NodeState::Down { reason } => {
                client
                    .disable_node(pb::DisableNodeRequest {
                        node_id: id.clone(),
                        reason,
                    })
                    .await
                    .map_err(|e| LatticeError::Internal(format!("disable_node RPC failed: {e}")))?;
            }
            _ => {
                return Err(LatticeError::Internal(format!(
                    "unsupported state transition via gRPC: {state:?}"
                )));
            }
        }
        Ok(())
    }

    async fn claim_node(
        &self,
        _id: &NodeId,
        _ownership: NodeOwnership,
    ) -> Result<(), LatticeError> {
        Err(LatticeError::Internal(
            "claim_node not supported from node agent".into(),
        ))
    }

    async fn release_node(&self, _id: &NodeId) -> Result<(), LatticeError> {
        Err(LatticeError::Internal(
            "release_node not supported from node agent".into(),
        ))
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Connect to a gRPC endpoint with exponential backoff retry.
async fn connect_with_retry(endpoint: &str) -> Result<Channel, String> {
    let mut delay = Duration::from_millis(100);
    let max_delay = Duration::from_secs(5);
    let max_attempts = 10;

    for attempt in 1..=max_attempts {
        match Channel::from_shared(endpoint.to_string())
            .map_err(|e| format!("invalid endpoint: {e}"))?
            .connect()
            .await
        {
            Ok(channel) => return Ok(channel),
            Err(e) => {
                warn!(
                    attempt,
                    max_attempts,
                    error = %e,
                    "failed to connect to quorum, retrying in {:?}",
                    delay
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(max_delay);
            }
        }
    }

    Err(format!(
        "failed to connect to {endpoint} after {max_attempts} attempts"
    ))
}

/// Convert a protobuf `NodeStatus` to a domain `Node`.
fn node_from_status(status: pb::NodeStatus) -> Node {
    Node {
        id: status.node_id,
        state: match status.state.as_str() {
            "ready" => NodeState::Ready,
            "draining" => NodeState::Draining,
            "drained" => NodeState::Drained,
            "degraded" => NodeState::Degraded {
                reason: status.state_reason.clone(),
            },
            "down" => NodeState::Down {
                reason: status.state_reason.clone(),
            },
            "booting" => NodeState::Booting,
            "failed" => NodeState::Failed {
                reason: status.state_reason.clone(),
            },
            _ => NodeState::Ready,
        },
        capabilities: NodeCapabilities {
            gpu_type: if status.gpu_type.is_empty() {
                None
            } else {
                Some(status.gpu_type)
            },
            gpu_count: status.gpu_count,
            cpu_cores: status.cpu_cores,
            memory_gb: status.memory_gb,
            features: status.features,
            gpu_topology: None,
        },
        group: status.group,
        owner: None,
        conformance_fingerprint: if status.conformance_fingerprint.is_empty() {
            None
        } else {
            Some(status.conformance_fingerprint)
        },
        last_heartbeat: status.last_heartbeat.map(|ts| {
            chrono::DateTime::from_timestamp(ts.seconds, ts.nanos as u32).unwrap_or_default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_from_status_ready() {
        let status = pb::NodeStatus {
            node_id: "n1".to_string(),
            state: "ready".to_string(),
            gpu_type: "GH200".to_string(),
            gpu_count: 4,
            cpu_cores: 72,
            memory_gb: 512,
            ..Default::default()
        };
        let node = node_from_status(status);
        assert_eq!(node.id, "n1");
        assert_eq!(node.state, NodeState::Ready);
        assert_eq!(node.capabilities.gpu_type.as_deref(), Some("GH200"));
    }

    #[test]
    fn node_from_status_degraded() {
        let status = pb::NodeStatus {
            node_id: "n2".to_string(),
            state: "degraded".to_string(),
            state_reason: "high temps".to_string(),
            ..Default::default()
        };
        let node = node_from_status(status);
        assert!(matches!(node.state, NodeState::Degraded { .. }));
    }

    #[test]
    fn node_from_status_empty_gpu_type_is_none() {
        let status = pb::NodeStatus {
            node_id: "n3".to_string(),
            state: "ready".to_string(),
            ..Default::default()
        };
        let node = node_from_status(status);
        assert!(node.capabilities.gpu_type.is_none());
    }
}

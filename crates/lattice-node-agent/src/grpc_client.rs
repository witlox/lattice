//! gRPC clients for connecting the node agent to the quorum.
//!
//! Provides `GrpcHeartbeatSink` (implements `HeartbeatSink`) and
//! `GrpcNodeRegistry` (implements `NodeRegistry`) that forward
//! calls over gRPC to the Lattice API server.

use std::time::Duration;

use async_trait::async_trait;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, warn};

use lattice_common::error::LatticeError;
use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::node_service_client::NodeServiceClient;
use lattice_common::traits::{NodeFilter, NodeRegistry};
use lattice_common::types::{
    MemoryDomain, MemoryDomainType, MemoryInterconnect, MemoryLinkType, MemoryTopology, Node,
    NodeCapabilities, NodeId, NodeOwnership, NodeState,
};

use crate::heartbeat::Heartbeat;
use crate::heartbeat_loop::HeartbeatSink;

// ─── Auth interceptor ──────────────────────────────────────────────────────

/// Injects a Bearer token into outgoing gRPC requests.
///
/// Read from `LATTICE_AGENT_TOKEN` env var at construction time.
/// When no token is set, requests pass through without auth metadata.
#[derive(Clone)]
pub struct AgentAuthInterceptor {
    token: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
}

impl AgentAuthInterceptor {
    /// Create an interceptor from an optional token string.
    pub fn new(token: Option<&str>) -> Self {
        Self {
            token: token.and_then(|t| {
                format!("Bearer {t}")
                    .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                    .ok()
            }),
        }
    }

    /// Create from the `LATTICE_AGENT_TOKEN` environment variable.
    pub fn from_env() -> Self {
        Self::new(std::env::var("LATTICE_AGENT_TOKEN").ok().as_deref())
    }
}

impl tonic::service::Interceptor for AgentAuthInterceptor {
    fn call(&mut self, mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(ref token) = self.token {
            req.metadata_mut().insert("authorization", token.clone());
        }
        Ok(req)
    }
}

/// Type alias for an authenticated NodeService client.
type AuthNodeServiceClient = NodeServiceClient<InterceptedService<Channel, AgentAuthInterceptor>>;

// ─── GrpcHeartbeatSink ──────────────────────────────────────────────────────

/// Delivers heartbeats to the quorum via the `NodeService.Heartbeat` RPC.
pub struct GrpcHeartbeatSink {
    client: AuthNodeServiceClient,
}

impl GrpcHeartbeatSink {
    /// Connect to the quorum endpoint with retry.
    ///
    /// When `tls` is provided, the channel uses mTLS (production path).
    /// Otherwise, falls back to `LATTICE_AGENT_TOKEN` Bearer token auth.
    pub async fn connect(
        endpoint: &str,
        tls: Option<tonic::transport::ClientTlsConfig>,
    ) -> Result<Self, String> {
        let channel = connect_with_retry(endpoint, tls).await?;
        let interceptor = AgentAuthInterceptor::from_env();
        Ok(Self {
            client: NodeServiceClient::with_interceptor(channel, interceptor),
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
            owner_version: heartbeat.owner_version,
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
    client: AuthNodeServiceClient,
}

impl GrpcNodeRegistry {
    /// Connect to the quorum endpoint with retry.
    ///
    /// When `tls` is provided, the channel uses mTLS (production path).
    /// Otherwise, falls back to `LATTICE_AGENT_TOKEN` Bearer token auth.
    pub async fn connect(
        endpoint: &str,
        tls: Option<tonic::transport::ClientTlsConfig>,
    ) -> Result<Self, String> {
        let channel = connect_with_retry(endpoint, tls).await?;
        let interceptor = AgentAuthInterceptor::from_env();
        Ok(Self {
            client: NodeServiceClient::with_interceptor(channel, interceptor),
        })
    }

    /// Register this node with the quorum.
    pub async fn register_node(
        &self,
        node_id: &str,
        capabilities: &NodeCapabilities,
    ) -> Result<(), String> {
        let (memory_domains, memory_interconnects, total_memory_capacity_bytes) =
            match &capabilities.memory_topology {
                Some(topo) => memory_topology_to_proto(topo),
                None => (vec![], vec![], 0),
            };
        let req = pb::RegisterNodeRequest {
            node_id: node_id.to_string(),
            gpu_type: capabilities.gpu_type.clone().unwrap_or_default(),
            gpu_count: capabilities.gpu_count,
            cpu_cores: capabilities.cpu_cores,
            memory_gb: capabilities.memory_gb,
            features: capabilities.features.clone(),
            memory_domains,
            memory_interconnects,
            total_memory_capacity_bytes,
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
///
/// When `tls` is provided, configures mutual TLS on the channel (production
/// path: SPIRE/bootstrap certs). Without TLS, connects plaintext (the
/// `AgentAuthInterceptor` handles Bearer token auth as fallback).
async fn connect_with_retry(
    endpoint: &str,
    tls: Option<tonic::transport::ClientTlsConfig>,
) -> Result<Channel, String> {
    let mut delay = Duration::from_millis(100);
    let max_delay = Duration::from_secs(5);
    let max_attempts = 10;

    for attempt in 1..=max_attempts {
        let mut channel_builder = Channel::from_shared(endpoint.to_string())
            .map_err(|e| format!("invalid endpoint: {e}"))?;

        if let Some(ref tls_config) = tls {
            channel_builder = channel_builder
                .tls_config(tls_config.clone())
                .map_err(|e| format!("TLS config error: {e}"))?;
        }

        match channel_builder.connect().await {
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
            memory_topology: memory_topology_from_proto(
                &status.memory_domains,
                &status.memory_interconnects,
                status.total_memory_capacity_bytes,
            ),
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
        owner_version: 0,
    }
}

// ─── Memory topology proto conversions ──────────────────────────────────────

fn memory_topology_to_proto(
    topo: &MemoryTopology,
) -> (
    Vec<pb::MemoryDomainProto>,
    Vec<pb::MemoryInterconnectProto>,
    u64,
) {
    let domains = topo
        .domains
        .iter()
        .map(|d| pb::MemoryDomainProto {
            id: d.id,
            domain_type: match d.domain_type {
                MemoryDomainType::Dram => "dram",
                MemoryDomainType::Hbm => "hbm",
                MemoryDomainType::CxlAttached => "cxl_attached",
                MemoryDomainType::Unified => "unified",
            }
            .to_string(),
            capacity_bytes: d.capacity_bytes,
            numa_node: d.numa_node,
            attached_cpus: d.attached_cpus.clone(),
            attached_gpus: d.attached_gpus.clone(),
        })
        .collect();
    let interconnects = topo
        .interconnects
        .iter()
        .map(|i| pb::MemoryInterconnectProto {
            domain_a: i.domain_a,
            domain_b: i.domain_b,
            link_type: match i.link_type {
                MemoryLinkType::NumaLink => "numa_link",
                MemoryLinkType::CxlSwitch => "cxl_switch",
                MemoryLinkType::CoherentFabric => "coherent_fabric",
            }
            .to_string(),
            bandwidth_gbps: i.bandwidth_gbps,
            latency_ns: i.latency_ns,
        })
        .collect();
    (domains, interconnects, topo.total_capacity_bytes)
}

fn memory_topology_from_proto(
    domains: &[pb::MemoryDomainProto],
    interconnects: &[pb::MemoryInterconnectProto],
    total_bytes: u64,
) -> Option<MemoryTopology> {
    if domains.is_empty() {
        return None;
    }
    Some(MemoryTopology {
        domains: domains
            .iter()
            .map(|d| MemoryDomain {
                id: d.id,
                domain_type: match d.domain_type.as_str() {
                    "hbm" => MemoryDomainType::Hbm,
                    "cxl_attached" => MemoryDomainType::CxlAttached,
                    "unified" => MemoryDomainType::Unified,
                    _ => MemoryDomainType::Dram,
                },
                capacity_bytes: d.capacity_bytes,
                numa_node: d.numa_node,
                attached_cpus: d.attached_cpus.clone(),
                attached_gpus: d.attached_gpus.clone(),
            })
            .collect(),
        interconnects: interconnects
            .iter()
            .map(|i| MemoryInterconnect {
                domain_a: i.domain_a,
                domain_b: i.domain_b,
                link_type: match i.link_type.as_str() {
                    "cxl_switch" => MemoryLinkType::CxlSwitch,
                    "coherent_fabric" => MemoryLinkType::CoherentFabric,
                    _ => MemoryLinkType::NumaLink,
                },
                bandwidth_gbps: i.bandwidth_gbps,
                latency_ns: i.latency_ns,
            })
            .collect(),
        total_capacity_bytes: total_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_interceptor_injects_bearer_token() {
        let interceptor = AgentAuthInterceptor::new(Some("test-token-123"));
        let mut interceptor = interceptor.clone();
        let req = tonic::Request::new(());
        let req = tonic::service::Interceptor::call(&mut interceptor, req).unwrap();
        let auth = req
            .metadata()
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(auth, "Bearer test-token-123");
    }

    #[test]
    fn auth_interceptor_passthrough_without_token() {
        let interceptor = AgentAuthInterceptor::new(None);
        let mut interceptor = interceptor.clone();
        let req = tonic::Request::new(());
        let req = tonic::service::Interceptor::call(&mut interceptor, req).unwrap();
        assert!(req.metadata().get("authorization").is_none());
    }

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
    fn node_from_status_with_memory_topology() {
        let status = pb::NodeStatus {
            node_id: "n4".to_string(),
            state: "ready".to_string(),
            memory_domains: vec![
                pb::MemoryDomainProto {
                    id: 0,
                    domain_type: "dram".to_string(),
                    capacity_bytes: 64 * 1024 * 1024 * 1024,
                    numa_node: Some(0),
                    attached_cpus: vec![0, 1, 2, 3],
                    attached_gpus: vec![],
                },
                pb::MemoryDomainProto {
                    id: 1,
                    domain_type: "hbm".to_string(),
                    capacity_bytes: 32 * 1024 * 1024 * 1024,
                    numa_node: None,
                    attached_cpus: vec![],
                    attached_gpus: vec![0, 1],
                },
            ],
            memory_interconnects: vec![pb::MemoryInterconnectProto {
                domain_a: 0,
                domain_b: 1,
                link_type: "coherent_fabric".to_string(),
                bandwidth_gbps: 900.0,
                latency_ns: 100,
            }],
            total_memory_capacity_bytes: 96 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        let node = node_from_status(status);
        let topo = node.capabilities.memory_topology.unwrap();
        assert_eq!(topo.domains.len(), 2);
        assert_eq!(topo.domains[0].domain_type, MemoryDomainType::Dram);
        assert_eq!(topo.domains[1].domain_type, MemoryDomainType::Hbm);
        assert_eq!(topo.interconnects.len(), 1);
        assert_eq!(
            topo.interconnects[0].link_type,
            MemoryLinkType::CoherentFabric
        );
        assert_eq!(topo.total_capacity_bytes, 96 * 1024 * 1024 * 1024);
    }

    #[test]
    fn node_from_status_no_memory_topology() {
        let status = pb::NodeStatus {
            node_id: "n5".to_string(),
            state: "ready".to_string(),
            ..Default::default()
        };
        let node = node_from_status(status);
        assert!(node.capabilities.memory_topology.is_none());
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

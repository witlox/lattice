//! NodeService gRPC implementation.
//!
//! Implements the 5 RPCs defined in nodes.proto.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::node_service_server::NodeService;
use lattice_common::types::{Node, NodeCapabilities, NodeState};

use crate::convert;
use crate::state::ApiState;

/// NodeService backed by trait-object NodeRegistry.
pub struct LatticeNodeService {
    state: Arc<ApiState>,
}

impl LatticeNodeService {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl NodeService for LatticeNodeService {
    async fn list_nodes(
        &self,
        request: Request<pb::ListNodesRequest>,
    ) -> Result<Response<pb::ListNodesResponse>, Status> {
        let req = request.into_inner();
        let filter = convert::node_filter_from_list_request(&req);

        let nodes = self
            .state
            .nodes
            .list_nodes(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let statuses: Vec<pb::NodeStatus> = nodes.iter().map(convert::node_to_status).collect();

        Ok(Response::new(pb::ListNodesResponse {
            nodes: statuses,
            next_cursor: String::new(),
        }))
    }

    async fn get_node(
        &self,
        request: Request<pb::GetNodeRequest>,
    ) -> Result<Response<pb::NodeStatus>, Status> {
        let req = request.into_inner();

        let node = self
            .state
            .nodes
            .get_node(&req.node_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(convert::node_to_status(&node)))
    }

    async fn drain_node(
        &self,
        request: Request<pb::DrainNodeRequest>,
    ) -> Result<Response<pb::DrainNodeResponse>, Status> {
        let req = request.into_inner();

        self.state
            .nodes
            .update_node_state(&req.node_id, NodeState::Draining)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::DrainNodeResponse {
            success: true,
            active_allocations: 0, // Would be computed from allocation store
        }))
    }

    async fn undrain_node(
        &self,
        request: Request<pb::UndrainNodeRequest>,
    ) -> Result<Response<pb::UndrainNodeResponse>, Status> {
        let req = request.into_inner();

        self.state
            .nodes
            .update_node_state(&req.node_id, NodeState::Ready)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::UndrainNodeResponse { success: true }))
    }

    async fn disable_node(
        &self,
        request: Request<pb::DisableNodeRequest>,
    ) -> Result<Response<pb::DisableNodeResponse>, Status> {
        let req = request.into_inner();

        self.state
            .nodes
            .update_node_state(&req.node_id, NodeState::Down { reason: req.reason })
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::DisableNodeResponse { success: true }))
    }

    async fn register_node(
        &self,
        request: Request<pb::RegisterNodeRequest>,
    ) -> Result<Response<pb::RegisterNodeResponse>, Status> {
        let req = request.into_inner();

        let node = Node {
            id: req.node_id.clone(),
            state: NodeState::Ready,
            capabilities: NodeCapabilities {
                gpu_type: if req.gpu_type.is_empty() {
                    None
                } else {
                    Some(req.gpu_type)
                },
                gpu_count: req.gpu_count,
                cpu_cores: req.cpu_cores,
                memory_gb: req.memory_gb,
                features: req.features,
                gpu_topology: None,
            },
            group: 0,
            owner: None,
            conformance_fingerprint: None,
            last_heartbeat: None,
        };

        // Propose through Raft if quorum is available
        if let Some(ref quorum) = self.state.quorum {
            quorum
                .propose(lattice_quorum::QuorumCommand::RegisterNode(node))
                .await
                .map_err(|e| Status::internal(format!("raft propose failed: {e}")))?;
        } else {
            return Err(Status::unavailable("quorum not available"));
        }

        tracing::info!(node_id = %req.node_id, "node registered via gRPC");
        Ok(Response::new(pb::RegisterNodeResponse { success: true }))
    }

    async fn heartbeat(
        &self,
        request: Request<pb::HeartbeatRequest>,
    ) -> Result<Response<pb::HeartbeatResponse>, Status> {
        let req = request.into_inner();

        if let Some(ref quorum) = self.state.quorum {
            quorum
                .propose(lattice_quorum::QuorumCommand::RecordHeartbeat {
                    id: req.node_id.clone(),
                    timestamp: chrono::Utc::now(),
                })
                .await
                .map_err(|e| Status::internal(format!("raft propose failed: {e}")))?;
        } else {
            return Err(Status::unavailable("quorum not available"));
        }

        Ok(Response::new(pb::HeartbeatResponse { accepted: true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::create_node_batch;
    use lattice_test_harness::mocks::{
        MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
    };

    fn test_state(nodes: usize) -> Arc<ApiState> {
        let node_batch = create_node_batch(nodes, 0);
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new().with_nodes(node_batch)),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: None,
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
            sovra: None,
            pty: None,
        })
    }

    #[tokio::test]
    async fn list_all_nodes() {
        let state = test_state(5);
        let svc = LatticeNodeService::new(state);

        let resp = svc
            .list_nodes(Request::new(pb::ListNodesRequest::default()))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().nodes.len(), 5);
    }

    #[tokio::test]
    async fn get_single_node() {
        let state = test_state(3);
        let svc = LatticeNodeService::new(state);

        let resp = svc
            .get_node(Request::new(pb::GetNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().node_id, "x1000c0s0b0n0");
        assert_eq!(resp.get_ref().state, "ready");
    }

    #[tokio::test]
    async fn get_node_not_found() {
        let state = test_state(1);
        let svc = LatticeNodeService::new(state);

        let result = svc
            .get_node(Request::new(pb::GetNodeRequest {
                node_id: "nonexistent".to_string(),
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn drain_node_succeeds() {
        let state = test_state(3);
        let svc = LatticeNodeService::new(state);

        let resp = svc
            .drain_node(Request::new(pb::DrainNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
                reason: "maintenance".to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);
    }

    async fn test_state_with_quorum() -> Arc<ApiState> {
        let quorum = lattice_quorum::create_test_quorum().await.unwrap();
        let quorum = Arc::new(quorum);
        Arc::new(ApiState {
            allocations: quorum.clone(),
            nodes: quorum.clone(),
            audit: quorum.clone(),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: Some(quorum),
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
            sovra: None,
            pty: None,
        })
    }

    #[tokio::test]
    async fn register_node_via_raft() {
        let state = test_state_with_quorum().await;
        let svc = LatticeNodeService::new(state.clone());

        let resp = svc
            .register_node(Request::new(pb::RegisterNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
                gpu_type: "GH200".to_string(),
                gpu_count: 4,
                cpu_cores: 72,
                memory_gb: 512,
                features: vec!["slingshot".to_string()],
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);

        // Verify node is now in the registry
        let got = svc
            .get_node(Request::new(pb::GetNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(got.get_ref().node_id, "x1000c0s0b0n0");
        assert_eq!(got.get_ref().gpu_type, "GH200");
    }

    #[tokio::test]
    async fn heartbeat_via_raft() {
        let state = test_state_with_quorum().await;
        let svc = LatticeNodeService::new(state);

        // First register the node
        svc.register_node(Request::new(pb::RegisterNodeRequest {
            node_id: "hb-test-node".to_string(),
            gpu_type: "".to_string(),
            gpu_count: 0,
            cpu_cores: 64,
            memory_gb: 256,
            features: vec![],
        }))
        .await
        .unwrap();

        // Send heartbeat
        let resp = svc
            .heartbeat(Request::new(pb::HeartbeatRequest {
                node_id: "hb-test-node".to_string(),
                healthy: true,
                issues: vec![],
                running_allocations: 2,
                conformance_fingerprint: "abc123".to_string(),
                sequence: 1,
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().accepted);
    }

    #[tokio::test]
    async fn register_node_fails_without_quorum() {
        let state = test_state(1);
        let svc = LatticeNodeService::new(state);

        let result = svc
            .register_node(Request::new(pb::RegisterNodeRequest {
                node_id: "no-quorum".to_string(),
                ..Default::default()
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn undrain_node_succeeds() {
        let state = test_state(3);
        let svc = LatticeNodeService::new(state);

        // First drain, then undrain
        svc.drain_node(Request::new(pb::DrainNodeRequest {
            node_id: "x1000c0s0b0n0".to_string(),
            reason: "maintenance".to_string(),
        }))
        .await
        .unwrap();

        let resp = svc
            .undrain_node(Request::new(pb::UndrainNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);
    }
}

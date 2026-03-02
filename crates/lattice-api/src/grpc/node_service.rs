//! NodeService gRPC implementation.
//!
//! Implements the 5 RPCs defined in nodes.proto.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::node_service_server::NodeService;
use lattice_common::types::NodeState;

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

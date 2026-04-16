//! NodeService gRPC implementation.
//!
//! Implements the RPCs defined in nodes.proto.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::node_service_server::NodeService;
use lattice_common::types::{Node, NodeCapabilities, NodeState};

use crate::convert::memory_topology_from_proto;

use crate::convert;
use crate::state::ApiState;

/// Reject syntactically invalid agent addresses (INV-D1). Matches
/// unroutable placeholders: empty host, port 0, or wildcards.
fn is_trivially_invalid_address(addr: &str) -> bool {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some((host, port)) = trimmed.rsplit_once(':') else {
        return true;
    };
    if host.is_empty() {
        return true;
    }
    match port.parse::<u16>() {
        Ok(0) => true,
        Ok(_) => {
            // Disallow unroutable hosts as construction-invalid.
            matches!(host, "0.0.0.0" | "::" | "[::]")
        }
        Err(_) => true,
    }
}

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

        // Count active (non-terminal) allocations assigned to this node.
        let all_allocs = self
            .state
            .allocations
            .list(&Default::default())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let active_count = all_allocs
            .iter()
            .filter(|a| a.assigned_nodes.contains(&req.node_id) && !a.state.is_terminal())
            .count() as u32;

        // Always transition Ready → Draining first (valid state machine edge).
        self.state
            .nodes
            .update_node_state(&req.node_id, NodeState::Draining)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        // If no active allocations, immediately complete: Draining → Drained.
        // Otherwise the scheduler loop will transition to Drained once all
        // allocations finish.
        if active_count == 0 {
            self.state
                .nodes
                .update_node_state(&req.node_id, NodeState::Drained)
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
        }

        Ok(Response::new(pb::DrainNodeResponse {
            success: true,
            active_allocations: active_count,
        }))
    }

    async fn undrain_node(
        &self,
        request: Request<pb::UndrainNodeRequest>,
    ) -> Result<Response<pb::UndrainNodeResponse>, Status> {
        let req = request.into_inner();

        // Check current state — undrain is only valid from Drained.
        // Draining → Ready is not a valid state machine transition;
        // the drain must complete first.
        let node = self
            .state
            .nodes
            .get_node(&req.node_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        if !matches!(node.state, NodeState::Drained) {
            return Err(Status::failed_precondition(format!(
                "Cannot undrain node in state {:?} — node must be in Drained state (drain must complete first)",
                node.state
            )));
        }

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

    async fn enable_node(
        &self,
        request: Request<pb::EnableNodeRequest>,
    ) -> Result<Response<pb::EnableNodeResponse>, Status> {
        let req = request.into_inner();

        self.state
            .nodes
            .update_node_state(&req.node_id, NodeState::Ready)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::EnableNodeResponse { success: true }))
    }

    async fn register_node(
        &self,
        request: Request<pb::RegisterNodeRequest>,
    ) -> Result<Response<pb::RegisterNodeResponse>, Status> {
        let req = request.into_inner();

        // INV-D1: agent_address required + non-trivial validation.
        if req.agent_address.trim().is_empty() {
            tracing::warn!(node_id = %req.node_id, "RegisterNode rejected: empty agent_address");
            return Ok(Response::new(pb::RegisterNodeResponse {
                success: false,
                reason: "agent_address_required".into(),
            }));
        }
        if is_trivially_invalid_address(&req.agent_address) {
            tracing::warn!(
                node_id = %req.node_id,
                addr = %req.agent_address,
                "RegisterNode rejected: syntactically invalid agent_address"
            );
            return Ok(Response::new(pb::RegisterNodeResponse {
                success: false,
                reason: "agent_address_invalid".into(),
            }));
        }

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
                memory_topology: memory_topology_from_proto(
                    &req.memory_domains,
                    &req.memory_interconnects,
                    req.total_memory_capacity_bytes,
                ),
            },
            group: 0,
            owner: None,
            conformance_fingerprint: None,
            last_heartbeat: None,
            owner_version: 0,
            agent_address: req.agent_address.clone(),
            consecutive_dispatch_failures: 0,
            degraded_at: None,
            reattach_in_progress: false,
            reattach_first_set_at: None,
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

        tracing::info!(
            node_id = %req.node_id,
            agent_address = %req.agent_address,
            "node registered via gRPC"
        );
        Ok(Response::new(pb::RegisterNodeResponse {
            success: true,
            reason: String::new(),
        }))
    }

    async fn update_node_address(
        &self,
        request: Request<pb::UpdateNodeAddressRequest>,
    ) -> Result<Response<pb::UpdateNodeAddressResponse>, Status> {
        let req = request.into_inner();
        if req.new_address.trim().is_empty() {
            return Ok(Response::new(pb::UpdateNodeAddressResponse {
                success: false,
                reason: "agent_address_required".into(),
            }));
        }
        if is_trivially_invalid_address(&req.new_address) {
            return Ok(Response::new(pb::UpdateNodeAddressResponse {
                success: false,
                reason: "agent_address_invalid".into(),
            }));
        }

        if let Some(ref quorum) = self.state.quorum {
            quorum
                .propose(lattice_quorum::QuorumCommand::UpdateNodeAddress {
                    node_id: req.node_id.clone(),
                    new_address: req.new_address.clone(),
                })
                .await
                .map_err(|e| Status::internal(format!("raft propose failed: {e}")))?;
        } else {
            return Err(Status::unavailable("quorum not available"));
        }

        tracing::info!(
            node_id = %req.node_id,
            new_address = %req.new_address,
            "node agent_address updated"
        );
        Ok(Response::new(pb::UpdateNodeAddressResponse {
            success: true,
            reason: String::new(),
        }))
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
                    owner_version: req.owner_version,
                    reattach_in_progress: req.reattach_in_progress,
                })
                .await
                .map_err(|e| Status::internal(format!("raft propose failed: {e}")))?;

            // Apply any Completion Reports carried on this heartbeat.
            // IP-03 / IP-14 / INV-D12-D7-D4 validation happens at apply-step
            // inside ApplyCompletionReport's handler.
            for report in req.completion_reports.iter() {
                let phase = match pb::CompletionPhase::try_from(report.phase) {
                    Ok(pb::CompletionPhase::Staging) => {
                        lattice_common::types::CompletionPhase::Staging
                    }
                    Ok(pb::CompletionPhase::Running) => {
                        lattice_common::types::CompletionPhase::Running
                    }
                    Ok(pb::CompletionPhase::Completed) => {
                        lattice_common::types::CompletionPhase::Completed
                    }
                    Ok(pb::CompletionPhase::Failed) => {
                        lattice_common::types::CompletionPhase::Failed
                    }
                    _ => continue, // unspecified/unknown: skip silently
                };
                let alloc_id = match uuid::Uuid::parse_str(&report.allocation_id) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let _ = quorum
                    .propose(lattice_quorum::QuorumCommand::ApplyCompletionReport {
                        node_id: req.node_id.clone(),
                        allocation_id: alloc_id,
                        phase,
                        pid: report.pid,
                        exit_code: report.exit_code,
                        reason: report.reason.clone(),
                    })
                    .await;
            }
        } else {
            return Err(Status::unavailable("quorum not available"));
        }

        Ok(Response::new(pb::HeartbeatResponse { accepted: true }))
    }

    async fn health(
        &self,
        _request: Request<pb::HealthRequest>,
    ) -> Result<Response<pb::HealthResponse>, Status> {
        Ok(Response::new(pb::HealthResponse {
            status: "healthy".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: 0, // Would be computed from server start time
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::traits::AllocationStore;
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
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
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
    async fn drain_node_with_no_allocations_goes_to_drained() {
        let state = test_state(3);
        let svc = LatticeNodeService::new(state.clone());

        let resp = svc
            .drain_node(Request::new(pb::DrainNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
                reason: "maintenance".to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);
        assert_eq!(resp.get_ref().active_allocations, 0);

        // Should be Drained (not stuck in Draining)
        let node = state
            .nodes
            .get_node(&"x1000c0s0b0n0".to_string())
            .await
            .unwrap();
        assert!(
            matches!(node.state, NodeState::Drained),
            "Expected Drained, got {:?}",
            node.state
        );
    }

    #[tokio::test]
    async fn drain_node_with_active_allocations_stays_draining() {
        use lattice_common::types::AllocationState;
        use lattice_test_harness::fixtures::AllocationBuilder;

        let node_batch = create_node_batch(3, 0);
        let mut alloc_with_node = AllocationBuilder::new().tenant("t1").build();

        // Insert a running allocation assigned to node 0
        alloc_with_node.assigned_nodes = vec!["x1000c0s0b0n0".to_string()];
        alloc_with_node.state = AllocationState::Running;

        let alloc_store = MockAllocationStore::new();
        alloc_store.insert(alloc_with_node).await.unwrap();

        let state = Arc::new(ApiState {
            allocations: Arc::new(alloc_store),
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
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
        });

        let svc = LatticeNodeService::new(state.clone());

        let resp = svc
            .drain_node(Request::new(pb::DrainNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
                reason: "maintenance".to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);
        assert_eq!(resp.get_ref().active_allocations, 1);

        // Should be Draining (waiting for allocation to complete)
        let node = state
            .nodes
            .get_node(&"x1000c0s0b0n0".to_string())
            .await
            .unwrap();
        assert!(
            matches!(node.state, NodeState::Draining),
            "Expected Draining, got {:?}",
            node.state
        );
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
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
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
                agent_address: "10.0.0.1:50052".to_string(),
                ..Default::default()
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
            ..Default::default()
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
                owner_version: 0,
                reattach_in_progress: false,
                completion_reports: Vec::new(),
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
                agent_address: "10.0.0.1:50052".to_string(),
                ..Default::default()
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn undrain_drained_node_succeeds() {
        let state = test_state(3);
        let svc = LatticeNodeService::new(state.clone());

        // Drain (no allocations → goes directly to Drained)
        svc.drain_node(Request::new(pb::DrainNodeRequest {
            node_id: "x1000c0s0b0n0".to_string(),
            reason: "maintenance".to_string(),
        }))
        .await
        .unwrap();

        // Now undrain from Drained → Ready
        let resp = svc
            .undrain_node(Request::new(pb::UndrainNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);

        // Verify back to Ready
        let node = state
            .nodes
            .get_node(&"x1000c0s0b0n0".to_string())
            .await
            .unwrap();
        assert!(
            matches!(node.state, NodeState::Ready),
            "Expected Ready, got {:?}",
            node.state
        );
    }

    #[tokio::test]
    async fn undrain_draining_node_fails() {
        use lattice_common::types::AllocationState;
        use lattice_test_harness::fixtures::AllocationBuilder;

        let node_batch = create_node_batch(3, 0);
        let mut alloc = AllocationBuilder::new().tenant("t1").build();
        alloc.assigned_nodes = vec!["x1000c0s0b0n0".to_string()];
        alloc.state = AllocationState::Running;

        let alloc_store = MockAllocationStore::new();
        alloc_store.insert(alloc).await.unwrap();

        let state = Arc::new(ApiState {
            allocations: Arc::new(alloc_store),
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
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
        });

        let svc = LatticeNodeService::new(state);

        // Drain with active allocations → stays in Draining
        svc.drain_node(Request::new(pb::DrainNodeRequest {
            node_id: "x1000c0s0b0n0".to_string(),
            reason: "maintenance".to_string(),
        }))
        .await
        .unwrap();

        // Undrain while Draining should fail
        let result = svc
            .undrain_node(Request::new(pb::UndrainNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn enable_node_from_down_succeeds() {
        let state = test_state(3);
        let svc = LatticeNodeService::new(state);

        // First disable, then enable
        svc.disable_node(Request::new(pb::DisableNodeRequest {
            node_id: "x1000c0s0b0n0".to_string(),
            reason: "maintenance".to_string(),
        }))
        .await
        .unwrap();

        let resp = svc
            .enable_node(Request::new(pb::EnableNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);

        // Verify node is back to ready
        let got = svc
            .get_node(Request::new(pb::GetNodeRequest {
                node_id: "x1000c0s0b0n0".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(got.get_ref().state, "ready");
    }
}

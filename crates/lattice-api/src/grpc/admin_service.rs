//! AdminService gRPC implementation.
//!
//! Implements the 6 RPCs defined in admin.proto.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::admin_service_server::AdminService;

use crate::convert;
use crate::state::ApiState;

/// AdminService backed by trait-object stores.
///
/// Tenant and VCluster operations are stored in-memory for now;
/// in production they would go through the quorum state machine.
pub struct LatticeAdminService {
    #[allow(dead_code)] // Used when wired to quorum
    state: Arc<ApiState>,
    tenants: tokio::sync::RwLock<Vec<lattice_common::types::Tenant>>,
    vclusters: tokio::sync::RwLock<Vec<lattice_common::types::VCluster>>,
}

impl LatticeAdminService {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self {
            state,
            tenants: tokio::sync::RwLock::new(Vec::new()),
            vclusters: tokio::sync::RwLock::new(Vec::new()),
        }
    }
}

#[tonic::async_trait]
impl AdminService for LatticeAdminService {
    async fn create_tenant(
        &self,
        request: Request<pb::CreateTenantRequest>,
    ) -> Result<Response<pb::TenantResponse>, Status> {
        let req = request.into_inner();

        if req.name.is_empty() {
            return Err(Status::invalid_argument("tenant name is required"));
        }

        let tenant = convert::tenant_from_create(&req);
        let resp = convert::tenant_to_response(&tenant);

        self.tenants.write().await.push(tenant);

        Ok(Response::new(resp))
    }

    async fn update_tenant(
        &self,
        request: Request<pb::UpdateTenantRequest>,
    ) -> Result<Response<pb::TenantResponse>, Status> {
        let req = request.into_inner();
        let mut tenants = self.tenants.write().await;

        let tenant = tenants
            .iter_mut()
            .find(|t| t.id == req.tenant_id)
            .ok_or_else(|| Status::not_found(format!("tenant {} not found", req.tenant_id)))?;

        if let Some(quota) = &req.quota {
            tenant.quota = lattice_common::types::TenantQuota {
                max_nodes: quota.max_nodes,
                fair_share_target: quota.fair_share_target,
                gpu_hours_budget: quota.gpu_hours_budget,
                max_concurrent_allocations: quota.max_concurrent_allocations,
            };
        }

        if let Some(ref level) = req.isolation_level {
            tenant.isolation_level = match level.as_str() {
                "strict" => lattice_common::types::IsolationLevel::Strict,
                _ => lattice_common::types::IsolationLevel::Standard,
            };
        }

        let resp = convert::tenant_to_response(tenant);
        Ok(Response::new(resp))
    }

    async fn create_v_cluster(
        &self,
        request: Request<pb::CreateVClusterRequest>,
    ) -> Result<Response<pb::VClusterResponse>, Status> {
        let req = request.into_inner();

        if req.name.is_empty() {
            return Err(Status::invalid_argument("vcluster name is required"));
        }

        if req.tenant_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id is required"));
        }

        let vc = convert::vcluster_from_create(&req);
        let resp = convert::vcluster_to_response(&vc);

        self.vclusters.write().await.push(vc);

        Ok(Response::new(resp))
    }

    async fn update_v_cluster(
        &self,
        request: Request<pb::UpdateVClusterRequest>,
    ) -> Result<Response<pb::VClusterResponse>, Status> {
        let req = request.into_inner();
        let mut vclusters = self.vclusters.write().await;

        let vc = vclusters
            .iter_mut()
            .find(|v| v.id == req.vcluster_id)
            .ok_or_else(|| Status::not_found(format!("vcluster {} not found", req.vcluster_id)))?;

        if let Some(weights) = &req.cost_weights {
            vc.cost_weights = lattice_common::types::CostWeights {
                priority: weights.priority,
                wait_time: weights.wait_time,
                fair_share: weights.fair_share,
                topology: weights.topology,
                data_readiness: weights.data_readiness,
                backlog: weights.backlog,
                energy: weights.energy,
                checkpoint_efficiency: weights.checkpoint_efficiency,
                conformance: weights.conformance,
            };
        }

        let resp = convert::vcluster_to_response(vc);
        Ok(Response::new(resp))
    }

    async fn get_raft_status(
        &self,
        _request: Request<pb::GetRaftStatusRequest>,
    ) -> Result<Response<pb::RaftStatusResponse>, Status> {
        // Raft status would come from the quorum module.
        // For now, return a placeholder indicating a single-node cluster.
        Ok(Response::new(pb::RaftStatusResponse {
            leader_id: 0,
            current_term: 1,
            last_applied: 0,
            commit_index: 0,
            members: vec![pb::RaftMemberStatus {
                node_id: 0,
                address: "127.0.0.1:50051".to_string(),
                role: "leader".to_string(),
                match_index: 0,
                last_contact: None,
            }],
        }))
    }

    async fn backup_verify(
        &self,
        request: Request<pb::BackupVerifyRequest>,
    ) -> Result<Response<pb::BackupVerifyResponse>, Status> {
        let req = request.into_inner();

        if req.backup_path.is_empty() {
            return Err(Status::invalid_argument("backup_path is required"));
        }

        // Backup verification would check snapshot integrity.
        // For now, return a placeholder.
        Ok(Response::new(pb::BackupVerifyResponse {
            valid: false,
            message: "backup verification not yet implemented".to_string(),
            backup_timestamp: None,
            snapshot_term: 0,
            snapshot_index: 0,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::mocks::{
        MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
    };

    fn test_state() -> Arc<ApiState> {
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new()),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
        })
    }

    #[tokio::test]
    async fn create_and_update_tenant() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let resp = svc
            .create_tenant(Request::new(pb::CreateTenantRequest {
                name: "physics".to_string(),
                quota: Some(pb::TenantQuotaSpec {
                    max_nodes: 50,
                    fair_share_target: 0.3,
                    gpu_hours_budget: None,
                    max_concurrent_allocations: None,
                }),
                isolation_level: "standard".to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().name, "physics");
        assert_eq!(resp.get_ref().quota.as_ref().unwrap().max_nodes, 50);

        // Update quota
        let resp = svc
            .update_tenant(Request::new(pb::UpdateTenantRequest {
                tenant_id: "physics".to_string(),
                quota: Some(pb::TenantQuotaSpec {
                    max_nodes: 100,
                    fair_share_target: 0.5,
                    gpu_hours_budget: None,
                    max_concurrent_allocations: None,
                }),
                isolation_level: None,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().quota.as_ref().unwrap().max_nodes, 100);
    }

    #[tokio::test]
    async fn create_tenant_requires_name() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let result = svc
            .create_tenant(Request::new(pb::CreateTenantRequest {
                name: String::new(),
                ..Default::default()
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn create_and_update_vcluster() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        // Create tenant first
        svc.create_tenant(Request::new(pb::CreateTenantRequest {
            name: "physics".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap();

        let resp = svc
            .create_v_cluster(Request::new(pb::CreateVClusterRequest {
                tenant_id: "physics".to_string(),
                name: "hpc-batch".to_string(),
                scheduler_type: "hpc_backfill".to_string(),
                cost_weights: Some(pb::CostWeightsSpec {
                    priority: 0.2,
                    ..Default::default()
                }),
                dedicated_nodes: vec![],
                allow_borrowing: true,
                allow_lending: false,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().name, "hpc-batch");
        assert_eq!(resp.get_ref().scheduler_type, "hpc_backfill");

        // Update cost weights
        let vc_id = resp.get_ref().vcluster_id.clone();
        let resp = svc
            .update_v_cluster(Request::new(pb::UpdateVClusterRequest {
                vcluster_id: vc_id,
                cost_weights: Some(pb::CostWeightsSpec {
                    priority: 0.5,
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().cost_weights.as_ref().unwrap().priority, 0.5);
    }

    #[tokio::test]
    async fn get_raft_status() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let resp = svc
            .get_raft_status(Request::new(pb::GetRaftStatusRequest {}))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().leader_id, 0);
        assert!(!resp.get_ref().members.is_empty());
    }

    #[tokio::test]
    async fn backup_verify_requires_path() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let result = svc
            .backup_verify(Request::new(pb::BackupVerifyRequest {
                backup_path: String::new(),
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn update_nonexistent_tenant_fails() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let result = svc
            .update_tenant(Request::new(pb::UpdateTenantRequest {
                tenant_id: "nonexistent".to_string(),
                ..Default::default()
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);
    }
}

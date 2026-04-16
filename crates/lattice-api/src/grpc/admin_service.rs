//! AdminService gRPC implementation.
//!
//! Implements the RPCs defined in admin.proto.
//! Tenant and VCluster mutations are Raft-committed via the quorum
//! when a quorum client is configured, falling back to in-memory storage.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::admin_service_server::AdminService;
use lattice_quorum::commands::{Command, CommandResponse};
use lattice_quorum::QuorumClient;

use crate::convert;
use crate::state::ApiState;

/// AdminService backed by quorum consensus (when available) or in-memory fallback.
pub struct LatticeAdminService {
    state: Arc<ApiState>,
    /// Fallback in-memory storage (used when quorum is None).
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

    fn quorum(&self) -> Option<&Arc<QuorumClient>> {
        self.state.quorum.as_ref()
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

        if let Some(quorum) = self.quorum() {
            let resp = quorum
                .propose(Command::CreateTenant(tenant.clone()))
                .await
                .map_err(|e| Status::internal(format!("quorum error: {e}")))?;
            match resp {
                CommandResponse::TenantId(_) => {}
                CommandResponse::Error(e) => return Err(Status::internal(e)),
                _ => return Err(Status::internal("unexpected response")),
            }
        } else {
            self.tenants.write().await.push(tenant.clone());
        }

        Ok(Response::new(convert::tenant_to_response(&tenant)))
    }

    async fn update_tenant(
        &self,
        request: Request<pb::UpdateTenantRequest>,
    ) -> Result<Response<pb::TenantResponse>, Status> {
        let req = request.into_inner();

        if let Some(quorum) = self.quorum() {
            let quota = req.quota.map(|q| lattice_common::types::TenantQuota {
                max_nodes: q.max_nodes,
                fair_share_target: q.fair_share_target,
                gpu_hours_budget: q.gpu_hours_budget,
                node_hours_budget: q.node_hours_budget,
                max_concurrent_allocations: q.max_concurrent_allocations,
                burst_allowance: q.burst_allowance,
            });

            let isolation = req.isolation_level.as_ref().map(|l| match l.as_str() {
                "strict" => lattice_common::types::IsolationLevel::Strict,
                _ => lattice_common::types::IsolationLevel::Standard,
            });

            let resp = quorum
                .propose(Command::UpdateTenant {
                    id: req.tenant_id.clone(),
                    quota,
                    isolation_level: isolation,
                })
                .await
                .map_err(|e| Status::internal(format!("quorum error: {e}")))?;

            match resp {
                CommandResponse::Ok => {}
                CommandResponse::Error(e) => {
                    if e.contains("not found") {
                        return Err(Status::not_found(e));
                    }
                    return Err(Status::internal(e));
                }
                _ => return Err(Status::internal("unexpected response")),
            }

            // Read updated tenant from quorum state
            let state = quorum.state().read().await;
            let tenant = state
                .tenants
                .get(&req.tenant_id)
                .ok_or_else(|| Status::not_found("tenant not found after update"))?;
            Ok(Response::new(convert::tenant_to_response(tenant)))
        } else {
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
                    node_hours_budget: quota.node_hours_budget,
                    max_concurrent_allocations: quota.max_concurrent_allocations,
                    burst_allowance: quota.burst_allowance,
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

        if let Some(quorum) = self.quorum() {
            let resp = quorum
                .propose(Command::CreateVCluster(vc.clone()))
                .await
                .map_err(|e| Status::internal(format!("quorum error: {e}")))?;
            match resp {
                CommandResponse::VClusterId(_) => {}
                CommandResponse::Error(e) => return Err(Status::internal(e)),
                _ => return Err(Status::internal("unexpected response")),
            }
        } else {
            self.vclusters.write().await.push(vc.clone());
        }

        Ok(Response::new(convert::vcluster_to_response(&vc)))
    }

    async fn update_v_cluster(
        &self,
        request: Request<pb::UpdateVClusterRequest>,
    ) -> Result<Response<pb::VClusterResponse>, Status> {
        let req = request.into_inner();

        if let Some(quorum) = self.quorum() {
            let cost_weights =
                req.cost_weights
                    .as_ref()
                    .map(|w| lattice_common::types::CostWeights {
                        priority: w.priority,
                        wait_time: w.wait_time,
                        fair_share: w.fair_share,
                        topology: w.topology,
                        data_readiness: w.data_readiness,
                        backlog: w.backlog,
                        energy: w.energy,
                        checkpoint_efficiency: w.checkpoint_efficiency,
                        conformance: w.conformance,
                    });

            let resp = quorum
                .propose(Command::UpdateVCluster {
                    id: req.vcluster_id.clone(),
                    cost_weights,
                    allow_borrowing: req.allow_borrowing,
                    allow_lending: req.allow_lending,
                })
                .await
                .map_err(|e| Status::internal(format!("quorum error: {e}")))?;

            match resp {
                CommandResponse::Ok => {}
                CommandResponse::Error(e) => {
                    if e.contains("not found") {
                        return Err(Status::not_found(e));
                    }
                    return Err(Status::internal(e));
                }
                _ => return Err(Status::internal("unexpected response")),
            }

            let state = quorum.state().read().await;
            let vc = state
                .vclusters
                .get(&req.vcluster_id)
                .ok_or_else(|| Status::not_found("vcluster not found after update"))?;
            Ok(Response::new(convert::vcluster_to_response(vc)))
        } else {
            let mut vclusters = self.vclusters.write().await;
            let vc = vclusters
                .iter_mut()
                .find(|v| v.id == req.vcluster_id)
                .ok_or_else(|| {
                    Status::not_found(format!("vcluster {} not found", req.vcluster_id))
                })?;

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
    }

    async fn get_raft_status(
        &self,
        _request: Request<pb::GetRaftStatusRequest>,
    ) -> Result<Response<pb::RaftStatusResponse>, Status> {
        if let Some(quorum) = self.quorum() {
            // Query real Raft metrics
            let raft = quorum.raft();
            let metrics = raft
                .wait(Some(std::time::Duration::from_millis(100)))
                .metrics(|_| true, "get metrics")
                .await
                .map_err(|e| Status::internal(format!("failed to get Raft metrics: {e}")))?;

            Ok(Response::new(pb::RaftStatusResponse {
                leader_id: metrics.current_leader.unwrap_or(0),
                current_term: metrics.current_term,
                last_applied: metrics.last_applied.map(|l| l.index).unwrap_or(0),
                commit_index: metrics.last_log_index.unwrap_or(0),
                members: vec![],
            }))
        } else {
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
    }

    async fn backup_verify(
        &self,
        request: Request<pb::BackupVerifyRequest>,
    ) -> Result<Response<pb::BackupVerifyResponse>, Status> {
        let req = request.into_inner();

        if req.backup_path.is_empty() {
            return Err(Status::invalid_argument("backup_path is required"));
        }

        let path = std::path::Path::new(&req.backup_path);
        match lattice_quorum::verify_backup(path) {
            Ok(meta) => Ok(Response::new(pb::BackupVerifyResponse {
                valid: true,
                message: format!(
                    "Backup valid: {} nodes, {} allocations, {} tenants",
                    meta.app.node_count, meta.app.allocation_count, meta.app.tenant_count
                ),
                backup_timestamp: Some(prost_types::Timestamp {
                    seconds: meta.timestamp.timestamp(),
                    nanos: meta.timestamp.timestamp_subsec_nanos() as i32,
                }),
                snapshot_term: meta.snapshot_term,
                snapshot_index: meta.snapshot_index,
            })),
            Err(e) => Ok(Response::new(pb::BackupVerifyResponse {
                valid: false,
                message: format!("Backup verification failed: {e}"),
                backup_timestamp: None,
                snapshot_term: 0,
                snapshot_index: 0,
            })),
        }
    }

    async fn create_backup(
        &self,
        request: Request<pb::CreateBackupRequest>,
    ) -> Result<Response<pb::CreateBackupResponse>, Status> {
        let req = request.into_inner();

        if req.backup_path.is_empty() {
            return Err(Status::invalid_argument("backup_path is required"));
        }

        let quorum = self
            .quorum()
            .ok_or_else(|| Status::unavailable("quorum not configured"))?;

        let path = std::path::Path::new(&req.backup_path);
        let state = quorum.state();

        match lattice_quorum::export_backup(state, path).await {
            Ok(meta) => Ok(Response::new(pb::CreateBackupResponse {
                success: true,
                message: "Backup created successfully".to_string(),
                backup_timestamp: Some(prost_types::Timestamp {
                    seconds: meta.timestamp.timestamp(),
                    nanos: meta.timestamp.timestamp_subsec_nanos() as i32,
                }),
                node_count: meta.app.node_count as u64,
                allocation_count: meta.app.allocation_count as u64,
                tenant_count: meta.app.tenant_count as u64,
                audit_entry_count: meta.app.audit_entry_count as u64,
            })),
            Err(e) => Ok(Response::new(pb::CreateBackupResponse {
                success: false,
                message: format!("Backup failed: {e}"),
                backup_timestamp: None,
                node_count: 0,
                allocation_count: 0,
                tenant_count: 0,
                audit_entry_count: 0,
            })),
        }
    }

    async fn restore_backup(
        &self,
        request: Request<pb::RestoreBackupRequest>,
    ) -> Result<Response<pb::RestoreBackupResponse>, Status> {
        let req = request.into_inner();

        if req.backup_path.is_empty() {
            return Err(Status::invalid_argument("backup_path is required"));
        }

        let data_dir = self
            .state
            .data_dir
            .as_ref()
            .ok_or_else(|| Status::unavailable("data_dir not configured, cannot restore"))?;

        let backup_path = std::path::Path::new(&req.backup_path);

        match lattice_quorum::restore_backup(backup_path, data_dir) {
            Ok(meta) => Ok(Response::new(pb::RestoreBackupResponse {
                success: true,
                message: format!(
                    "Backup restored successfully ({} nodes, {} allocations). Restart required.",
                    meta.app.node_count, meta.app.allocation_count
                ),
            })),
            Err(e) => Ok(Response::new(pb::RestoreBackupResponse {
                success: false,
                message: format!("Restore failed: {e}"),
            })),
        }
    }

    // ─── Tenant / VCluster Read RPCs ────────────────────────

    async fn list_tenants(
        &self,
        _request: Request<pb::ListTenantsRequest>,
    ) -> Result<Response<pb::ListTenantsResponse>, Status> {
        if let Some(quorum) = self.quorum() {
            let state = quorum.state().read().await;
            let tenants: Vec<pb::TenantResponse> = state
                .tenants
                .values()
                .map(convert::tenant_to_response)
                .collect();
            Ok(Response::new(pb::ListTenantsResponse {
                tenants,
                next_cursor: String::new(),
            }))
        } else {
            let tenants = self.tenants.read().await;
            let responses: Vec<pb::TenantResponse> =
                tenants.iter().map(convert::tenant_to_response).collect();
            Ok(Response::new(pb::ListTenantsResponse {
                tenants: responses,
                next_cursor: String::new(),
            }))
        }
    }

    async fn get_tenant(
        &self,
        request: Request<pb::GetTenantRequest>,
    ) -> Result<Response<pb::TenantResponse>, Status> {
        let req = request.into_inner();

        if let Some(quorum) = self.quorum() {
            let state = quorum.state().read().await;
            let tenant = state
                .tenants
                .get(&req.tenant_id)
                .ok_or_else(|| Status::not_found(format!("tenant {} not found", req.tenant_id)))?;
            Ok(Response::new(convert::tenant_to_response(tenant)))
        } else {
            let tenants = self.tenants.read().await;
            let tenant = tenants
                .iter()
                .find(|t| t.id == req.tenant_id)
                .ok_or_else(|| Status::not_found(format!("tenant {} not found", req.tenant_id)))?;
            Ok(Response::new(convert::tenant_to_response(tenant)))
        }
    }

    async fn list_v_clusters(
        &self,
        _request: Request<pb::ListVClustersRequest>,
    ) -> Result<Response<pb::ListVClustersResponse>, Status> {
        if let Some(quorum) = self.quorum() {
            let state = quorum.state().read().await;
            let vclusters: Vec<pb::VClusterResponse> = state
                .vclusters
                .values()
                .map(convert::vcluster_to_response)
                .collect();
            Ok(Response::new(pb::ListVClustersResponse {
                vclusters,
                next_cursor: String::new(),
            }))
        } else {
            let vclusters = self.vclusters.read().await;
            let responses: Vec<pb::VClusterResponse> = vclusters
                .iter()
                .map(convert::vcluster_to_response)
                .collect();
            Ok(Response::new(pb::ListVClustersResponse {
                vclusters: responses,
                next_cursor: String::new(),
            }))
        }
    }

    async fn get_v_cluster(
        &self,
        request: Request<pb::GetVClusterRequest>,
    ) -> Result<Response<pb::VClusterResponse>, Status> {
        let req = request.into_inner();

        if let Some(quorum) = self.quorum() {
            let state = quorum.state().read().await;
            let vc = state.vclusters.get(&req.vcluster_id).ok_or_else(|| {
                Status::not_found(format!("vcluster {} not found", req.vcluster_id))
            })?;
            Ok(Response::new(convert::vcluster_to_response(vc)))
        } else {
            let vclusters = self.vclusters.read().await;
            let vc = vclusters
                .iter()
                .find(|v| v.id == req.vcluster_id)
                .ok_or_else(|| {
                    Status::not_found(format!("vcluster {} not found", req.vcluster_id))
                })?;
            Ok(Response::new(convert::vcluster_to_response(vc)))
        }
    }

    async fn get_v_cluster_queue(
        &self,
        request: Request<pb::GetVClusterQueueRequest>,
    ) -> Result<Response<pb::VClusterQueueResponse>, Status> {
        let req = request.into_inner();

        // Count allocations by state for this vcluster
        let filter = lattice_common::traits::AllocationFilter {
            vcluster: Some(req.vcluster_id.clone()),
            ..Default::default()
        };

        let allocations = self
            .state
            .allocations
            .list(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let pending = allocations
            .iter()
            .filter(|a| a.state == lattice_common::types::AllocationState::Pending)
            .count() as u32;
        let running = allocations
            .iter()
            .filter(|a| a.state == lattice_common::types::AllocationState::Running)
            .count() as u32;

        Ok(Response::new(pb::VClusterQueueResponse {
            vcluster_id: req.vcluster_id,
            pending,
            running,
            total: allocations.len() as u32,
        }))
    }

    // ─── Audit / Accounting RPCs ────────────────────────────

    async fn query_audit(
        &self,
        request: Request<pb::QueryAuditRequest>,
    ) -> Result<Response<pb::QueryAuditResponse>, Status> {
        let req = request.into_inner();

        let filter = lattice_common::traits::AuditFilter {
            principal: if req.user_id.is_empty() {
                None
            } else {
                Some(req.user_id)
            },
            allocation: None,
            action: if req.action.is_empty() {
                None
            } else {
                Some(req.action)
            },
            since: None,
            until: None,
        };

        let entries = self
            .state
            .audit
            .query(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let proto_entries: Vec<pb::AuditEntryProto> = entries
            .into_iter()
            .map(|e| pb::AuditEntryProto {
                id: e.event.id.clone(),
                tenant_id: req.tenant_id.clone(),
                user_id: e.event.principal.identity.clone(),
                action: e.event.action.clone(),
                details: e.event.detail.clone(),
                timestamp: Some(prost_types::Timestamp {
                    seconds: e.event.timestamp.timestamp(),
                    nanos: e.event.timestamp.timestamp_subsec_nanos() as i32,
                }),
            })
            .collect();

        Ok(Response::new(pb::QueryAuditResponse {
            entries: proto_entries,
            next_cursor: String::new(),
        }))
    }

    async fn get_accounting_usage(
        &self,
        request: Request<pb::GetAccountingUsageRequest>,
    ) -> Result<Response<pb::AccountingUsageResponse>, Status> {
        let req = request.into_inner();

        // If accounting service is configured, delegate to it
        if let Some(ref _accounting) = self.state.accounting {
            // Real accounting implementation would query the accounting service
            Ok(Response::new(pb::AccountingUsageResponse {
                tenant_id: req.tenant_id,
                cpu_hours: 0.0,
                gpu_hours: 0.0,
                storage_bytes: 0,
                allocation_count: 0,
            }))
        } else {
            Ok(Response::new(pb::AccountingUsageResponse {
                tenant_id: req.tenant_id,
                cpu_hours: 0.0,
                gpu_hours: 0.0,
                storage_bytes: 0,
                allocation_count: 0,
            }))
        }
    }

    async fn get_tenant_usage(
        &self,
        request: Request<pb::GetTenantUsageRequest>,
    ) -> Result<Response<pb::TenantUsageResponse>, Status> {
        let req = request.into_inner();
        let days = if req.days == 0 { 90 } else { req.days };

        let quorum = self
            .quorum()
            .ok_or_else(|| Status::unavailable("quorum not configured"))?;

        let gs = quorum.state().read().await;
        let tenant = gs
            .tenants
            .get(&req.tenant_id)
            .ok_or_else(|| Status::not_found(format!("tenant '{}' not found", req.tenant_id)))?
            .clone();
        drop(gs);

        let now = chrono::Utc::now();
        let period_start = now - chrono::Duration::days(days as i64);

        let allocs = self
            .state
            .allocations
            .list(&lattice_common::traits::AllocationFilter {
                tenant: Some(req.tenant_id.clone()),
                ..Default::default()
            })
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let nodes = self
            .state
            .nodes
            .list_nodes(&lattice_common::traits::NodeFilter::default())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let metrics = lattice_scheduler::quota::compute_tenant_usage_metrics(
            &tenant,
            &allocs,
            &nodes,
            period_start,
            now,
        );

        let gpu_fraction = tenant
            .quota
            .gpu_hours_budget
            .filter(|b| *b > 0.0)
            .map(|b| metrics.gpu_hours_used / b);
        let node_fraction = tenant
            .quota
            .node_hours_budget
            .filter(|b| *b > 0.0)
            .map(|b| metrics.node_hours_used / b);

        Ok(Response::new(pb::TenantUsageResponse {
            tenant: req.tenant_id,
            gpu_hours_used: metrics.gpu_hours_used,
            gpu_hours_budget: tenant.quota.gpu_hours_budget,
            gpu_fraction_used: gpu_fraction,
            node_hours_used: metrics.node_hours_used,
            node_hours_budget: tenant.quota.node_hours_budget,
            node_fraction_used: node_fraction,
            period_start: period_start.to_rfc3339(),
            period_end: now.to_rfc3339(),
            period_days: days,
        }))
    }

    async fn get_user_usage(
        &self,
        request: Request<pb::GetUserUsageRequest>,
    ) -> Result<Response<pb::UserUsageResponse>, Status> {
        let req = request.into_inner();
        let days = if req.days == 0 { 90 } else { req.days };
        let now = chrono::Utc::now();
        let period_start = now - chrono::Duration::days(days as i64);

        let allocs = self
            .state
            .allocations
            .list(&lattice_common::traits::AllocationFilter {
                user: Some(req.user.clone()),
                ..Default::default()
            })
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let nodes = self
            .state
            .nodes
            .list_nodes(&lattice_common::traits::NodeFilter::default())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let by_tenant = lattice_scheduler::quota::compute_user_usage_metrics(
            &req.user,
            &allocs,
            &nodes,
            period_start,
            now,
        );

        // Look up budgets from quorum if available
        let tenant_quotas: std::collections::HashMap<String, lattice_common::types::TenantQuota> =
            if let Some(quorum) = self.quorum() {
                let gs = quorum.state().read().await;
                gs.tenants
                    .iter()
                    .map(|(id, t)| (id.clone(), t.quota.clone()))
                    .collect()
            } else {
                std::collections::HashMap::new()
            };

        let total_gpu_hours: f64 = by_tenant.values().map(|m| m.gpu_hours_used).sum();
        let tenants: Vec<pb::UserTenantUsageProto> = by_tenant
            .into_iter()
            .map(|(tid, metrics)| {
                let quota = tenant_quotas.get(&tid);
                pb::UserTenantUsageProto {
                    tenant: tid,
                    gpu_hours_used: metrics.gpu_hours_used,
                    gpu_hours_budget: quota.and_then(|q| q.gpu_hours_budget),
                    node_hours_used: metrics.node_hours_used,
                    node_hours_budget: quota.and_then(|q| q.node_hours_budget),
                }
            })
            .collect();

        Ok(Response::new(pb::UserUsageResponse {
            user: req.user,
            tenants,
            total_gpu_hours,
            period_start: period_start.to_rfc3339(),
            period_end: now.to_rfc3339(),
        }))
    }

    async fn lookup_service(
        &self,
        request: Request<pb::LookupServiceRequest>,
    ) -> Result<Response<pb::LookupServiceResponse>, Status> {
        let requesting_tenant = request
            .metadata()
            .get("x-lattice-tenant")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let name = request.into_inner().name;

        let endpoints = if let Some(ref quorum) = self.state.quorum {
            let sm = quorum.state().read().await;
            match sm.lookup_service(&name) {
                Some(entry) => entry
                    .endpoints
                    .iter()
                    // Filter to requesting tenant's endpoints only (F05)
                    .filter(|ep| requesting_tenant.as_ref().map_or(true, |t| &ep.tenant == t))
                    .map(|ep| pb::RegisteredEndpointProto {
                        allocation_id: ep.allocation_id.to_string(),
                        tenant: ep.tenant.clone(),
                        nodes: ep.nodes.clone(),
                        port: ep.port as u32,
                        protocol: ep.protocol.clone().unwrap_or_default(),
                    })
                    .collect(),
                None => vec![],
            }
        } else {
            vec![]
        };

        Ok(Response::new(pb::LookupServiceResponse { name, endpoints }))
    }

    async fn list_services(
        &self,
        request: Request<pb::ListServicesRequest>,
    ) -> Result<Response<pb::ListServicesResponse>, Status> {
        let requesting_tenant = request
            .metadata()
            .get("x-lattice-tenant")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let names = if let Some(ref quorum) = self.state.quorum {
            let sm = quorum.state().read().await;
            // Filter to services that have endpoints for the requesting tenant (F05)
            if let Some(ref tenant) = requesting_tenant {
                sm.list_services()
                    .into_iter()
                    .filter(|name| {
                        sm.lookup_service(name)
                            .is_some_and(|e| e.endpoints.iter().any(|ep| &ep.tenant == tenant))
                    })
                    .collect()
            } else {
                sm.list_services()
            }
        } else {
            vec![]
        };

        Ok(Response::new(pb::ListServicesResponse { names }))
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
            san_validator: crate::state::ApiState::default_dev_san_validator(),
        })
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
            san_validator: crate::state::ApiState::default_dev_san_validator(),
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
                    ..Default::default()
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
                    ..Default::default()
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

    #[tokio::test]
    async fn create_tenant_via_quorum() {
        let state = test_state_with_quorum().await;
        let svc = LatticeAdminService::new(state.clone());

        let resp = svc
            .create_tenant(Request::new(pb::CreateTenantRequest {
                name: "bio".to_string(),
                quota: Some(pb::TenantQuotaSpec {
                    max_nodes: 10,
                    fair_share_target: 0.1,
                    ..Default::default()
                }),
                isolation_level: "standard".to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().name, "bio");

        // Verify it's in quorum state
        let quorum_state = state.quorum.as_ref().unwrap().state().read().await;
        assert!(quorum_state.tenants.contains_key("bio"));
    }

    #[tokio::test]
    async fn create_vcluster_via_quorum() {
        let state = test_state_with_quorum().await;
        let svc = LatticeAdminService::new(state.clone());

        let resp = svc
            .create_v_cluster(Request::new(pb::CreateVClusterRequest {
                tenant_id: "bio".to_string(),
                name: "gpu-batch".to_string(),
                scheduler_type: "hpc_backfill".to_string(),
                cost_weights: None,
                dedicated_nodes: vec![],
                allow_borrowing: false,
                allow_lending: false,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().name, "gpu-batch");

        // Verify it's in quorum state
        let quorum_state = state.quorum.as_ref().unwrap().state().read().await;
        assert!(quorum_state.vclusters.contains_key("bio/gpu-batch"));
    }

    #[tokio::test]
    async fn create_tenant_with_node_hours_budget() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let resp = svc
            .create_tenant(Request::new(pb::CreateTenantRequest {
                name: "ml-team".to_string(),
                quota: Some(pb::TenantQuotaSpec {
                    max_nodes: 20,
                    fair_share_target: 0.2,
                    gpu_hours_budget: Some(5000.0),
                    max_concurrent_allocations: Some(5),
                    node_hours_budget: Some(10000.0),
                    burst_allowance: Some(1.5),
                }),
                isolation_level: "standard".to_string(),
            }))
            .await
            .unwrap();

        let quota = resp.get_ref().quota.as_ref().unwrap();
        assert_eq!(quota.node_hours_budget, Some(10000.0));
        assert_eq!(quota.burst_allowance, Some(1.5));
        assert_eq!(quota.gpu_hours_budget, Some(5000.0));
    }

    #[tokio::test]
    async fn update_tenant_preserves_node_hours_budget() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        // Create with budgets
        svc.create_tenant(Request::new(pb::CreateTenantRequest {
            name: "bio".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 10,
                fair_share_target: 0.1,
                gpu_hours_budget: None,
                max_concurrent_allocations: None,
                node_hours_budget: None,
                burst_allowance: None,
            }),
            isolation_level: "standard".to_string(),
        }))
        .await
        .unwrap();

        // Update with node_hours_budget
        let resp = svc
            .update_tenant(Request::new(pb::UpdateTenantRequest {
                tenant_id: "bio".to_string(),
                quota: Some(pb::TenantQuotaSpec {
                    max_nodes: 10,
                    fair_share_target: 0.1,
                    gpu_hours_budget: None,
                    max_concurrent_allocations: None,
                    node_hours_budget: Some(8000.0),
                    burst_allowance: Some(2.0),
                }),
                isolation_level: None,
            }))
            .await
            .unwrap();

        let quota = resp.get_ref().quota.as_ref().unwrap();
        assert_eq!(quota.node_hours_budget, Some(8000.0));
        assert_eq!(quota.burst_allowance, Some(2.0));
    }

    #[tokio::test]
    async fn get_tenant_usage_requires_quorum() {
        let state = test_state();
        let svc = LatticeAdminService::new(state);

        let result = svc
            .get_tenant_usage(Request::new(pb::GetTenantUsageRequest {
                tenant_id: "nonexistent".to_string(),
                days: 30,
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn get_tenant_usage_via_quorum() {
        let state = test_state_with_quorum().await;
        let svc = LatticeAdminService::new(state.clone());

        // Create a tenant first
        svc.create_tenant(Request::new(pb::CreateTenantRequest {
            name: "usage-test".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 10,
                fair_share_target: 0.1,
                gpu_hours_budget: Some(1000.0),
                max_concurrent_allocations: None,
                node_hours_budget: Some(5000.0),
                burst_allowance: None,
            }),
            isolation_level: "standard".to_string(),
        }))
        .await
        .unwrap();

        let resp = svc
            .get_tenant_usage(Request::new(pb::GetTenantUsageRequest {
                tenant_id: "usage-test".to_string(),
                days: 90,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().tenant, "usage-test");
        assert_eq!(resp.get_ref().gpu_hours_budget, Some(1000.0));
        assert_eq!(resp.get_ref().node_hours_budget, Some(5000.0));
        assert_eq!(resp.get_ref().period_days, 90);
        // No allocations yet, so usage should be 0
        assert_eq!(resp.get_ref().gpu_hours_used, 0.0);
        assert_eq!(resp.get_ref().node_hours_used, 0.0);
    }

    #[tokio::test]
    async fn get_user_usage_empty() {
        let state = test_state_with_quorum().await;
        let svc = LatticeAdminService::new(state);

        let resp = svc
            .get_user_usage(Request::new(pb::GetUserUsageRequest {
                user: "testuser".to_string(),
                days: 30,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().user, "testuser");
        assert!(resp.get_ref().tenants.is_empty());
        assert_eq!(resp.get_ref().total_gpu_hours, 0.0);
    }

    #[tokio::test]
    async fn get_tenant_usage_defaults_to_90_days() {
        let state = test_state_with_quorum().await;
        let svc = LatticeAdminService::new(state);

        svc.create_tenant(Request::new(pb::CreateTenantRequest {
            name: "days-test".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 5,
                fair_share_target: 0.1,
                gpu_hours_budget: None,
                max_concurrent_allocations: None,
                node_hours_budget: None,
                burst_allowance: None,
            }),
            isolation_level: "standard".to_string(),
        }))
        .await
        .unwrap();

        let resp = svc
            .get_tenant_usage(Request::new(pb::GetTenantUsageRequest {
                tenant_id: "days-test".to_string(),
                days: 0, // should default to 90
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().period_days, 90);
    }

    #[tokio::test]
    async fn raft_status_from_quorum() {
        let state = test_state_with_quorum().await;
        let svc = LatticeAdminService::new(state);

        let resp = svc
            .get_raft_status(Request::new(pb::GetRaftStatusRequest {}))
            .await
            .unwrap();

        // With a real quorum, leader should be 1
        assert_eq!(resp.get_ref().leader_id, 1);
        assert!(resp.get_ref().current_term > 0);
    }
}

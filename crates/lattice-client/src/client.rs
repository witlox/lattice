//! Unified Lattice gRPC client.

use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;

use lattice_common::proto::lattice::v1 as pb;
use pb::admin_service_client::AdminServiceClient;
use pb::allocation_service_client::AllocationServiceClient;
use pb::node_service_client::NodeServiceClient;

use crate::auth::AuthInterceptor;
use crate::config::ClientConfig;
use crate::error::LatticeClientError;

type AuthChannel = InterceptedService<Channel, AuthInterceptor>;

/// gRPC client for the Lattice distributed workload scheduler.
///
/// Wraps all three gRPC services (Allocation, Node, Admin) behind a single
/// connection with bearer-token authentication.
///
/// # Example
///
/// ```no_run
/// # async fn example() -> Result<(), lattice_client::LatticeClientError> {
/// use lattice_client::{LatticeClient, ClientConfig};
///
/// let config = ClientConfig {
///     endpoint: "http://lattice-api:50051".to_string(),
///     token: Some("my-token".to_string()),
///     ..Default::default()
/// };
/// let mut client = LatticeClient::connect(config).await?;
/// let nodes = client.list_nodes(Default::default()).await?;
/// # Ok(())
/// # }
/// ```
pub struct LatticeClient {
    alloc: AllocationServiceClient<AuthChannel>,
    nodes: NodeServiceClient<AuthChannel>,
    admin: AdminServiceClient<AuthChannel>,
}

impl LatticeClient {
    /// Connect to a Lattice API server.
    pub async fn connect(config: ClientConfig) -> Result<Self, LatticeClientError> {
        let channel = Channel::from_shared(config.endpoint.clone())
            .map_err(|e| LatticeClientError::Internal(e.to_string()))?
            .timeout(config.timeout())
            .connect()
            .await?;
        let interceptor = AuthInterceptor::new(config.token);
        Ok(Self {
            alloc: AllocationServiceClient::with_interceptor(channel.clone(), interceptor.clone()),
            nodes: NodeServiceClient::with_interceptor(channel.clone(), interceptor.clone()),
            admin: AdminServiceClient::with_interceptor(channel, interceptor),
        })
    }

    // ─── Allocation Operations ──────────────────────────────

    /// Submit an allocation, DAG, or task group.
    pub async fn submit(
        &mut self,
        req: pb::SubmitRequest,
    ) -> Result<pb::SubmitResponse, LatticeClientError> {
        Ok(self.alloc.submit(req).await?.into_inner())
    }

    /// Get a single allocation's status.
    pub async fn get_allocation(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::AllocationStatus, LatticeClientError> {
        let req = pb::GetAllocationRequest {
            allocation_id: allocation_id.to_string(),
        };
        Ok(self.alloc.get(req).await?.into_inner())
    }

    /// List allocations with filters.
    pub async fn list_allocations(
        &mut self,
        req: pb::ListAllocationsRequest,
    ) -> Result<pb::ListAllocationsResponse, LatticeClientError> {
        Ok(self.alloc.list(req).await?.into_inner())
    }

    /// Cancel a single allocation.
    pub async fn cancel(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::CancelResponse, LatticeClientError> {
        let req = pb::CancelRequest {
            allocation_id: allocation_id.to_string(),
        };
        Ok(self.alloc.cancel(req).await?.into_inner())
    }

    /// Update a running allocation (extend walltime, change telemetry).
    pub async fn update(
        &mut self,
        req: pb::UpdateAllocationRequest,
    ) -> Result<pb::AllocationStatus, LatticeClientError> {
        Ok(self.alloc.update(req).await?.into_inner())
    }

    /// Launch tasks within an existing allocation (srun equivalent).
    pub async fn launch_tasks(
        &mut self,
        req: pb::LaunchTasksRequest,
    ) -> Result<pb::LaunchTasksResponse, LatticeClientError> {
        Ok(self.alloc.launch_tasks(req).await?.into_inner())
    }

    /// Request a checkpoint of a running allocation.
    pub async fn checkpoint(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::CheckpointResponse, LatticeClientError> {
        let req = pb::CheckpointRequest {
            allocation_id: allocation_id.to_string(),
        };
        Ok(self.alloc.checkpoint(req).await?.into_inner())
    }

    /// Watch allocation events (server-streaming).
    pub async fn watch(
        &mut self,
        req: pb::WatchRequest,
    ) -> Result<tonic::Streaming<pb::AllocationEvent>, LatticeClientError> {
        Ok(self.alloc.watch(req).await?.into_inner())
    }

    /// Stream logs from an allocation (server-streaming).
    pub async fn stream_logs(
        &mut self,
        req: pb::LogStreamRequest,
    ) -> Result<tonic::Streaming<pb::LogEntry>, LatticeClientError> {
        Ok(self.alloc.stream_logs(req).await?.into_inner())
    }

    /// Query a metrics snapshot for an allocation.
    pub async fn query_metrics(
        &mut self,
        req: pb::QueryMetricsRequest,
    ) -> Result<pb::MetricsSnapshot, LatticeClientError> {
        Ok(self.alloc.query_metrics(req).await?.into_inner())
    }

    /// Stream live metrics from an allocation (server-streaming).
    pub async fn stream_metrics(
        &mut self,
        req: pb::StreamMetricsRequest,
    ) -> Result<tonic::Streaming<pb::MetricsEvent>, LatticeClientError> {
        Ok(self.alloc.stream_metrics(req).await?.into_inner())
    }

    /// Get diagnostics (network + storage health) for a running allocation.
    pub async fn get_diagnostics(
        &mut self,
        req: pb::DiagnosticsRequest,
    ) -> Result<pb::DiagnosticsResponse, LatticeClientError> {
        Ok(self.alloc.get_diagnostics(req).await?.into_inner())
    }

    /// Compare metrics across multiple allocations.
    pub async fn compare_metrics(
        &mut self,
        req: pb::CompareMetricsRequest,
    ) -> Result<pb::CompareMetricsResponse, LatticeClientError> {
        Ok(self.alloc.compare_metrics(req).await?.into_inner())
    }

    /// Attach an interactive terminal (bidirectional streaming).
    pub async fn attach(
        &mut self,
        input: impl tokio_stream::Stream<Item = pb::AttachInput> + Send + 'static,
    ) -> Result<tonic::Streaming<pb::AttachOutput>, LatticeClientError> {
        Ok(self.alloc.attach(input).await?.into_inner())
    }

    // ─── DAG Operations ─────────────────────────────────────

    /// Get DAG status.
    pub async fn get_dag(&mut self, dag_id: &str) -> Result<pb::DagStatus, LatticeClientError> {
        let req = pb::GetDagRequest {
            dag_id: dag_id.to_string(),
        };
        Ok(self.alloc.get_dag(req).await?.into_inner())
    }

    /// List DAGs with filters.
    pub async fn list_dags(
        &mut self,
        req: pb::ListDagsRequest,
    ) -> Result<pb::ListDagsResponse, LatticeClientError> {
        Ok(self.alloc.list_dags(req).await?.into_inner())
    }

    /// Cancel a DAG workflow.
    pub async fn cancel_dag(
        &mut self,
        dag_id: &str,
    ) -> Result<pb::CancelDagResponse, LatticeClientError> {
        let req = pb::CancelDagRequest {
            dag_id: dag_id.to_string(),
        };
        Ok(self.alloc.cancel_dag(req).await?.into_inner())
    }

    // ─── Session Operations ─────────────────────────────────

    /// Create an interactive session attached to an allocation.
    pub async fn create_session(
        &mut self,
        allocation_id: &str,
        user_id: &str,
    ) -> Result<pb::SessionResponse, LatticeClientError> {
        let req = pb::CreateSessionRequest {
            allocation_id: allocation_id.to_string(),
            user_id: user_id.to_string(),
        };
        Ok(self.alloc.create_session(req).await?.into_inner())
    }

    /// Get session details.
    pub async fn get_session(
        &mut self,
        session_id: &str,
    ) -> Result<pb::SessionResponse, LatticeClientError> {
        let req = pb::GetSessionRequest {
            session_id: session_id.to_string(),
        };
        Ok(self.alloc.get_session(req).await?.into_inner())
    }

    /// Delete (terminate) a session.
    pub async fn delete_session(
        &mut self,
        session_id: &str,
    ) -> Result<pb::DeleteSessionResponse, LatticeClientError> {
        let req = pb::DeleteSessionRequest {
            session_id: session_id.to_string(),
        };
        Ok(self.alloc.delete_session(req).await?.into_inner())
    }

    // ─── Node Operations ────────────────────────────────────

    /// List nodes with optional filters.
    pub async fn list_nodes(
        &mut self,
        req: pb::ListNodesRequest,
    ) -> Result<pb::ListNodesResponse, LatticeClientError> {
        Ok(self.nodes.list_nodes(req).await?.into_inner())
    }

    /// Get a single node by ID.
    pub async fn get_node(&mut self, node_id: &str) -> Result<pb::NodeStatus, LatticeClientError> {
        let req = pb::GetNodeRequest {
            node_id: node_id.to_string(),
        };
        Ok(self.nodes.get_node(req).await?.into_inner())
    }

    /// Drain a node (finish existing allocations, accept no new ones).
    pub async fn drain_node(
        &mut self,
        node_id: &str,
        reason: &str,
    ) -> Result<pb::DrainNodeResponse, LatticeClientError> {
        let req = pb::DrainNodeRequest {
            node_id: node_id.to_string(),
            reason: reason.to_string(),
        };
        Ok(self.nodes.drain_node(req).await?.into_inner())
    }

    /// Cancel drain and return node to Ready state.
    pub async fn undrain_node(
        &mut self,
        node_id: &str,
    ) -> Result<pb::UndrainNodeResponse, LatticeClientError> {
        let req = pb::UndrainNodeRequest {
            node_id: node_id.to_string(),
        };
        Ok(self.nodes.undrain_node(req).await?.into_inner())
    }

    /// Disable a node (mark as unavailable until re-enabled).
    pub async fn disable_node(
        &mut self,
        node_id: &str,
        reason: &str,
    ) -> Result<pb::DisableNodeResponse, LatticeClientError> {
        let req = pb::DisableNodeRequest {
            node_id: node_id.to_string(),
            reason: reason.to_string(),
        };
        Ok(self.nodes.disable_node(req).await?.into_inner())
    }

    /// Re-enable a previously disabled node.
    pub async fn enable_node(
        &mut self,
        node_id: &str,
    ) -> Result<pb::EnableNodeResponse, LatticeClientError> {
        let req = pb::EnableNodeRequest {
            node_id: node_id.to_string(),
        };
        Ok(self.nodes.enable_node(req).await?.into_inner())
    }

    /// Health check.
    pub async fn health(&mut self) -> Result<pb::HealthResponse, LatticeClientError> {
        Ok(self.nodes.health(pb::HealthRequest {}).await?.into_inner())
    }

    // ─── Tenant Operations ──────────────────────────────────

    /// Create a new tenant.
    pub async fn create_tenant(
        &mut self,
        req: pb::CreateTenantRequest,
    ) -> Result<pb::TenantResponse, LatticeClientError> {
        Ok(self.admin.create_tenant(req).await?.into_inner())
    }

    /// Update an existing tenant.
    pub async fn update_tenant(
        &mut self,
        req: pb::UpdateTenantRequest,
    ) -> Result<pb::TenantResponse, LatticeClientError> {
        Ok(self.admin.update_tenant(req).await?.into_inner())
    }

    /// List all tenants.
    pub async fn list_tenants(
        &mut self,
        req: pb::ListTenantsRequest,
    ) -> Result<pb::ListTenantsResponse, LatticeClientError> {
        Ok(self.admin.list_tenants(req).await?.into_inner())
    }

    /// Get a single tenant by ID.
    pub async fn get_tenant(
        &mut self,
        tenant_id: &str,
    ) -> Result<pb::TenantResponse, LatticeClientError> {
        let req = pb::GetTenantRequest {
            tenant_id: tenant_id.to_string(),
        };
        Ok(self.admin.get_tenant(req).await?.into_inner())
    }

    // ─── VCluster Operations ────────────────────────────────

    /// Create a new vCluster.
    pub async fn create_vcluster(
        &mut self,
        req: pb::CreateVClusterRequest,
    ) -> Result<pb::VClusterResponse, LatticeClientError> {
        Ok(self.admin.create_v_cluster(req).await?.into_inner())
    }

    /// Update vCluster settings.
    pub async fn update_vcluster(
        &mut self,
        req: pb::UpdateVClusterRequest,
    ) -> Result<pb::VClusterResponse, LatticeClientError> {
        Ok(self.admin.update_v_cluster(req).await?.into_inner())
    }

    /// List all vClusters.
    pub async fn list_vclusters(
        &mut self,
        req: pb::ListVClustersRequest,
    ) -> Result<pb::ListVClustersResponse, LatticeClientError> {
        Ok(self.admin.list_v_clusters(req).await?.into_inner())
    }

    /// Get a single vCluster by ID.
    pub async fn get_vcluster(
        &mut self,
        vcluster_id: &str,
    ) -> Result<pb::VClusterResponse, LatticeClientError> {
        let req = pb::GetVClusterRequest {
            vcluster_id: vcluster_id.to_string(),
        };
        Ok(self.admin.get_v_cluster(req).await?.into_inner())
    }

    /// Get queue info for a vCluster.
    pub async fn vcluster_queue(
        &mut self,
        vcluster_id: &str,
    ) -> Result<pb::VClusterQueueResponse, LatticeClientError> {
        let req = pb::GetVClusterQueueRequest {
            vcluster_id: vcluster_id.to_string(),
        };
        Ok(self.admin.get_v_cluster_queue(req).await?.into_inner())
    }

    // ─── Audit / Accounting ─────────────────────────────────

    /// Query audit log entries.
    pub async fn query_audit(
        &mut self,
        req: pb::QueryAuditRequest,
    ) -> Result<pb::QueryAuditResponse, LatticeClientError> {
        Ok(self.admin.query_audit(req).await?.into_inner())
    }

    /// Get accounting usage data.
    pub async fn accounting_usage(
        &mut self,
        req: pb::GetAccountingUsageRequest,
    ) -> Result<pb::AccountingUsageResponse, LatticeClientError> {
        Ok(self.admin.get_accounting_usage(req).await?.into_inner())
    }

    // ─── Service Discovery ────────────────────────────────────

    /// Look up registered endpoints for a named service.
    pub async fn lookup_service(
        &mut self,
        name: &str,
    ) -> Result<pb::LookupServiceResponse, LatticeClientError> {
        Ok(self
            .admin
            .lookup_service(pb::LookupServiceRequest {
                name: name.to_string(),
            })
            .await?
            .into_inner())
    }

    /// List all registered service names.
    pub async fn list_services(&mut self) -> Result<pb::ListServicesResponse, LatticeClientError> {
        Ok(self
            .admin
            .list_services(pb::ListServicesRequest {})
            .await?
            .into_inner())
    }

    // ─── Raft / Admin ───────────────────────────────────────

    /// Get Raft cluster status.
    pub async fn raft_status(&mut self) -> Result<pb::RaftStatusResponse, LatticeClientError> {
        Ok(self
            .admin
            .get_raft_status(pb::GetRaftStatusRequest {})
            .await?
            .into_inner())
    }

    /// Create a backup of the current state.
    pub async fn create_backup(
        &mut self,
        backup_path: &str,
    ) -> Result<pb::CreateBackupResponse, LatticeClientError> {
        let req = pb::CreateBackupRequest {
            backup_path: backup_path.to_string(),
        };
        Ok(self.admin.create_backup(req).await?.into_inner())
    }

    /// Verify backup integrity.
    pub async fn verify_backup(
        &mut self,
        backup_path: &str,
    ) -> Result<pb::BackupVerifyResponse, LatticeClientError> {
        let req = pb::BackupVerifyRequest {
            backup_path: backup_path.to_string(),
        };
        Ok(self.admin.backup_verify(req).await?.into_inner())
    }

    /// Restore from a backup.
    pub async fn restore_backup(
        &mut self,
        backup_path: &str,
    ) -> Result<pb::RestoreBackupResponse, LatticeClientError> {
        let req = pb::RestoreBackupRequest {
            backup_path: backup_path.to_string(),
        };
        Ok(self.admin.restore_backup(req).await?.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_invalid_endpoint_returns_error() {
        let config = ClientConfig {
            endpoint: "not a valid uri ://".to_string(),
            ..Default::default()
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(LatticeClient::connect(config));
        assert!(result.is_err());
    }
}

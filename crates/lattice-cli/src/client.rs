//! gRPC client wrapper — connects to the lattice-api server.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tonic::service::interceptor::InterceptedService;
use tonic::service::Interceptor;
use tonic::transport::Channel;

use lattice_common::proto::lattice::v1 as pb;
use pb::admin_service_client::AdminServiceClient;
use pb::allocation_service_client::AllocationServiceClient;
use pb::node_service_client::NodeServiceClient;

/// Interceptor that adds a bearer token to outgoing gRPC requests.
#[derive(Clone)]
pub struct AuthInterceptor {
    token: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
}

impl AuthInterceptor {
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: token.and_then(|t| {
                format!("Bearer {t}")
                    .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                    .ok()
            }),
        }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(ref token) = self.token {
            request
                .metadata_mut()
                .insert("authorization", token.clone());
        }
        Ok(request)
    }
}

/// Client configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub api_endpoint: String,
    pub timeout_secs: u64,
    pub user: String,
    pub tenant: Option<String>,
    pub vcluster: Option<String>,
    /// Bearer token for authenticated gRPC requests.
    #[serde(skip)]
    pub token: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            api_endpoint: "http://localhost:50051".to_string(),
            timeout_secs: 30,
            user: whoami().unwrap_or_else(|| "anonymous".to_string()),
            tenant: None,
            vcluster: None,
            token: None,
        }
    }
}

impl ClientConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

fn whoami() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
}

type AuthChannel = InterceptedService<Channel, AuthInterceptor>;

/// gRPC client that wraps all three Lattice services.
pub struct LatticeGrpcClient {
    pub(crate) allocations: AllocationServiceClient<AuthChannel>,
    nodes: NodeServiceClient<AuthChannel>,
    admin: AdminServiceClient<AuthChannel>,
}

impl LatticeGrpcClient {
    /// Connect to the lattice-api server using the given configuration.
    pub async fn connect(config: &ClientConfig) -> Result<Self, anyhow::Error> {
        let channel = Channel::from_shared(config.api_endpoint.clone())?
            .timeout(config.timeout())
            .connect()
            .await?;
        let interceptor = AuthInterceptor::new(config.token.clone());
        Ok(Self {
            allocations: AllocationServiceClient::with_interceptor(
                channel.clone(),
                interceptor.clone(),
            ),
            nodes: NodeServiceClient::with_interceptor(channel.clone(), interceptor.clone()),
            admin: AdminServiceClient::with_interceptor(channel, interceptor),
        })
    }

    /// Submit an allocation, DAG, or task group.
    pub async fn submit(
        &mut self,
        req: pb::SubmitRequest,
    ) -> Result<pb::SubmitResponse, tonic::Status> {
        let resp = self.allocations.submit(req).await?;
        Ok(resp.into_inner())
    }

    /// Get a single allocation's status.
    pub async fn get_allocation(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::AllocationStatus, tonic::Status> {
        let req = pb::GetAllocationRequest {
            allocation_id: allocation_id.to_string(),
        };
        let resp = self.allocations.get(req).await?;
        Ok(resp.into_inner())
    }

    /// List allocations with filters.
    pub async fn list_allocations(
        &mut self,
        req: pb::ListAllocationsRequest,
    ) -> Result<pb::ListAllocationsResponse, tonic::Status> {
        let resp = self.allocations.list(req).await?;
        Ok(resp.into_inner())
    }

    /// Cancel a single allocation.
    pub async fn cancel(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::CancelResponse, tonic::Status> {
        let req = pb::CancelRequest {
            allocation_id: allocation_id.to_string(),
        };
        let resp = self.allocations.cancel(req).await?;
        Ok(resp.into_inner())
    }

    /// Watch allocation events (server-streaming).
    pub async fn watch(
        &mut self,
        req: pb::WatchRequest,
    ) -> Result<tonic::Streaming<pb::AllocationEvent>, tonic::Status> {
        let resp = self.allocations.watch(req).await?;
        Ok(resp.into_inner())
    }

    /// Stream logs from an allocation (server-streaming).
    pub async fn stream_logs(
        &mut self,
        req: pb::LogStreamRequest,
    ) -> Result<tonic::Streaming<pb::LogEntry>, tonic::Status> {
        let resp = self.allocations.stream_logs(req).await?;
        Ok(resp.into_inner())
    }

    /// Query a metrics snapshot for an allocation.
    pub async fn query_metrics(
        &mut self,
        req: pb::QueryMetricsRequest,
    ) -> Result<pb::MetricsSnapshot, tonic::Status> {
        let resp = self.allocations.query_metrics(req).await?;
        Ok(resp.into_inner())
    }

    /// Stream live metrics from an allocation (server-streaming).
    pub async fn stream_metrics(
        &mut self,
        req: pb::StreamMetricsRequest,
    ) -> Result<tonic::Streaming<pb::MetricsEvent>, tonic::Status> {
        let resp = self.allocations.stream_metrics(req).await?;
        Ok(resp.into_inner())
    }

    /// Get diagnostics for a running allocation.
    pub async fn get_diagnostics(
        &mut self,
        req: pb::DiagnosticsRequest,
    ) -> Result<pb::DiagnosticsResponse, tonic::Status> {
        let resp = self.allocations.get_diagnostics(req).await?;
        Ok(resp.into_inner())
    }

    /// Request a checkpoint of a running allocation.
    pub async fn checkpoint(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::CheckpointResponse, tonic::Status> {
        let req = pb::CheckpointRequest {
            allocation_id: allocation_id.to_string(),
        };
        let resp = self.allocations.checkpoint(req).await?;
        Ok(resp.into_inner())
    }

    /// Get DAG status.
    pub async fn get_dag(&mut self, dag_id: &str) -> Result<pb::DagStatus, tonic::Status> {
        let req = pb::GetDagRequest {
            dag_id: dag_id.to_string(),
        };
        let resp = self.allocations.get_dag(req).await?;
        Ok(resp.into_inner())
    }

    /// List DAGs with filters.
    pub async fn list_dags(
        &mut self,
        req: pb::ListDagsRequest,
    ) -> Result<pb::ListDagsResponse, tonic::Status> {
        let resp = self.allocations.list_dags(req).await?;
        Ok(resp.into_inner())
    }

    /// Cancel a DAG workflow.
    pub async fn cancel_dag(
        &mut self,
        dag_id: &str,
    ) -> Result<pb::CancelDagResponse, tonic::Status> {
        let req = pb::CancelDagRequest {
            dag_id: dag_id.to_string(),
        };
        let resp = self.allocations.cancel_dag(req).await?;
        Ok(resp.into_inner())
    }

    /// List nodes with filters.
    pub async fn list_nodes(
        &mut self,
        req: pb::ListNodesRequest,
    ) -> Result<pb::ListNodesResponse, tonic::Status> {
        let resp = self.nodes.list_nodes(req).await?;
        Ok(resp.into_inner())
    }

    /// Get a single node by ID.
    pub async fn get_node(&mut self, node_id: &str) -> Result<pb::NodeStatus, tonic::Status> {
        let req = pb::GetNodeRequest {
            node_id: node_id.to_string(),
        };
        let resp = self.nodes.get_node(req).await?;
        Ok(resp.into_inner())
    }

    /// Drain a node.
    pub async fn drain_node(
        &mut self,
        node_id: &str,
        reason: &str,
    ) -> Result<pb::DrainNodeResponse, tonic::Status> {
        let req = pb::DrainNodeRequest {
            node_id: node_id.to_string(),
            reason: reason.to_string(),
        };
        let resp = self.nodes.drain_node(req).await?;
        Ok(resp.into_inner())
    }

    /// Undrain a node.
    pub async fn undrain_node(
        &mut self,
        node_id: &str,
    ) -> Result<pb::UndrainNodeResponse, tonic::Status> {
        let req = pb::UndrainNodeRequest {
            node_id: node_id.to_string(),
        };
        let resp = self.nodes.undrain_node(req).await?;
        Ok(resp.into_inner())
    }

    /// Disable a node.
    pub async fn disable_node(
        &mut self,
        node_id: &str,
        reason: &str,
    ) -> Result<pb::DisableNodeResponse, tonic::Status> {
        let req = pb::DisableNodeRequest {
            node_id: node_id.to_string(),
            reason: reason.to_string(),
        };
        let resp = self.nodes.disable_node(req).await?;
        Ok(resp.into_inner())
    }

    /// Create a tenant.
    pub async fn create_tenant(
        &mut self,
        req: pb::CreateTenantRequest,
    ) -> Result<pb::TenantResponse, tonic::Status> {
        let resp = self.admin.create_tenant(req).await?;
        Ok(resp.into_inner())
    }

    /// Get Raft cluster status.
    pub async fn get_raft_status(&mut self) -> Result<pb::RaftStatusResponse, tonic::Status> {
        let req = pb::GetRaftStatusRequest {};
        let resp = self.admin.get_raft_status(req).await?;
        Ok(resp.into_inner())
    }

    /// Verify a backup.
    pub async fn backup_verify(
        &mut self,
        backup_path: &str,
    ) -> Result<pb::BackupVerifyResponse, tonic::Status> {
        let req = pb::BackupVerifyRequest {
            backup_path: backup_path.to_string(),
        };
        let resp = self.admin.backup_verify(req).await?;
        Ok(resp.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = ClientConfig::default();
        assert_eq!(config.api_endpoint, "http://localhost:50051");
        assert_eq!(config.timeout_secs, 30);
        assert!(config.tenant.is_none());
    }

    #[test]
    fn timeout_conversion() {
        let config = ClientConfig {
            timeout_secs: 60,
            ..Default::default()
        };
        assert_eq!(config.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn connect_invalid_endpoint_returns_error() {
        let config = ClientConfig {
            api_endpoint: "not a valid uri ://".to_string(),
            ..Default::default()
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(LatticeGrpcClient::connect(&config));
        assert!(result.is_err());
    }

    #[test]
    fn default_config_has_no_token() {
        let config = ClientConfig::default();
        assert!(config.token.is_none());
    }

    #[test]
    fn auth_interceptor_with_token_adds_authorization_header() {
        let mut interceptor = AuthInterceptor::new(Some("test-token-123".to_string()));
        let request = tonic::Request::new(());
        let result = interceptor.call(request).unwrap();
        let auth_value = result.metadata().get("authorization").unwrap();
        assert_eq!(auth_value.to_str().unwrap(), "Bearer test-token-123");
    }

    #[test]
    fn auth_interceptor_without_token_passes_through() {
        let mut interceptor = AuthInterceptor::new(None);
        let request = tonic::Request::new(());
        let result = interceptor.call(request).unwrap();
        assert!(result.metadata().get("authorization").is_none());
    }
}

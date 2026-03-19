//! gRPC client wrapper — delegates to lattice-client SDK.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use lattice_client::proto as pb;
pub use lattice_client::LatticeClientError;

/// CLI-specific configuration. Extends the SDK [`lattice_client::ClientConfig`]
/// with user/tenant/vcluster metadata needed for the CLI workflow.
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

    fn to_sdk_config(&self) -> lattice_client::ClientConfig {
        lattice_client::ClientConfig {
            endpoint: self.api_endpoint.clone(),
            timeout_secs: self.timeout_secs,
            token: self.token.clone(),
        }
    }
}

fn whoami() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
}

/// gRPC client that wraps the lattice-client SDK.
pub struct LatticeGrpcClient {
    inner: lattice_client::LatticeClient,
}

impl LatticeGrpcClient {
    /// Connect to the lattice-api server using the given configuration.
    pub async fn connect(config: &ClientConfig) -> Result<Self, anyhow::Error> {
        let inner = lattice_client::LatticeClient::connect(config.to_sdk_config()).await?;
        Ok(Self { inner })
    }

    // ─── Allocation Operations ──────────────────────────────

    pub async fn submit(
        &mut self,
        req: pb::SubmitRequest,
    ) -> Result<pb::SubmitResponse, tonic::Status> {
        self.inner.submit(req).await.map_err(to_status)
    }

    pub async fn get_allocation(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::AllocationStatus, tonic::Status> {
        self.inner
            .get_allocation(allocation_id)
            .await
            .map_err(to_status)
    }

    pub async fn list_allocations(
        &mut self,
        req: pb::ListAllocationsRequest,
    ) -> Result<pb::ListAllocationsResponse, tonic::Status> {
        self.inner.list_allocations(req).await.map_err(to_status)
    }

    pub async fn cancel(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::CancelResponse, tonic::Status> {
        self.inner.cancel(allocation_id).await.map_err(to_status)
    }

    pub async fn watch(
        &mut self,
        req: pb::WatchRequest,
    ) -> Result<tonic::Streaming<pb::AllocationEvent>, tonic::Status> {
        self.inner.watch(req).await.map_err(to_status)
    }

    pub async fn stream_logs(
        &mut self,
        req: pb::LogStreamRequest,
    ) -> Result<tonic::Streaming<pb::LogEntry>, tonic::Status> {
        self.inner.stream_logs(req).await.map_err(to_status)
    }

    pub async fn query_metrics(
        &mut self,
        req: pb::QueryMetricsRequest,
    ) -> Result<pb::MetricsSnapshot, tonic::Status> {
        self.inner.query_metrics(req).await.map_err(to_status)
    }

    pub async fn stream_metrics(
        &mut self,
        req: pb::StreamMetricsRequest,
    ) -> Result<tonic::Streaming<pb::MetricsEvent>, tonic::Status> {
        self.inner.stream_metrics(req).await.map_err(to_status)
    }

    pub async fn get_diagnostics(
        &mut self,
        req: pb::DiagnosticsRequest,
    ) -> Result<pb::DiagnosticsResponse, tonic::Status> {
        self.inner.get_diagnostics(req).await.map_err(to_status)
    }

    pub async fn checkpoint(
        &mut self,
        allocation_id: &str,
    ) -> Result<pb::CheckpointResponse, tonic::Status> {
        self.inner
            .checkpoint(allocation_id)
            .await
            .map_err(to_status)
    }

    pub async fn attach(
        &mut self,
        input: impl tokio_stream::Stream<Item = pb::AttachInput> + Send + 'static,
    ) -> Result<tonic::Streaming<pb::AttachOutput>, tonic::Status> {
        self.inner.attach(input).await.map_err(to_status)
    }

    // ─── DAG Operations ─────────────────────────────────────

    pub async fn get_dag(&mut self, dag_id: &str) -> Result<pb::DagStatus, tonic::Status> {
        self.inner.get_dag(dag_id).await.map_err(to_status)
    }

    pub async fn list_dags(
        &mut self,
        req: pb::ListDagsRequest,
    ) -> Result<pb::ListDagsResponse, tonic::Status> {
        self.inner.list_dags(req).await.map_err(to_status)
    }

    pub async fn cancel_dag(
        &mut self,
        dag_id: &str,
    ) -> Result<pb::CancelDagResponse, tonic::Status> {
        self.inner.cancel_dag(dag_id).await.map_err(to_status)
    }

    // ─── Node Operations ────────────────────────────────────

    pub async fn list_nodes(
        &mut self,
        req: pb::ListNodesRequest,
    ) -> Result<pb::ListNodesResponse, tonic::Status> {
        self.inner.list_nodes(req).await.map_err(to_status)
    }

    pub async fn get_node(&mut self, node_id: &str) -> Result<pb::NodeStatus, tonic::Status> {
        self.inner.get_node(node_id).await.map_err(to_status)
    }

    pub async fn drain_node(
        &mut self,
        node_id: &str,
        reason: &str,
    ) -> Result<pb::DrainNodeResponse, tonic::Status> {
        self.inner
            .drain_node(node_id, reason)
            .await
            .map_err(to_status)
    }

    pub async fn undrain_node(
        &mut self,
        node_id: &str,
    ) -> Result<pb::UndrainNodeResponse, tonic::Status> {
        self.inner.undrain_node(node_id).await.map_err(to_status)
    }

    pub async fn disable_node(
        &mut self,
        node_id: &str,
        reason: &str,
    ) -> Result<pb::DisableNodeResponse, tonic::Status> {
        self.inner
            .disable_node(node_id, reason)
            .await
            .map_err(to_status)
    }

    pub async fn enable_node(
        &mut self,
        node_id: &str,
    ) -> Result<pb::EnableNodeResponse, tonic::Status> {
        self.inner.enable_node(node_id).await.map_err(to_status)
    }

    // ─── Admin Operations ───────────────────────────────────

    pub async fn create_tenant(
        &mut self,
        req: pb::CreateTenantRequest,
    ) -> Result<pb::TenantResponse, tonic::Status> {
        self.inner.create_tenant(req).await.map_err(to_status)
    }

    pub async fn get_raft_status(&mut self) -> Result<pb::RaftStatusResponse, tonic::Status> {
        self.inner.raft_status().await.map_err(to_status)
    }

    pub async fn backup_verify(
        &mut self,
        backup_path: &str,
    ) -> Result<pb::BackupVerifyResponse, tonic::Status> {
        self.inner
            .verify_backup(backup_path)
            .await
            .map_err(to_status)
    }
}

/// Convert [`LatticeClientError`] back to [`tonic::Status`] for CLI backward
/// compatibility (existing command handlers expect `tonic::Status`).
fn to_status(err: lattice_client::LatticeClientError) -> tonic::Status {
    match err {
        lattice_client::LatticeClientError::Transport(e) => {
            tonic::Status::unavailable(e.to_string())
        }
        lattice_client::LatticeClientError::NotFound(msg) => tonic::Status::not_found(msg),
        lattice_client::LatticeClientError::Auth(msg) => tonic::Status::unauthenticated(msg),
        lattice_client::LatticeClientError::PermissionDenied(msg) => {
            tonic::Status::permission_denied(msg)
        }
        lattice_client::LatticeClientError::InvalidArgument(msg) => {
            tonic::Status::invalid_argument(msg)
        }
        lattice_client::LatticeClientError::Internal(msg) => tonic::Status::internal(msg),
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
    fn to_sdk_config_maps_fields() {
        let config = ClientConfig {
            api_endpoint: "http://example:50051".to_string(),
            timeout_secs: 45,
            token: Some("tok".to_string()),
            ..Default::default()
        };
        let sdk = config.to_sdk_config();
        assert_eq!(sdk.endpoint, "http://example:50051");
        assert_eq!(sdk.timeout_secs, 45);
        assert_eq!(sdk.token.as_deref(), Some("tok"));
    }
}

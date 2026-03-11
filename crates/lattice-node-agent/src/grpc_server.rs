//! gRPC server for the node agent.
//!
//! Implements the `NodeAgentService` defined in `agent.proto`, handling
//! allocation lifecycle, attach sessions, log streaming, and MPI process
//! management (LaunchProcesses, PmiFence, AbortProcesses).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::types::{CxiCredentials, LaunchId, PeerInfo, PmiMode};

use crate::pmi2::fence::{FenceCoordinator, FenceTransport};
use crate::process_launcher::{LaunchConfig, ProcessLauncher};

/// State for an active MPI launch on this node.
struct ActiveLaunch {
    fence_coordinator: Arc<FenceCoordinator>,
    abort_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Node agent gRPC service implementation.
pub struct NodeAgentServer {
    node_id: String,
    active_launches: Arc<Mutex<HashMap<LaunchId, ActiveLaunch>>>,
    fence_transport: Arc<dyn FenceTransport>,
}

impl NodeAgentServer {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            active_launches: Arc::new(Mutex::new(HashMap::new())),
            fence_transport: Arc::new(GrpcFenceTransport {}),
        }
    }

    /// Create with a custom fence transport (for testing).
    pub fn with_transport(node_id: String, transport: Arc<dyn FenceTransport>) -> Self {
        Self {
            node_id,
            active_launches: Arc::new(Mutex::new(HashMap::new())),
            fence_transport: transport,
        }
    }

    #[allow(clippy::result_large_err)]
    fn parse_launch_id(s: &str) -> Result<LaunchId, Status> {
        uuid::Uuid::parse_str(s)
            .map_err(|e| Status::invalid_argument(format!("invalid launch_id: {e}")))
    }
}

type StreamPin<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl pb::node_agent_service_server::NodeAgentService for NodeAgentServer {
    // ─── Allocation Lifecycle (existing stubs) ────────────

    async fn run_allocation(
        &self,
        request: Request<pb::RunAllocationRequest>,
    ) -> Result<Response<pb::RunAllocationResponse>, Status> {
        let req = request.into_inner();
        info!(alloc_id = %req.allocation_id, "RunAllocation received");
        Ok(Response::new(pb::RunAllocationResponse {
            accepted: true,
            message: "accepted".into(),
        }))
    }

    async fn stop_allocation(
        &self,
        request: Request<pb::StopAllocationRequest>,
    ) -> Result<Response<pb::StopAllocationResponse>, Status> {
        let req = request.into_inner();
        info!(alloc_id = %req.allocation_id, "StopAllocation received");
        Ok(Response::new(pb::StopAllocationResponse {
            success: true,
            message: "stopped".into(),
        }))
    }

    // ─── Attach (stub) ───────────────────────────────────

    type AttachStream = StreamPin<pb::AttachOutput>;

    async fn attach(
        &self,
        _request: Request<tonic::Streaming<pb::AttachInput>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        Err(Status::unimplemented("attach not yet implemented on agent"))
    }

    // ─── Log Streaming (stub) ────────────────────────────

    type StreamLogsStream = StreamPin<pb::LogEntry>;

    async fn stream_logs(
        &self,
        _request: Request<pb::LogStreamRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        Err(Status::unimplemented(
            "stream_logs not yet implemented on agent",
        ))
    }

    // ─── MPI: LaunchProcesses ────────────────────────────

    async fn launch_processes(
        &self,
        request: Request<pb::LaunchProcessesRequest>,
    ) -> Result<Response<pb::LaunchProcessesResponse>, Status> {
        let req = request.into_inner();
        let launch_id = Self::parse_launch_id(&req.launch_id)?;
        let allocation_id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation_id: {e}")))?;

        info!(
            launch_id = %launch_id,
            alloc_id = %allocation_id,
            ranks = req.tasks_per_node,
            first_rank = req.first_rank,
            world_size = req.world_size,
            "LaunchProcesses received"
        );

        let peers: Vec<PeerInfo> = req
            .peers
            .iter()
            .map(|p| PeerInfo {
                node_id: p.node_id.clone(),
                grpc_address: p.grpc_address.clone(),
                first_rank: p.first_rank,
                num_ranks: p.num_ranks,
            })
            .collect();

        let my_index = peers
            .iter()
            .position(|p| p.node_id == self.node_id)
            .unwrap_or(0) as u32;

        let cxi = req.cxi_credentials.as_ref().map(|creds| CxiCredentials {
            vni: creds.vni,
            auth_key: creds.auth_key.clone(),
            svc_id: creds.svc_id,
        });

        let nodelist = peers
            .iter()
            .map(|p| p.node_id.as_str())
            .collect::<Vec<_>>()
            .join(",");

        let launch_config = LaunchConfig {
            launch_id,
            allocation_id,
            entrypoint: req.entrypoint,
            args: req.args,
            env: req.env,
            tasks_per_node: req.tasks_per_node,
            first_rank: req.first_rank,
            world_size: req.world_size,
            pmi_mode: if req.pmi_mode == pb::PmiMode::Pmix as i32 {
                PmiMode::Pmix
            } else {
                PmiMode::Pmi2
            },
            cxi_credentials: cxi,
            peers: peers.clone(),
            head_node_index: req.head_node_index,
            my_node_index: my_index,
            node_id: self.node_id.clone(),
            socket_dir: std::env::temp_dir(),
            nodelist,
        };

        let transport = self.fence_transport.clone();
        let launcher = ProcessLauncher::new(launch_config, transport);

        // Spawn the launch in a background task
        let active_launches = self.active_launches.clone();
        let lid = launch_id;
        tokio::spawn(async move {
            let result = launcher.launch().await;
            info!(
                launch_id = %lid,
                success = result.success,
                "launch completed"
            );
            active_launches.lock().await.remove(&lid);
        });

        Ok(Response::new(pb::LaunchProcessesResponse {
            accepted: true,
            message: "launch started".into(),
        }))
    }

    // ─── MPI: PmiFence ──────────────────────────────────

    async fn pmi_fence(
        &self,
        request: Request<pb::PmiFenceRequest>,
    ) -> Result<Response<pb::PmiFenceResponse>, Status> {
        let req = request.into_inner();
        let launch_id = Self::parse_launch_id(&req.launch_id)?;

        let launches = self.active_launches.lock().await;
        let active = launches
            .get(&launch_id)
            .ok_or_else(|| Status::not_found(format!("no active launch {launch_id}")))?;

        let entries: HashMap<String, String> = req.kvs_entries;
        match active
            .fence_coordinator
            .receive_peer_fence(req.node_index, entries)
            .await
        {
            Ok(merged) => Ok(Response::new(pb::PmiFenceResponse {
                success: true,
                merged_kvs: merged,
            })),
            Err(e) => {
                // "waiting for more peers" is not an error — it means the
                // head is still collecting. In a real implementation, we'd
                // use a condvar/notify pattern. For now, return success=false.
                debug!(launch_id = %launch_id, error = %e, "fence pending");
                Ok(Response::new(pb::PmiFenceResponse {
                    success: false,
                    merged_kvs: HashMap::new(),
                }))
            }
        }
    }

    // ─── MPI: AbortProcesses ────────────────────────────

    async fn abort_processes(
        &self,
        request: Request<pb::AbortProcessesRequest>,
    ) -> Result<Response<pb::AbortProcessesResponse>, Status> {
        let req = request.into_inner();
        let launch_id = Self::parse_launch_id(&req.launch_id)?;

        warn!(launch_id = %launch_id, reason = %req.reason, "AbortProcesses received");

        let mut launches = self.active_launches.lock().await;
        if let Some(active) = launches.remove(&launch_id) {
            if let Some(handle) = active.abort_handle {
                handle.abort();
            }
        }

        Ok(Response::new(pb::AbortProcessesResponse { success: true }))
    }
}

/// Real gRPC fence transport (calls peer node agents).
struct GrpcFenceTransport;

#[async_trait::async_trait]
impl FenceTransport for GrpcFenceTransport {
    async fn send_fence(
        &self,
        peer_address: &str,
        launch_id: LaunchId,
        kvs_entries: HashMap<String, String>,
        node_index: u32,
    ) -> Result<HashMap<String, String>, String> {
        let mut client = pb::node_agent_service_client::NodeAgentServiceClient::connect(
            peer_address.to_string(),
        )
        .await
        .map_err(|e| format!("connect to {peer_address}: {e}"))?;

        let resp = client
            .pmi_fence(pb::PmiFenceRequest {
                launch_id: launch_id.to_string(),
                kvs_entries,
                node_index,
            })
            .await
            .map_err(|e| format!("PmiFence RPC to {peer_address}: {e}"))?;

        let inner = resp.into_inner();
        if inner.success {
            Ok(inner.merged_kvs)
        } else {
            Err("fence not yet complete on head".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb::node_agent_service_server::NodeAgentService;

    #[tokio::test]
    async fn run_allocation_accepted() {
        let server = NodeAgentServer::new("test-node".into());
        let resp = server
            .run_allocation(Request::new(pb::RunAllocationRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
                entrypoint: "echo".into(),
                uenv: String::new(),
                image: String::new(),
                gpu_count: 0,
                cpu_cores: 1,
                memory_bytes: 0,
            }))
            .await
            .unwrap();
        assert!(resp.into_inner().accepted);
    }

    #[tokio::test]
    async fn stop_allocation_succeeds() {
        let server = NodeAgentServer::new("test-node".into());
        let resp = server
            .stop_allocation(Request::new(pb::StopAllocationRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
                grace_period_seconds: 10,
                reason: "test".into(),
            }))
            .await
            .unwrap();
        assert!(resp.into_inner().success);
    }

    #[tokio::test]
    async fn launch_processes_validates_ids() {
        let server = NodeAgentServer::new("test-node".into());
        let result = server
            .launch_processes(Request::new(pb::LaunchProcessesRequest {
                launch_id: "not-a-uuid".into(),
                allocation_id: uuid::Uuid::new_v4().to_string(),
                entrypoint: "echo".into(),
                args: vec![],
                tasks_per_node: 1,
                first_rank: 0,
                world_size: 1,
                env: HashMap::new(),
                pmi_mode: 0,
                cxi_credentials: None,
                peers: vec![],
                head_node_index: 0,
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn abort_processes_unknown_launch_ok() {
        let server = NodeAgentServer::new("test-node".into());
        let resp = server
            .abort_processes(Request::new(pb::AbortProcessesRequest {
                launch_id: uuid::Uuid::new_v4().to_string(),
                reason: "test".into(),
            }))
            .await
            .unwrap();
        assert!(resp.into_inner().success);
    }

    #[tokio::test]
    async fn launch_processes_valid_request_accepted() {
        let server = NodeAgentServer::new("test-node".into());
        let launch_id = uuid::Uuid::new_v4();
        let alloc_id = uuid::Uuid::new_v4();

        let resp = server
            .launch_processes(Request::new(pb::LaunchProcessesRequest {
                launch_id: launch_id.to_string(),
                allocation_id: alloc_id.to_string(),
                entrypoint: "/bin/echo".into(),
                args: vec!["hello".into()],
                tasks_per_node: 1,
                first_rank: 0,
                world_size: 1,
                env: HashMap::new(),
                pmi_mode: pb::PmiMode::Pmi2 as i32,
                cxi_credentials: None,
                peers: vec![pb::PeerInfo {
                    node_id: "test-node".into(),
                    grpc_address: "http://test-node:50052".into(),
                    first_rank: 0,
                    num_ranks: 1,
                }],
                head_node_index: 0,
            }))
            .await
            .unwrap();

        let inner = resp.into_inner();
        assert!(inner.accepted);
        assert!(!inner.message.is_empty());
    }

    #[tokio::test]
    async fn launch_processes_invalid_allocation_id() {
        let server = NodeAgentServer::new("test-node".into());
        let result = server
            .launch_processes(Request::new(pb::LaunchProcessesRequest {
                launch_id: uuid::Uuid::new_v4().to_string(),
                allocation_id: "not-a-uuid".into(),
                entrypoint: "echo".into(),
                args: vec![],
                tasks_per_node: 1,
                first_rank: 0,
                world_size: 1,
                env: HashMap::new(),
                pmi_mode: 0,
                cxi_credentials: None,
                peers: vec![],
                head_node_index: 0,
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn abort_processes_invalid_launch_id() {
        let server = NodeAgentServer::new("test-node".into());
        let result = server
            .abort_processes(Request::new(pb::AbortProcessesRequest {
                launch_id: "not-a-uuid".into(),
                reason: "test".into(),
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn pmi_fence_invalid_launch_id() {
        let server = NodeAgentServer::new("test-node".into());
        let result = server
            .pmi_fence(Request::new(pb::PmiFenceRequest {
                launch_id: "not-a-uuid".into(),
                kvs_entries: HashMap::new(),
                node_index: 0,
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }
}

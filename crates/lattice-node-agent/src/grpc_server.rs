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
use lattice_common::types::{
    AllocId, CompletionPhase, CompletionReport, CxiCredentials, LaunchId, PeerInfo, PmiMode,
    RuntimeVariant,
};

use crate::allocation_runner::{AllocationManager, CompletionBuffer};
use crate::pmi2::fence::{FenceCoordinator, FenceTransport};
use crate::process_launcher::{LaunchConfig, ProcessLauncher};
use crate::runtime::{BareProcessRuntime, PrepareConfig, Runtime};

/// State for an active MPI launch on this node.
struct ActiveLaunch {
    fence_coordinator: Arc<FenceCoordinator>,
    abort_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Optional dispatch bridge (Impl 5). When configured, the gRPC server's
/// `run_allocation` handler actually registers with the AllocationManager,
/// selects a Runtime, spawns the entrypoint, and emits Completion Reports.
/// When not configured, `run_allocation` returns a stub acceptance (used
/// by unit tests that exercise the gRPC surface in isolation).
pub struct DispatchBridge {
    pub allocations: Arc<Mutex<AllocationManager>>,
    pub reports: CompletionBuffer,
    pub bare: Arc<BareProcessRuntime>,
    // Uenv/Podman runtimes are wired by the agent main.rs when enabled;
    // we keep them as trait objects so the server doesn't depend on their
    // concrete types. For now, Bare-Process is sufficient for the OV suite
    // scenarios; additional runtimes slot in here.
    pub uenv: Option<Arc<dyn Runtime>>,
    pub podman: Option<Arc<dyn Runtime>>,
}

/// Node agent gRPC service implementation.
pub struct NodeAgentServer {
    node_id: String,
    active_launches: Arc<Mutex<HashMap<LaunchId, ActiveLaunch>>>,
    fence_transport: Arc<dyn FenceTransport>,
    /// When set, dispatch is real (Impl 5). When None, run_allocation is
    /// a stub that returns accepted without spawning.
    dispatch: Option<Arc<DispatchBridge>>,
}

impl NodeAgentServer {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            active_launches: Arc::new(Mutex::new(HashMap::new())),
            fence_transport: Arc::new(GrpcFenceTransport {}),
            dispatch: None,
        }
    }

    /// Create with a custom fence transport (for testing).
    pub fn with_transport(node_id: String, transport: Arc<dyn FenceTransport>) -> Self {
        Self {
            node_id,
            active_launches: Arc::new(Mutex::new(HashMap::new())),
            fence_transport: transport,
            dispatch: None,
        }
    }

    /// Enable real dispatch (Impl 5). Without this, run_allocation stubs.
    pub fn with_dispatch(mut self, dispatch: DispatchBridge) -> Self {
        self.dispatch = Some(Arc::new(dispatch));
        self
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

        // Parse allocation_id.
        let alloc_id = match uuid::Uuid::parse_str(&req.allocation_id) {
            Ok(id) => id,
            Err(e) => {
                return Ok(Response::new(pb::RunAllocationResponse {
                    accepted: false,
                    message: format!("invalid allocation_id: {e}"),
                    refusal_reason: Some(pb::RefusalReason::RefusalMalformedRequest as i32),
                }));
            }
        };

        // No dispatch bridge wired: stub acceptance for isolated unit tests.
        let Some(dispatch) = self.dispatch.as_ref().cloned() else {
            return Ok(Response::new(pb::RunAllocationResponse {
                accepted: true,
                message: "accepted (stub: no dispatch bridge wired)".into(),
                refusal_reason: None,
            }));
        };

        // Decide which Runtime to drive. The Dispatcher has already told us
        // the allocation's shape via the RunAllocationRequest's fields; we
        // infer the variant by precedence: image > uenv > bare. This is a
        // pure function of `req`, so we compute it without holding any lock.
        let variant = if !req.image.trim().is_empty() {
            RuntimeVariant::Podman
        } else if !req.uenv.trim().is_empty() {
            RuntimeVariant::Uenv
        } else {
            RuntimeVariant::BareProcess
        };

        // For Uenv and Podman, the DispatchBridge must have the concrete
        // runtime wired. Otherwise we return UNSUPPORTED_CAPABILITY so the
        // Dispatcher can rollback and re-place on a capable node.
        let runtime: Arc<dyn Runtime> = match variant {
            RuntimeVariant::BareProcess => dispatch.bare.clone() as Arc<dyn Runtime>,
            RuntimeVariant::Uenv => match dispatch.uenv.as_ref() {
                Some(rt) => rt.clone(),
                None => {
                    return Ok(Response::new(pb::RunAllocationResponse {
                        accepted: false,
                        message: "uenv runtime not available on this agent".into(),
                        refusal_reason: Some(
                            pb::RefusalReason::RefusalUnsupportedCapability as i32,
                        ),
                    }));
                }
            },
            RuntimeVariant::Podman => match dispatch.podman.as_ref() {
                Some(rt) => rt.clone(),
                None => {
                    return Ok(Response::new(pb::RunAllocationResponse {
                        accepted: false,
                        message: "podman runtime not available on this agent".into(),
                        refusal_reason: Some(
                            pb::RefusalReason::RefusalUnsupportedCapability as i32,
                        ),
                    }));
                }
            },
        };

        // INV-D3 idempotency + start: hold the lock across the
        // contains_active check AND the start call to avoid a TOCTOU race
        // between concurrent RunAllocation calls for the same allocation
        // (D-ADV-IMPL-10 fix).
        {
            let mut mgr = dispatch.allocations.lock().await;
            if mgr.contains_active(&alloc_id) {
                return Ok(Response::new(pb::RunAllocationResponse {
                    accepted: true,
                    message: "already_running".into(),
                    refusal_reason: Some(pb::RefusalReason::RefusalAlreadyRunning as i32),
                }));
            }
            if let Err(e) = mgr.start(alloc_id, req.entrypoint.clone()) {
                // Should not happen given the contains_active guard under
                // the same lock, but we still handle it for belt-and-
                // suspenders: a different caller inserted under a prior
                // lock-cycle.
                debug!(alloc_id = %alloc_id, error = %e, "start rejected — idempotent path");
                return Ok(Response::new(pb::RunAllocationResponse {
                    accepted: true,
                    message: "already_running (race)".into(),
                    refusal_reason: Some(pb::RefusalReason::RefusalAlreadyRunning as i32),
                }));
            }
        }

        // Spawn the monitor task. It runs prologue → spawn → wait →
        // epilogue, emitting Completion Reports into the shared buffer at
        // each phase transition. We do NOT block the RPC response on this.
        let node_id = self.node_id.clone();
        let allocations = dispatch.allocations.clone();
        let reports = dispatch.reports.clone();
        let entrypoint = req.entrypoint.clone();
        // Fix for D-ADV-IMPL-02: split the entrypoint string into program
        // + args before calling Runtime::spawn. The RunAllocationRequest
        // carries a whole command line as a single string; Rust's
        // `Command::new()` expects only the program path. Use shell-style
        // quoted-word parsing so `sh -c 'exit 1'` reaches the runtime as
        // three argv slots instead of four.
        let parts: Vec<String> = parse_shell_words(&entrypoint);
        let (program, args) = match parts.split_first() {
            Some((head, tail)) => (head.clone(), tail.to_vec()),
            None => {
                warn!(
                    alloc_id = %alloc_id,
                    "RunAllocation entrypoint was empty after whitespace split"
                );
                // Remove the partial registration so a retry can succeed.
                let mut mgr = dispatch.allocations.lock().await;
                let _ = mgr.fail(&alloc_id, "empty_entrypoint".into());
                return Ok(Response::new(pb::RunAllocationResponse {
                    accepted: false,
                    message: "empty_entrypoint".into(),
                    refusal_reason: Some(pb::RefusalReason::RefusalMalformedRequest as i32),
                }));
            }
        };
        let uenv_spec = if req.uenv.trim().is_empty() {
            None
        } else {
            Some(req.uenv.clone())
        };
        let image_spec = if req.image.trim().is_empty() {
            None
        } else {
            Some(req.image.clone())
        };
        tokio::spawn(async move {
            run_allocation_monitor(
                alloc_id,
                node_id,
                program,
                args,
                uenv_spec,
                image_spec,
                runtime,
                allocations,
                reports,
            )
            .await;
        });

        Ok(Response::new(pb::RunAllocationResponse {
            accepted: true,
            message: "accepted".into(),
            refusal_reason: None,
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
/// Background monitor for a dispatched allocation.
///
/// Drives the Runtime lifecycle (prologue → spawn → wait → epilogue) and
/// emits Completion Reports into the shared `CompletionBuffer` at each
/// phase transition. Called by `run_allocation` from a detached task so
/// the RPC returns promptly after registering the allocation.
#[allow(clippy::too_many_arguments)]
async fn run_allocation_monitor(
    alloc_id: AllocId,
    node_id: String,
    entrypoint: String,
    args: Vec<String>,
    uenv: Option<String>,
    image: Option<String>,
    runtime: Arc<dyn Runtime>,
    allocations: Arc<Mutex<AllocationManager>>,
    reports: CompletionBuffer,
) {
    // ── Prologue ────────────────────────────────────────────────
    // Propagate uenv/image from the RunAllocationRequest so the selected
    // Runtime (Uenv / Podman) can actually prepare (IMPL-03/IMPL-09 fix).
    let prepare_config = PrepareConfig {
        alloc_id,
        uenv: uenv.clone(),
        view: None,
        image: image.clone(),
        workdir: None,
        env_vars: Vec::new(),
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: Vec::new(),
        scratch_per_node: None,
        resource_limits: None,
        images: Vec::new(),
        env_patches: Vec::new(),
    };
    if let Err(e) = runtime.prepare(&prepare_config).await {
        warn!(
            alloc_id = %alloc_id,
            node = %node_id,
            error = %e,
            "prologue failed; emitting Failed Completion Report"
        );
        reports.push(CompletionReport {
            allocation_id: alloc_id,
            phase: CompletionPhase::Failed,
            pid: None,
            exit_code: None,
            reason: Some(format!("prepare_failed: {e}")),
        });
        let mut mgr = allocations.lock().await;
        let _ = mgr.fail(&alloc_id, format!("prepare_failed: {e}"));
        return;
    }
    // Emit Staging (phase after Prologue).
    reports.push(CompletionReport {
        allocation_id: alloc_id,
        phase: CompletionPhase::Staging,
        pid: None,
        exit_code: None,
        reason: None,
    });

    // ── Spawn ────────────────────────────────────────────────────
    let handle = match runtime.spawn(alloc_id, &entrypoint, &args).await {
        Ok(h) => h,
        Err(e) => {
            warn!(
                alloc_id = %alloc_id,
                node = %node_id,
                error = %e,
                "spawn failed; emitting Failed Completion Report"
            );
            reports.push(CompletionReport {
                allocation_id: alloc_id,
                phase: CompletionPhase::Failed,
                pid: None,
                exit_code: None,
                reason: Some(format!("spawn_failed: {e}")),
            });
            let mut mgr = allocations.lock().await;
            let _ = mgr.fail(&alloc_id, format!("spawn_failed: {e}"));
            return;
        }
    };
    // Update local phase + emit Running report with pid.
    // INT-4: wire the workload pid into the LocalAllocation so attach
    // (nsenter) and reattach (state persistence) can find it. The runtime
    // owns the ProcessHandle; before this, the pid was only emitted via
    // the one-shot Completion Report to the quorum, so no agent-local
    // consumer could look it up.
    {
        let mut mgr = allocations.lock().await;
        // Prologue → Running
        let _ = mgr.advance(&alloc_id);
        if let Some(pid) = handle.pid {
            let _ = mgr.set_pid(&alloc_id, pid);
        }
    }
    reports.push(CompletionReport {
        allocation_id: alloc_id,
        phase: CompletionPhase::Running,
        pid: handle.pid,
        exit_code: None,
        reason: None,
    });
    debug!(alloc_id = %alloc_id, pid = ?handle.pid, "workload running");

    // ── Wait ─────────────────────────────────────────────────────
    match runtime.wait(&handle).await {
        Ok(status) => {
            let (phase, code, reason) = classify_exit(&status);
            debug!(
                alloc_id = %alloc_id,
                exit_status = ?status,
                "workload exited"
            );
            let _ = runtime.cleanup(alloc_id).await;
            {
                let mut mgr = allocations.lock().await;
                if phase == CompletionPhase::Failed {
                    let _ = mgr.fail(&alloc_id, reason.clone().unwrap_or_default());
                } else {
                    // Running → Epilogue → Completed
                    let _ = mgr.advance(&alloc_id);
                    let _ = mgr.advance(&alloc_id);
                }
            }
            reports.push(CompletionReport {
                allocation_id: alloc_id,
                phase,
                pid: handle.pid,
                exit_code: code,
                reason,
            });
        }
        Err(e) => {
            warn!(
                alloc_id = %alloc_id,
                error = %e,
                "wait() failed; emitting Failed Completion Report"
            );
            reports.push(CompletionReport {
                allocation_id: alloc_id,
                phase: CompletionPhase::Failed,
                pid: handle.pid,
                exit_code: None,
                reason: Some(format!("wait_failed: {e}")),
            });
            let mut mgr = allocations.lock().await;
            let _ = mgr.fail(&alloc_id, format!("wait_failed: {e}"));
        }
    }
}

/// Shell-style word splitter for allocation entrypoints. Handles
/// single-quoted and double-quoted substrings (without variable
/// expansion), plus `\`-escape of quote characters. Not a full POSIX
/// parser — no subshells, no env expansion, no heredocs — but enough
/// to let `sh -c 'exit 1'` or `bash -lc "echo hi && sleep 5"` reach
/// the runtime with the correct argv.
pub(crate) fn parse_shell_words(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_token = false;

    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                current.push(c);
            }
            has_token = true;
            continue;
        }
        if in_double {
            match c {
                '"' => in_double = false,
                '\\' => {
                    if let Some(&next) = chars.peek() {
                        if next == '"' || next == '\\' {
                            current.push(next);
                            chars.next();
                        } else {
                            current.push('\\');
                        }
                    } else {
                        current.push('\\');
                    }
                }
                _ => current.push(c),
            }
            has_token = true;
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                has_token = true;
            }
            '"' => {
                in_double = true;
                has_token = true;
            }
            '\\' => {
                if let Some(&next) = chars.peek() {
                    current.push(next);
                    chars.next();
                    has_token = true;
                }
            }
            c if c.is_whitespace() => {
                if has_token {
                    out.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            _ => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(current);
    }
    out
}

fn classify_exit(
    status: &crate::runtime::ExitStatus,
) -> (CompletionPhase, Option<i32>, Option<String>) {
    use crate::runtime::ExitStatus;
    match status {
        ExitStatus::Code(0) => (CompletionPhase::Completed, Some(0), None),
        ExitStatus::Code(c) => (
            CompletionPhase::Failed,
            Some(*c),
            Some(format!("non_zero_exit: {c}")),
        ),
        ExitStatus::Signal(s) => (
            CompletionPhase::Failed,
            Some(-(*s)),
            Some(format!("killed_by_signal: {s}")),
        ),
        ExitStatus::Unknown => (
            CompletionPhase::Failed,
            None,
            Some("exit_status_unknown".into()),
        ),
    }
}

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

    #[test]
    fn shell_words_plain_split() {
        assert_eq!(
            parse_shell_words("echo hello world"),
            vec!["echo", "hello", "world"]
        );
    }

    #[test]
    fn shell_words_single_quoted_preserves_spaces() {
        assert_eq!(
            parse_shell_words("sh -c 'exit 1'"),
            vec!["sh", "-c", "exit 1"]
        );
    }

    #[test]
    fn shell_words_double_quoted_with_spaces() {
        assert_eq!(
            parse_shell_words(r#"bash -c "echo hi && sleep 5""#),
            vec!["bash", "-c", "echo hi && sleep 5"]
        );
    }

    #[test]
    fn shell_words_backslash_escapes() {
        assert_eq!(
            parse_shell_words(r"echo a\ b"),
            vec!["echo", "a b"]
        );
    }

    #[test]
    fn shell_words_empty_is_empty() {
        assert!(parse_shell_words("").is_empty());
        assert!(parse_shell_words("   ").is_empty());
    }

    #[test]
    fn shell_words_mixed_quotes() {
        assert_eq!(
            parse_shell_words(r#"sh -c "echo 'nested quote'""#),
            vec!["sh", "-c", "echo 'nested quote'"]
        );
    }

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

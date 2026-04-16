//! # Dispatcher
//!
//! Control-plane component that bridges the Scheduling context to the Node
//! Management context: it observes Running-but-un-acked allocations in
//! GlobalState, calls `NodeAgentService.RunAllocation` on the target
//! agents, retries with bounded backoff on failure, and submits a
//! `RollbackDispatch` Raft proposal when the retry budget is exhausted.
//!
//! Per DEC-DISP-04, the Dispatcher runs ONLY on the Raft leader. Followers
//! do not dispatch. Leadership change causes a pause of `heartbeat_interval`
//! before the new leader's Dispatcher begins its first tick, giving
//! in-flight Completion Reports from the previous leader's attempts time
//! to land and advance allocation state.
//!
//! Per DEC-DISP-07, rollback is all-or-nothing: when dispatch fails on any
//! node of a multi-node allocation, ALL assigned nodes are released and
//! the allocation returns to Pending (or Failed at the retry cap). Per
//! DEC-DISP-08, the Dispatcher fires best-effort `StopAllocation` to every
//! agent that had accepted a prior attempt, so any in-flight prologue is
//! reliably torn down.
//!
//! Per INV-D10, the target `agent_address` is re-read from GlobalState at
//! the start of each retry attempt — no caching.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::traits::{AllocationFilter, AllocationStore};
use lattice_common::types::{AllocId, Allocation, AllocationState, NodeId, RefusalReason};
use lattice_quorum::{QuorumClient, QuorumCommand, QuorumResponse};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Configurable knobs for Dispatcher behavior (DispatcherConfig in spec).
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// Delay before the tick loop begins after leadership acquisition
    /// (DEC-DISP-04). Default: one heartbeat_interval (10s).
    pub leadership_pause: Duration,
    /// How often the Dispatcher wakes to look for pending dispatches.
    pub tick_interval: Duration,
    /// Attempt timeout for a single RunAllocation RPC.
    pub attempt_timeout: Duration,
    /// Per-attempt backoff schedule on one node (len N = N retries).
    pub attempt_backoff: Vec<Duration>,
    /// Max concurrent attempts in flight globally.
    pub max_concurrent_attempts: usize,
    /// Max concurrent attempts targeting one agent.
    pub per_agent_concurrency: usize,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            leadership_pause: Duration::from_secs(10),
            tick_interval: Duration::from_secs(1),
            attempt_timeout: Duration::from_secs(1),
            attempt_backoff: vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(5),
            ],
            max_concurrent_attempts: 64,
            per_agent_concurrency: 8,
        }
    }
}

/// Per-node dispatch state (tracks whether the agent accepted a prior
/// attempt, so cleanup knows who to send StopAllocation to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerNodeDispatchState {
    NotYetAttempted,
    InFlight,
    Accepted,
    FailedFinal,
}

/// Single Dispatch Attempt outcome (internal).
#[derive(Debug)]
enum DispatchOutcome {
    /// Agent accepted; phase transitions follow via Completion Reports.
    Accepted,
    /// Agent already has this allocation (INV-D3 short-circuit) — treat as success.
    AcceptedAlreadyRunning,
    /// Transient failure (connect, timeout). Retry within budget.
    TransientFailure(String),
    /// Agent busy; retry within budget with backoff.
    RefusalBusy(String),
    /// Capability mismatch. Rollback to Pending immediately so the
    /// scheduler can re-place on a capable node.
    RefusalUnsupported(String),
    /// Malformed request. Transition allocation to Failed immediately.
    RefusalMalformed(String),
}

/// Session state per allocation (tracked in-memory; lost on leadership
/// flap. D-ADV-ARCH-04 mitigated via leadership_pause).
#[derive(Debug, Default)]
struct DispatchSession {
    per_node: HashMap<NodeId, PerNodeDispatchState>,
}

/// Dispatcher component. One instance per lattice-api process; only the
/// Raft leader's instance runs its tick loop.
pub struct Dispatcher {
    quorum: Arc<QuorumClient>,
    config: DispatcherConfig,
    /// Sessions keyed by allocation id.
    sessions: Arc<Mutex<HashMap<AllocId, DispatchSession>>>,
    /// Whether this Dispatcher is the Raft leader (DEC-DISP-04).
    is_leader: Arc<std::sync::atomic::AtomicBool>,
    /// When leadership was acquired (Instant). Used to enforce
    /// `leadership_pause` before first tick.
    leader_since: Arc<Mutex<Option<Instant>>>,
    /// Global concurrency cap on in-flight Dispatch Attempts
    /// (DEC-DISP-12 / D-ADV-IMPL-05 fix).
    attempt_semaphore: Arc<tokio::sync::Semaphore>,
    /// Per-agent concurrency caps, keyed by node_id.
    per_agent_semaphores: Arc<Mutex<HashMap<NodeId, Arc<tokio::sync::Semaphore>>>>,
}

impl Dispatcher {
    pub fn new(quorum: Arc<QuorumClient>, config: DispatcherConfig) -> Self {
        let attempt_semaphore =
            Arc::new(tokio::sync::Semaphore::new(config.max_concurrent_attempts));
        Self {
            quorum,
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            is_leader: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            leader_since: Arc::new(Mutex::new(None)),
            attempt_semaphore,
            per_agent_semaphores: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// DEC-DISP-04: called externally on Raft leadership transitions.
    /// When becoming leader, starts the leadership_pause timer. When losing
    /// leadership, halts new attempts (in-flight attempts continue; INV-D3
    /// absorbs duplicates).
    pub async fn on_leadership_change(&self, is_leader: bool) {
        self.is_leader
            .store(is_leader, std::sync::atomic::Ordering::Relaxed);
        let mut guard = self.leader_since.lock().await;
        *guard = if is_leader {
            Some(Instant::now())
        } else {
            None
        };
        info!(is_leader, "Dispatcher leadership change");
    }

    /// Single Dispatcher-loop iteration. Observes un-acked Running
    /// allocations and issues dispatch attempts. Returns the number of
    /// allocations acted on this iteration.
    pub async fn tick(&self) -> Result<usize, String> {
        // Leader gate (DEC-DISP-04).
        if !self.is_leader.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(0);
        }
        // Leadership-pause gate: skip until enough time has elapsed since
        // becoming leader (DEC-DISP-04 / D-ADV-ARCH-04 resolution).
        {
            let guard = self.leader_since.lock().await;
            if let Some(since) = *guard {
                if since.elapsed() < self.config.leadership_pause {
                    debug!("Dispatcher still in leadership pause window");
                    return Ok(0);
                }
            }
        }

        // Observe pending dispatches from GlobalState.
        let to_dispatch = self.pending_dispatches().await?;
        let count = to_dispatch.len();
        for (alloc, nodes) in to_dispatch {
            // Spawn one attempt-drive per allocation. Use a simple semaphore
            // via task spawning; a more elaborate rate limiter is a future
            // refinement (DEC-DISP-05 per_agent_concurrency).
            let dispatcher = self.clone_handles();
            tokio::spawn(async move {
                dispatcher.drive_allocation(alloc, nodes).await;
            });
        }
        Ok(count)
    }

    fn clone_handles(&self) -> DispatcherHandle {
        DispatcherHandle {
            quorum: self.quorum.clone(),
            config: self.config.clone(),
            sessions: self.sessions.clone(),
            attempt_semaphore: self.attempt_semaphore.clone(),
            per_agent_semaphores: self.per_agent_semaphores.clone(),
        }
    }

    /// Observe all Running allocations with non-empty assigned_nodes
    /// whose phase has not yet advanced (i.e., no Completion Report has
    /// set last_completion_report_at).
    async fn pending_dispatches(&self) -> Result<Vec<(Allocation, Vec<NodeId>)>, String> {
        let filter = AllocationFilter {
            state: Some(AllocationState::Running),
            tenant: None,
            vcluster: None,
            user: None,
        };
        let running = <QuorumClient as AllocationStore>::list(self.quorum.as_ref(), &filter)
            .await
            .map_err(|e| format!("list allocations: {e}"))?;

        let mut out = Vec::new();
        for alloc in running {
            if alloc.assigned_nodes.is_empty() {
                continue;
            }
            if alloc.last_completion_report_at.is_some() {
                // Agent already reported progress; nothing for the
                // Dispatcher to do.
                continue;
            }
            let nodes = alloc.assigned_nodes.clone();
            out.push((alloc, nodes));
        }
        Ok(out)
    }
}

/// Lightweight Dispatcher handle with the bits needed inside a spawned
/// attempt-drive task. Cheap to clone.
#[derive(Clone)]
struct DispatcherHandle {
    quorum: Arc<QuorumClient>,
    config: DispatcherConfig,
    sessions: Arc<Mutex<HashMap<AllocId, DispatchSession>>>,
    attempt_semaphore: Arc<tokio::sync::Semaphore>,
    per_agent_semaphores: Arc<Mutex<HashMap<NodeId, Arc<tokio::sync::Semaphore>>>>,
}

impl DispatcherHandle {
    /// Acquire the per-agent semaphore, creating it on first use.
    async fn per_agent_permit(&self, node_id: &NodeId) -> tokio::sync::OwnedSemaphorePermit {
        let sem = {
            let mut guard = self.per_agent_semaphores.lock().await;
            guard
                .entry(node_id.clone())
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Semaphore::new(
                        self.config.per_agent_concurrency,
                    ))
                })
                .clone()
        };
        sem.acquire_owned()
            .await
            .expect("per-agent semaphore closed")
    }
}

impl DispatcherHandle {
    /// Drive dispatch for one allocation across its assigned nodes. On
    /// per-node success, marks the per-node state Accepted and waits for
    /// the agent's Completion Reports via the heartbeat path. On per-node
    /// failure after the attempt budget is exhausted, declares Dispatch
    /// Failure and submits the all-or-nothing RollbackDispatch (DEC-DISP-07).
    async fn drive_allocation(self, alloc: Allocation, nodes: Vec<NodeId>) {
        // Initialize session.
        {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.entry(alloc.id).or_default();
            for n in &nodes {
                session
                    .per_node
                    .entry(n.clone())
                    .or_insert(PerNodeDispatchState::NotYetAttempted);
            }
        }

        let mut overall_failed_node: Option<NodeId> = None;
        for node_id in &nodes {
            match self.dispatch_one_with_retry(&alloc, node_id).await {
                DispatchOutcome::Accepted | DispatchOutcome::AcceptedAlreadyRunning => {
                    self.mark_accepted(&alloc.id, node_id).await;
                }
                other => {
                    warn!(
                        alloc = %alloc.id,
                        node = %node_id,
                        outcome = ?other,
                        "dispatch failed on node — triggering all-or-nothing rollback"
                    );
                    overall_failed_node = Some(node_id.clone());
                    break;
                }
            }
        }

        if let Some(failed) = overall_failed_node {
            self.do_rollback(&alloc, &nodes, &failed, "dispatch_failed".into())
                .await;
        }
    }

    async fn dispatch_one_with_retry(
        &self,
        alloc: &Allocation,
        node_id: &NodeId,
    ) -> DispatchOutcome {
        for (i, backoff) in self.config.attempt_backoff.iter().enumerate() {
            // INV-D10: re-read agent_address on each attempt.
            let addr = match self.resolve_address(node_id).await {
                Ok(a) => a,
                Err(e) => {
                    debug!(
                        alloc = %alloc.id,
                        node = %node_id,
                        attempt = i,
                        error = %e,
                        "address resolution failed"
                    );
                    tokio::time::sleep(*backoff).await;
                    continue;
                }
            };
            self.mark_in_flight(&alloc.id, node_id).await;
            // DEC-DISP-12 / D-ADV-IMPL-05 rate limiting: acquire both
            // global and per-agent permits before issuing the RPC.
            let _global_permit = self
                .attempt_semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("attempt semaphore closed");
            let _per_agent_permit = self.per_agent_permit(node_id).await;
            let outcome = self.attempt_single(alloc, node_id, &addr).await;
            let result_label = match &outcome {
                DispatchOutcome::Accepted => "accepted",
                DispatchOutcome::AcceptedAlreadyRunning => "already_running",
                DispatchOutcome::TransientFailure(_) => "transient",
                DispatchOutcome::RefusalBusy(_) => "refusal_busy",
                DispatchOutcome::RefusalUnsupported(_) => "refusal_unsupported",
                DispatchOutcome::RefusalMalformed(_) => "refusal_malformed",
            };
            lattice_common::metrics::record_dispatch_attempt(node_id, result_label);
            match outcome {
                DispatchOutcome::Accepted => return DispatchOutcome::Accepted,
                DispatchOutcome::AcceptedAlreadyRunning => {
                    return DispatchOutcome::AcceptedAlreadyRunning
                }
                DispatchOutcome::RefusalUnsupported(reason) => {
                    // Short-circuit: no retry budget consumed. Caller
                    // rolls back to Pending so scheduler re-places.
                    return DispatchOutcome::RefusalUnsupported(reason);
                }
                DispatchOutcome::RefusalMalformed(reason) => {
                    return DispatchOutcome::RefusalMalformed(reason);
                }
                DispatchOutcome::TransientFailure(e) | DispatchOutcome::RefusalBusy(e) => {
                    debug!(
                        alloc = %alloc.id,
                        node = %node_id,
                        attempt = i,
                        error = %e,
                        "dispatch attempt failed, backing off"
                    );
                    tokio::time::sleep(*backoff).await;
                }
            }
        }
        DispatchOutcome::TransientFailure("retry budget exhausted".into())
    }

    /// Perform one RunAllocation RPC. Parses response into DispatchOutcome.
    async fn attempt_single(
        &self,
        alloc: &Allocation,
        node_id: &NodeId,
        addr: &str,
    ) -> DispatchOutcome {
        let endpoint = if addr.starts_with("http://") || addr.starts_with("https://") {
            addr.to_string()
        } else {
            format!("http://{addr}")
        };
        let client =
            match pb::node_agent_service_client::NodeAgentServiceClient::connect(endpoint.clone())
                .await
            {
                Ok(c) => c,
                Err(e) => return DispatchOutcome::TransientFailure(format!("connect: {e}")),
            };
        let mut client = client;
        // Fix for D-ADV-IMPL-03: extract uenv + image from the allocation's
        // environment so the agent can select the right Runtime variant.
        // One Uenv image OR one OCI image is allowed per INV-D14-adjacent
        // runtime-selection rules; if both, select_runtime_variant on the
        // agent side returns MalformedRequest.
        use lattice_common::types::ImageType;
        let mut uenv_spec = String::new();
        let mut image_spec = String::new();
        for img in &alloc.environment.images {
            match img.image_type {
                ImageType::Uenv if uenv_spec.is_empty() => {
                    uenv_spec = img.spec.clone();
                }
                ImageType::Oci if image_spec.is_empty() => {
                    image_spec = img.spec.clone();
                }
                _ => {}
            }
        }
        let req = tonic::Request::new(pb::RunAllocationRequest {
            allocation_id: alloc.id.to_string(),
            entrypoint: alloc.entrypoint.clone(),
            uenv: uenv_spec,
            image: image_spec,
            gpu_count: 0, // simplified; scheduler already validated capability
            cpu_cores: 0,
            memory_bytes: 0,
        });
        let _ = node_id; // logging only
        let resp =
            match tokio::time::timeout(self.config.attempt_timeout, client.run_allocation(req))
                .await
            {
                Ok(Ok(r)) => r.into_inner(),
                Ok(Err(e)) => return DispatchOutcome::TransientFailure(format!("rpc: {e}")),
                Err(_) => return DispatchOutcome::TransientFailure("attempt_timeout".into()),
            };
        if !resp.accepted {
            // Map proto RefusalReason (or unknown → MalformedRequest per
            // D-ADV-ARCH-10).
            let reason_kind = map_refusal_reason(resp.refusal_reason);
            return match reason_kind {
                Some(RefusalReason::Busy) => DispatchOutcome::RefusalBusy(resp.message),
                Some(RefusalReason::UnsupportedCapability) => {
                    DispatchOutcome::RefusalUnsupported(resp.message)
                }
                Some(RefusalReason::MalformedRequest) | None => {
                    DispatchOutcome::RefusalMalformed(resp.message)
                }
                Some(RefusalReason::AlreadyRunning) => DispatchOutcome::AcceptedAlreadyRunning,
            };
        }
        // Accepted: check for ALREADY_RUNNING idempotent path.
        if matches!(
            map_refusal_reason(resp.refusal_reason),
            Some(RefusalReason::AlreadyRunning)
        ) {
            return DispatchOutcome::AcceptedAlreadyRunning;
        }
        DispatchOutcome::Accepted
    }

    async fn resolve_address(&self, node_id: &NodeId) -> Result<String, String> {
        use lattice_common::traits::NodeRegistry;
        match self.quorum.get_node(node_id).await {
            Ok(n) if !n.agent_address.is_empty() => Ok(n.agent_address),
            Ok(_) => Err("empty agent_address".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn mark_accepted(&self, alloc_id: &AllocId, node_id: &NodeId) {
        let mut sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get_mut(alloc_id) {
            s.per_node
                .insert(node_id.clone(), PerNodeDispatchState::Accepted);
        }
    }

    async fn mark_in_flight(&self, alloc_id: &AllocId, node_id: &NodeId) {
        let mut sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get_mut(alloc_id) {
            s.per_node
                .insert(node_id.clone(), PerNodeDispatchState::InFlight);
        }
    }

    async fn do_rollback(
        &self,
        alloc: &Allocation,
        released_nodes: &[NodeId],
        failed_node: &NodeId,
        reason: String,
    ) {
        // Collect agents to clean up via StopAllocation (DEC-DISP-08).
        let to_stop: Vec<NodeId> = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(&alloc.id)
                .map(|s| {
                    s.per_node
                        .iter()
                        .filter(|(_, state)| **state == PerNodeDispatchState::Accepted)
                        .map(|(n, _)| n.clone())
                        .collect()
                })
                .unwrap_or_default()
        };

        lattice_common::metrics::record_dispatch_rollback(&reason);
        match self
            .quorum
            .propose(QuorumCommand::RollbackDispatch {
                allocation_id: alloc.id,
                released_nodes: released_nodes.to_vec(),
                failed_node: Some(failed_node.clone()),
                observed_state_version: alloc.state_version,
                reason,
            })
            .await
        {
            Ok(QuorumResponse::Ok) => {
                info!(alloc = %alloc.id, "RollbackDispatch committed");
            }
            Ok(QuorumResponse::Error(e)) => {
                // State-version mismatch is benign: completion landed first.
                debug!(alloc = %alloc.id, error = %e, "RollbackDispatch rejected (likely late-completion race)");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(alloc = %alloc.id, error = %e, "RollbackDispatch propose failed");
            }
        }

        // DEC-DISP-08: fire-and-forget StopAllocation to each accepted node.
        for n in to_stop {
            let qc = self.quorum.clone();
            let alloc_id = alloc.id;
            let node = n.clone();
            tokio::spawn(async move {
                match send_stop_allocation(&qc, &node, alloc_id).await {
                    Ok(()) => {
                        lattice_common::metrics::record_rollback_stop_sent("success");
                    }
                    Err(e) => {
                        lattice_common::metrics::record_rollback_stop_sent("failure");
                        warn!(alloc = %alloc_id, node = %node, error = %e, "StopAllocation cleanup failed (non-fatal)");
                    }
                }
            });
        }

        // Clear session.
        let mut sessions = self.sessions.lock().await;
        sessions.remove(&alloc.id);
    }
}

async fn send_stop_allocation(
    quorum: &Arc<QuorumClient>,
    node_id: &NodeId,
    alloc_id: AllocId,
) -> Result<(), String> {
    use lattice_common::traits::NodeRegistry;
    let addr = match quorum.get_node(node_id).await {
        Ok(n) if !n.agent_address.is_empty() => n.agent_address,
        _ => return Err("no agent_address".into()),
    };
    let endpoint = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };
    let mut client = pb::node_agent_service_client::NodeAgentServiceClient::connect(endpoint)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let req = tonic::Request::new(pb::StopAllocationRequest {
        allocation_id: alloc_id.to_string(),
        grace_period_seconds: 30,
        reason: "dispatch_rolled_back".into(),
    });
    let _ = client
        .stop_allocation(req)
        .await
        .map_err(|e| format!("rpc: {e}"))?;
    Ok(())
}

fn map_refusal_reason(code: Option<i32>) -> Option<RefusalReason> {
    let c = code?;
    match pb::RefusalReason::try_from(c) {
        Ok(pb::RefusalReason::RefusalBusy) => Some(RefusalReason::Busy),
        Ok(pb::RefusalReason::RefusalUnsupportedCapability) => {
            Some(RefusalReason::UnsupportedCapability)
        }
        Ok(pb::RefusalReason::RefusalMalformedRequest) => Some(RefusalReason::MalformedRequest),
        Ok(pb::RefusalReason::RefusalAlreadyRunning) => Some(RefusalReason::AlreadyRunning),
        Ok(pb::RefusalReason::RefusalUnspecified) => None,
        Err(_) => {
            // D-ADV-ARCH-10: unknown enum code from a newer agent is
            // treated as MalformedRequest (safe-fail). Counter surfaces
            // rolling-upgrade skew between agent and lattice-api versions.
            lattice_common::metrics::record_unknown_refusal_reason(&format!("{c}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_refusal_reason_maps_to_none() {
        // D-ADV-ARCH-10: proto values from a newer agent that we don't
        // recognize MUST be treated as MalformedRequest (safe-fail). Our
        // map returns None; the caller then selects MalformedRequest.
        assert_eq!(map_refusal_reason(Some(99)), None);
        assert_eq!(map_refusal_reason(None), None);
    }

    #[test]
    fn dispatcher_config_defaults_are_sensible() {
        let c = DispatcherConfig::default();
        assert_eq!(c.attempt_backoff.len(), 3);
        assert!(c.per_agent_concurrency <= c.max_concurrent_attempts);
    }
}

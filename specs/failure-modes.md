# Failure Modes & Degradation Behavior

Reorganized from `docs/architecture/failure-modes.md` into the spec format. Each failure specifies detection, blast radius, degradation behavior, and what is unacceptable.

## Design Principle

Fail-safe defaults. Running allocations survive component failures. No silent failures (INV-N4).

## Component Failures

### FM-01: Quorum Member Loss

| Aspect | Detail |
|---|---|
| **Detection** | Raft heartbeat timeout (500ms) |
| **Blast radius** | None if minority. Raft tolerates 1 failure (3-member) or 2 failures (5-member). |
| **Degradation** | Transparent. Remaining majority continues reads and commits. |
| **Recovery** | Replace failed member via Raft membership change. |
| **Data loss** | None. Raft log is replicated. |
| **Unacceptable** | Majority loss (see FM-03). |

### FM-02: Quorum Leader Loss

| Aspect | Detail |
|---|---|
| **Detection** | Raft follower timeout triggers election |
| **Blast radius** | 1-3s scheduling pause (no commits during election) |
| **Degradation** | In-flight proposals not yet committed are re-proposed by schedulers on next cycle. Running allocations unaffected. |
| **Recovery** | Automatic leader election. |
| **Data loss** | None. Uncommitted proposals retried. |
| **Unacceptable** | Election taking >10s (would indicate network or configuration issue). |

### FM-03: Complete Quorum Loss

| Aspect | Detail |
|---|---|
| **Detection** | All quorum members unreachable. API returns unavailable. |
| **Blast radius** | No new allocations. No node ownership changes. No sensitive audit writes. |
| **Degradation** | Running allocations continue — node agents operate autonomously. Node agents buffer heartbeats for replay on recovery. |
| **Recovery** | Restore from latest Raft snapshot + WAL replay. |
| **Data loss** | None (last committed state restored). |
| **Unacceptable** | Snapshot corruption or missing WAL. Must have backup verification. |

### FM-04: Node Agent Crash

| Aspect | Detail |
|---|---|
| **Detection** | Heartbeat timeout (30s) → Degraded → grace period (60s standard, 5min sensitive) → Down |
| **Blast radius** | Allocations on that node. Other nodes unaffected. |
| **Degradation** | Node removed from scheduling during Degraded. Workloads survive in their own cgroup scopes (`KillMode=process` in systemd). |
| **Recovery** | Agent restart → load persisted state from `/var/lib/lattice/agent-state.json` → reattach to surviving processes (PID liveness check) → clean up orphaned cgroups → re-register with quorum → health check → Ready. |
| **Data loss** | Running allocation output since last checkpoint. Agent state file is persisted on graceful shutdown and periodically during operation. |
| **Unacceptable** | Silent Down transition without alert. Sensitive allocation auto-requeued (requires operator intervention). |

### FM-05: Node Hardware Failure

| Aspect | Detail |
|---|---|
| **Detection** | Dual-path: heartbeat timeout + OpenCHAMI Redfish BMC polling |
| **Blast radius** | Same as FM-04 but immediate Down (BMC detection bypasses grace period). |
| **Degradation** | Allocations requeued per policy. Alert raised. |
| **Recovery** | Hardware replacement or repair. OpenCHAMI reimage. |
| **Data loss** | Running allocation output since last checkpoint. |
| **Unacceptable** | BMC failure masking hardware failure (no detection path). |

### FM-06: vCluster Scheduler Crash

| Aspect | Detail |
|---|---|
| **Detection** | Liveness probe failure |
| **Blast radius** | Scheduling pauses for this vCluster only. Running allocations unaffected. Other vClusters unaffected. |
| **Degradation** | No new scheduling for this vCluster. Pending allocations wait. |
| **Recovery** | Stateless restart. Reads pending allocations and node state from quorum. |
| **Data loss** | None. Pending allocations persisted in quorum. |
| **Unacceptable** | Crash affecting other vClusters' scheduling. |

### FM-07: API Server Crash

| Aspect | Detail |
|---|---|
| **Detection** | Load balancer health check |
| **Blast radius** | User requests fail until restart or failover to another replica. |
| **Degradation** | Multiple API servers behind load balancer provide redundancy. Client retries with backoff. |
| **Recovery** | Stateless restart. |
| **Data loss** | Submissions acknowledged but not yet committed (see assumption A-A1, window <30s). |
| **Unacceptable** | Silent submission loss. Users must be able to detect via status query. |

### FM-08: Checkpoint Broker Crash

| Aspect | Detail |
|---|---|
| **Detection** | Health check failure |
| **Blast radius** | Pending checkpoint decisions delayed. No allocation data lost. |
| **Degradation** | Running allocations effectively non-preemptible during outage (no checkpoint hints sent). |
| **Recovery** | Restart, re-evaluate all running allocations against cost model. |
| **Data loss** | One evaluation cycle of checkpoint decisions delayed. |
| **Unacceptable** | Broker crash causing preemption without checkpoint (must gracefully degrade to no-preemption). |

## Infrastructure Failures

### FM-09: Network Partition (Node ↔ Quorum)

| Aspect | Detail |
|---|---|
| **Detection** | Heartbeat timeout (quorum side). Connection failure (node side). |
| **Blast radius** | Affected nodes enter Degraded → Down. Allocations on affected nodes requeued. |
| **Degradation** | Node agent continues running allocations autonomously. Buffers heartbeats. If partition heals within grace period: no disruption. |
| **Recovery** | Connectivity restore → heartbeat replay → Ready. |
| **Data loss** | None if partition heals in time. |
| **Unacceptable** | Partition healing not detected (node stays Down despite being reachable). |

### FM-10: Network Partition (Quorum Split-Brain)

| Aspect | Detail |
|---|---|
| **Detection** | Raft protocol inherently prevents split-brain |
| **Blast radius** | Minority partition stalls (cannot commit). Majority continues. |
| **Degradation** | Schedulers connected to minority partition cannot commit proposals. |
| **Recovery** | Partition heals → minority catches up via Raft log replication. |
| **Data loss** | None. No divergent state possible. |
| **Unacceptable** | Divergent state (impossible by Raft design). |

### FM-11: Storage Unavailability (VAST Down)

| Aspect | Detail |
|---|---|
| **Detection** | Failed VAST API calls / NFS mount timeouts |
| **Blast radius** | Data staging pauses. Checkpoint writes fail. New allocations needing staging held. |
| **Degradation** | Running allocations with data already mounted continue. Checkpoint broker suppresses checkpoint decisions (write bandwidth = 0). |
| **Recovery** | Automatic retry with backoff. Staging resumes on recovery. |
| **Data loss** | None. Running allocations unaffected. |
| **Unacceptable** | VAST failure causing allocation crashes (data must be accessible once mounted). |

### FM-12: OpenCHAMI Unavailable

| Aspect | Detail |
|---|---|
| **Detection** | Failed API calls |
| **Blast radius** | Node boot/reimage blocked. Sensitive wipe blocked (nodes quarantined). |
| **Degradation** | Scheduling to already-booted nodes continues. Wipe-pending nodes stay in quarantine (safe). |
| **Recovery** | Operations queued and retried on recovery. |
| **Data loss** | None. |
| **Unacceptable** | Quarantined sensitive nodes auto-released without wipe. |

### FM-13: Waldur Unavailable

| Aspect | Detail |
|---|---|
| **Detection** | Failed API calls |
| **Blast radius** | Accounting events buffered. Quota updates from Waldur delayed. |
| **Degradation** | Scheduling continues unaffected (INV-N5). Events buffered in memory (10K) then disk (100K). Budget enforcement falls back to internal ledger (IP-10a): GPU-hours computed from allocation history in quorum state. Budget penalty in cost function remains active. |
| **Recovery** | Buffered events replayed in order on reconnection. When Waldur recovers, its `remaining_budget` takes precedence over internal ledger. |
| **Data loss** | If both buffers full: events dropped (counter metric). Reconstructable from quorum logs via reconciliation. |
| **Unacceptable** | Scheduling blocked by accounting failure. Budget enforcement disabled silently (must fall back to internal ledger, not to "unlimited"). |

### FM-14: TSDB Unavailable (VictoriaMetrics Down)

| Aspect | Detail |
|---|---|
| **Detection** | Failed metric queries |
| **Blast radius** | Cost function inputs stale (f5 data readiness, f7 energy). Autoscaling paused. Dashboards unavailable. |
| **Degradation** | Scheduling continues with stale scores. Autoscaling: no action (allocation stays at current size). |
| **Recovery** | Normal evaluation resumes on reconnection. |
| **Data loss** | Metrics during outage lost (ring buffer covers live access). |
| **Unacceptable** | Scheduling crash due to missing metrics. Autoscaling without metrics (blind scaling). |

## hpc-core Integration Failures

### FM-19: PACT Handoff Socket Unavailable

| Aspect | Detail |
|---|---|
| **Detection** | Socket connection timeout (1s) on `/run/pact/handoff.sock` |
| **Blast radius** | None — transparent fallback to self-service mode. |
| **Degradation** | Lattice-node-agent creates namespaces via `unshare(2)` directly. No shared mount cache (each allocation mounts independently). Audit event emitted: `namespace.handoff_failed`. |
| **Recovery** | If PACT restarts: next allocation uses handoff. Already-running allocations unaffected (namespaces are per-process). |
| **Data loss** | None. |
| **Unacceptable** | Allocation startup blocked waiting for PACT. Must fail fast (1s timeout). |

### FM-20: Identity Cascade Exhaustion

| Aspect | Detail |
|---|---|
| **Detection** | All `IdentityProvider` implementations return errors |
| **Blast radius** | Node agent or quorum member cannot establish mTLS connections. Cannot register, heartbeat, or serve RPCs. |
| **Degradation** | SPIRE unavailable (socket missing or agent down) → SelfSignedProvider fails (quorum unreachable) → StaticProvider fails (bootstrap cert expired or files missing) → **no valid identity**. Agent enters degraded state. Existing connections continue until cert expires. |
| **Recovery** | Fix underlying provider: restart SPIRE agent (primary path on HPE Cray), restore quorum for self-signed CA, or provision new bootstrap certs. Agent automatically retries cascade on next rotation cycle. |
| **Data loss** | None. Running allocations continue (existing TLS sessions remain valid). |
| **Unacceptable** | Silent operation without mTLS. Must refuse to serve RPCs without valid identity. |

### FM-21: Audit Sink Buffer Overflow

| Aspect | Detail |
|---|---|
| **Detection** | Internal buffer full, `emit()` can no longer accept events |
| **Blast radius** | Non-sensitive audit events dropped (with counter metric). Sensitive audit events are never dropped — they block on Raft commit (INV-S3). |
| **Degradation** | Audit gap for non-sensitive operations. Counter metric `lattice_audit_events_dropped_total` tracks loss. |
| **Recovery** | Quorum reconnection → buffer drains → counter stops increasing. |
| **Data loss** | Dropped non-sensitive audit events. Reconstructable from Raft log (committed state changes are authoritative). |
| **Unacceptable** | Sensitive audit events dropped. Blocking non-sensitive operations waiting for audit. |

### FM-22: Readiness Gate Timeout

| Aspect | Detail |
|---|---|
| **Detection** | `/run/pact/ready` not present after `readiness_timeout` (default 30s) |
| **Blast radius** | None — agent proceeds as ready. |
| **Degradation** | Agent starts accepting allocations before PACT confirms all infrastructure services are up. Risk: chronyd not yet synced (time skew), GPU daemon not started (GPU unavailable). |
| **Recovery** | PACT eventually starts infrastructure services. GPU/time issues resolve. Running allocations may see brief degradation. |
| **Data loss** | None. |
| **Unacceptable** | Agent blocked indefinitely (INV-RI4). |

### FM-23: Certificate Rotation Failure

| Aspect | Detail |
|---|---|
| **Detection** | `CertRotator::rotate()` returns `RotationFailed` |
| **Blast radius** | Active TLS channel unchanged (safe). Next rotation retry at scheduled interval. |
| **Degradation** | Current cert continues serving. If cert expires before next successful rotation: connections fail (FM-20 cascade). |
| **Recovery** | Fix signing endpoint or SPIRE. Rotation succeeds on next attempt. |
| **Data loss** | None. |
| **Unacceptable** | Rotation dropping in-flight connections (INV-ID3). |

### FM-24: Cgroup Scope Cleanup Failure

| Aspect | Detail |
|---|---|
| **Detection** | `destroy_scope()` returns `KillFailed` — processes stuck in D-state |
| **Blast radius** | Single allocation. Cgroup scope orphaned with zombie processes. |
| **Degradation** | Node remains in Ready state but has reduced effective capacity (orphaned scope consumes resources). Alert raised. |
| **Recovery** | Operator intervention: reboot node or resolve stuck processes. OpenCHAMI reimage as last resort. |
| **Data loss** | None (allocation already completed/failed). |
| **Unacceptable** | Silently ignoring orphaned scope. Must be tracked and alerted. |

## Secret Resolution Failures

### FM-25: Vault Unavailable at Startup

| Aspect | Detail |
|---|---|
| **Detection** | `SecretResolver` Vault connection or auth failure during startup |
| **Blast radius** | Component does not start. No running allocations affected (startup-only). |
| **Degradation** | None — fatal. Component logs error with Vault address and failure reason, then exits. |
| **Recovery** | Fix Vault (restore service, fix network, correct AppRole credentials) and restart component. Or remove `vault.address` from config to fall back to env/config mode. |
| **Data loss** | None. Component never started. |
| **Unacceptable** | Starting with partial secrets. Retrying indefinitely (blocks operator from diagnosing). Falling back to config literals when Vault is explicitly configured. |

### FM-26: Vault Key Missing at Startup

| Aspect | Detail |
|---|---|
| **Detection** | `SecretResolver` receives 404 from Vault KV v2 for a required secret path |
| **Blast radius** | Component does not start. |
| **Degradation** | None — fatal. Error message includes the missing Vault path and key name. |
| **Recovery** | Operator creates the missing secret in Vault at the expected path, then restarts. |
| **Data loss** | None. |
| **Unacceptable** | Silently using empty/default values for missing secrets. |

## Allocation-Level Failures

### FM-15: Prologue Failure

| Aspect | Detail |
|---|---|
| **Detection** | Node agent reports prologue error |
| **Blast radius** | Single allocation. Other allocations on the node unaffected. |
| **Degradation** | Allocation retried on different nodes (max 3 retries). |
| **Recovery** | Retry succeeds, or allocation fails after max retries with descriptive error. |
| **Data loss** | None. |
| **Unacceptable** | Silent prologue failure (allocation runs without its environment). Violates INV-O2. |

### FM-16: Application Crash

| Aspect | Detail |
|---|---|
| **Detection** | Node agent detects process exit with non-zero status |
| **Blast radius** | Single allocation. Nodes released. |
| **Degradation** | Requeue per policy (never / on_node_failure / always). DAG edges evaluated. |
| **Recovery** | Application-dependent. |
| **Data loss** | Application-dependent. |
| **Unacceptable** | Infinite requeue loop (bounded by max_requeue, default 3). |

### FM-17: Walltime Exceeded

| Aspect | Detail |
|---|---|
| **Detection** | Node agent timer |
| **Blast radius** | Single allocation. SIGTERM → 30s grace → SIGKILL. |
| **Degradation** | Allocation marked Failed with reason `walltime_exceeded`. Nodes released. |
| **Recovery** | User resubmits with longer walltime or checkpoint-enabled. |
| **Data loss** | Unsaved work since last checkpoint. |
| **Unacceptable** | Walltime extended to accommodate in-progress checkpoint (INV-E4). |

### FM-18: Checkpoint Timeout During Preemption

| Aspect | Detail |
|---|---|
| **Detection** | Checkpoint broker timeout (default 10 min) |
| **Blast radius** | Single allocation. |
| **Degradation** | Slow application: one 50% timeout extension. Unresponsive: SIGTERM → SIGKILL. Failed, not Suspended. |
| **Recovery** | Allocation requeued if policy allows, but without checkpoint (restarts from scratch). |
| **Data loss** | All progress since last successful checkpoint. |
| **Unacceptable** | Preemption blocked indefinitely by unresponsive application. |

## Dispatch Failures (FM-D)

Introduced 2026-04-16. Each row exercises a specific path in the Dispatch cross-context contracts (IP-02, IP-03, IP-13, IP-14, IP-15) and is covered by a Gherkin scenario in `features/allocation_dispatch.feature`.

### FM-D1: Agent Unreachable at Dispatch Time

| Aspect | Detail |
|---|---|
| **Detection** | Dispatcher's `RunAllocation` RPC returns connection-refused, DNS fail, or times out within the attempt deadline (1s default). |
| **Blast radius** | One (allocation, node) pair per attempt. Other allocations on the same node proceed independently; other nodes unaffected. |
| **Degradation** | Bounded retry on same node (3 × backoff 1/2/5s). On budget exhaustion: IP-14 `RollbackDispatch` → allocation returns to Pending, scheduler re-places. Both `allocation.dispatch_retry_count` and `node.consecutive_dispatch_failures` incremented (INV-D11). |
| **Recovery** | Automatic. Either (a) agent becomes reachable and next attempt succeeds, or (b) allocation is placed on a different node, or (c) node-level counter escalates the node to Degraded (INV-D11). |
| **Data loss** | None. Workload has not started. |
| **Unacceptable** | Infinite retry on same unreachable node. Allocation stuck without progress or terminal state. Silent absorption of allocations into a broken node. |

### FM-D2: Agent Accepts Dispatch Then Crashes Before Spawn

| Aspect | Detail |
|---|---|
| **Detection** | Two independent signals: (a) INV-D8 silent-node sweep finds no heartbeat or Completion Report for the allocation within `heartbeat_interval + grace_period`; (b) heartbeat-based node health also transitions the node to Degraded/Down. |
| **Blast radius** | This allocation (and any other allocations that had been running on the same node, already covered by FM-04). |
| **Degradation** | Scheduler's silent-node reconciliation (IP-15) transitions the allocation to `Failed` with reason `node_silent`, or `Pending` with requeue_count++ if RequeuePolicy permits (OnNodeFailure, Always). |
| **Recovery** | If the agent restarts and reattach finds the allocation is dead, a late Completion Report phase `Failed` may arrive — INV-D7 rejects it since state is already terminal. No-op. |
| **Data loss** | No workload output (spawn never happened). |
| **Unacceptable** | Allocation stuck Running with no process and no silent-sweep trigger (i.e., scheduler reconciliation failing to run). |

### FM-D3: Agent Spawns Workload Then Crashes With PID Alive

| Aspect | Detail |
|---|---|
| **Detection** | Agent restart; reattach path finds the PID in `AgentState` and `is_process_alive(pid) == true`. |
| **Blast radius** | None if reattach succeeds. Workload continues in its own cgroup scope. |
| **Degradation** | None under `KillMode=process`: the Workload Process survives the agent crash because it is parented to its own cgroup scope (systemd scope, not the agent service). |
| **Recovery** | Reattach registers the allocation in `AllocationManager` with phase `Running`, heartbeats resume carrying the allocation. First heartbeat after reattach also emits a Completion Report phase `Running` (idempotent by INV-D4) to confirm for the quorum. |
| **Data loss** | None. |
| **Unacceptable** | Double-spawn (agent forgets reattach, starts a second process). INV-D3 prevents but must be tested. |

### FM-D4: Agent Spawns Workload Then Crashes With PID Dead

| Aspect | Detail |
|---|---|
| **Detection** | Agent restart; reattach path finds the PID in `AgentState` but `is_process_alive(pid) == false`. |
| **Blast radius** | This allocation only. |
| **Degradation** | Agent enqueues a Completion Report phase `Failed` with reason `pid_vanished_across_restart`. Next heartbeat delivers it; quorum applies state transition. |
| **Recovery** | Standard Failed path. If RequeuePolicy permits, reconciliation requeues. |
| **Data loss** | Workload output since last checkpoint (same as FM-04). |
| **Unacceptable** | Process silently stays as Running in GlobalState. |

### FM-D5: Orphan Workload Process On Agent Boot

| Aspect | Detail |
|---|---|
| **Detection** | Agent boot sequence enumerates `workload.slice/` and finds a cgroup scope whose allocation ID is neither in `AgentState` nor in the current reattach set. |
| **Blast radius** | One orphan at a time. |
| **Degradation** | INV-D9: terminate the scope, remove it, emit a `lost_workload` audit event with the orphaned allocation ID. Complete within one heartbeat interval. |
| **Recovery** | Automatic. |
| **Data loss** | Workload output since the orphan was spawned (no record in state file, so no checkpoint relationship). Unavoidable. |
| **Unacceptable** | Orphans accumulating over multiple restarts; cgroup-scope leaks reducing node capacity. |

### FM-D6: Late Completion Report Races Dispatch Rollback

| Aspect | Detail |
|---|---|
| **Detection** | Quorum receives a RollbackDispatch proposal whose `observed_state_version` does not match current allocation version. |
| **Blast radius** | One allocation. |
| **Degradation** | Quorum rejects RollbackDispatch with `StateVersionMismatch`. The race is benign: completion landed first. Counter `lattice_dispatch_rollback_raced_by_completion_total` increments for observability. |
| **Recovery** | None needed. Allocation is Completed (or whatever terminal state the Completion Report set). Retry budget burned on a successful allocation — accepted cost. |
| **Data loss** | None. |
| **Unacceptable** | Rollback winning over completion would corrupt accounting and leak a terminated process. |

### FM-D11: Orphaned Workload After Accepted-But-Rollback Race

| Aspect | Detail |
|---|---|
| **Detection** | Race window: Dispatcher's final attempt times out, `RollbackDispatch` commits, but the target agent had actually accepted the RPC and began prologue. On success, the agent starts a Workload Process for an allocation that no longer "owns" the node. |
| **Blast radius** | One Workload Process per raced allocation per agent. |
| **Degradation** | Dispatcher's DEC-DISP-08 cleanup issues fire-and-forget `StopAllocation` on every released node whose per-node state was `Accepted` or `InFlight`. If the StopAllocation lands before the Workload Process is fully spawned, the process is terminated via agent epilogue. If it lands after, `StopAllocation` still reaches the AllocationManager and the still-running process is SIGTERM'd. |
| **Recovery** | If `StopAllocation` is lost in transit (network blip, agent restart mid-cleanup), the orphan is caught by INV-D9 orphan-cleanup at the next agent boot. Bounded by `reattach_grace_period` + agent restart cadence in the worst case. |
| **Data loss** | None — no user data. Some CPU-seconds wasted on the aborted prologue + process startup. |
| **Unacceptable** | Orphan running past one reattach cycle without detection. |

### FM-D7: Stale Heartbeat Replay (Phase Regression)

| Aspect | Detail |
|---|---|
| **Detection** | Quorum compares Completion Report phase to allocation's current state. A report whose phase is earlier than current state (e.g., `Staging` when state is already `Completed`) is a regression. |
| **Blast radius** | One allocation. |
| **Degradation** | INV-D7: reject the report, log anomaly `(node_id, allocation_id, current_state, reported_phase)`, ACK the heartbeat so the agent does not retransmit. |
| **Recovery** | None needed. Anomaly logged for operator investigation (suggests agent buffer corruption, reattach mis-ordering, or attacker replay). |
| **Data loss** | None. |
| **Unacceptable** | Resurrection of a Completed allocation; accounting double-count; ownership of a node already released. |

### FM-D8: Completion Report Buffer Pressure on Agent

| Aspect | Detail |
|---|---|
| **Detection** | Rare — buffer is keyed by allocation_id (INV-D13), so it cannot grow past the number of distinct active allocations on the node. Pressure arises only if a node has more than `max_buffer_allocations` (default 256) concurrently active allocations, which itself is a cardinality anomaly for typical HPC nodes. |
| **Blast radius** | The agent's extra allocations beyond the buffer bound. |
| **Degradation** | INV-D13 keeps one entry per allocation with latest-phase-wins semantics; a later report for the same allocation overwrites an earlier one, and because Local Allocation Phases are monotonic the replacement is always forward (terminal reports therefore never get overwritten by non-terminal ones). The buffer bound is a safety net for the cardinality-anomaly case: when exceeded, the agent rejects new report appends for allocations not yet in the buffer and raises a `dispatch_report_buffer_exhausted` alarm with `(node_id, active_allocation_count)`. |
| **Recovery** | Automatic per INV-D13's correctness-by-construction argument. When some active allocation reaches terminal state and its report is flushed, a slot opens for a new allocation's first report. Chronic pressure indicates a node mis-provisioned for its workload density — operator concern, not runtime concern. |
| **Data loss** | None under normal cardinality. Under cardinality anomaly: the very-newest allocations may have their first Staging report dropped until a slot frees; their subsequent reports (once a slot frees) still reach the quorum. Intermediate Running reports are structurally absent but not missed (terminal reports preserve correctness). |
| **Unacceptable** | Final-state reports dropped for ANY allocation. INV-D13 prevents this by construction. |

### FM-D9: Node With Working Heartbeat But Broken RunAllocation Handler

| Aspect | Detail |
|---|---|
| **Detection** | INV-D11: `node.consecutive_dispatch_failures` reaches `max_node_dispatch_failures` (default 5) while the node's heartbeat continues to land normally. |
| **Blast radius** | The affected node absorbs failed dispatches from multiple allocations until the counter caps. Up to `max_node_dispatch_failures × max_attempts_per_dispatch` wasted RPC roundtrips. |
| **Degradation** | Raft proposal transitions the node to `Degraded` and removes it from `available_nodes()`. Scheduler stops placing new allocations on this node. Existing allocations on this node (if any managed to start before the handler broke) continue running. |
| **Recovery** | When the underlying issue is fixed and a dispatch on this node succeeds, the quorum's handler for the first `Staging` Completion Report resets `consecutive_dispatch_failures` to zero and transitions the node back to `Ready`. |
| **Data loss** | None directly. Allocations whose dispatches failed are requeued or Failed per IP-14. |
| **Unacceptable** | Silent absorption of infinite allocations into a broken node (exactly what would happen without INV-D11). Permanent Degraded state with no reset path. |

### FM-D10: Dispatcher Process Crash Mid-Attempt

| Aspect | Detail |
|---|---|
| **Detection** | The `lattice-api` process (host of the Dispatcher) crashes or restarts while Dispatch Attempts are in flight. |
| **Blast radius** | All in-flight dispatch attempts for that process instance. Scheduler cycles continue on surviving replicas. |
| **Degradation** | The next running `lattice-api` process discovers the Dispatcher state by reading Raft: allocations that are `Running` with assigned_nodes and no phase-advancing Completion Report are picked up fresh. No dispatcher-local state is lost because dispatcher state is derived from GlobalState (it has no private store). |
| **Recovery** | Automatic on process restart. The new Dispatcher resumes with a fresh attempt budget for each un-acked allocation. From the agent's perspective, INV-D3 still deduplicates if the earlier attempt actually reached the agent. |
| **Data loss** | None. |
| **Unacceptable** | Dispatcher maintains hidden state (timer, counter) outside Raft; a crash then causes permanent loss of in-flight context. |

## Recovery Matrix

| Failure | Detection Time | Recovery Time | Data Loss | Scheduling Impact |
|---|---|---|---|---|
| Quorum member | 500ms | Seconds (election) | None | None |
| Quorum leader | Seconds | 1-3s | None | Brief pause |
| Complete quorum | Immediate | Minutes (snapshot restore) | None | Full stop until recovered |
| Node agent crash | 30s + 60s grace | Minutes (restart) | Since last checkpoint | Node unavailable |
| Hardware failure | Seconds (BMC) | Hours (replacement) | Since last checkpoint | Node unavailable |
| vCluster scheduler | Seconds (probe) | Seconds (restart) | None | vCluster paused |
| API server | Seconds (probe) | Seconds (restart) | <30s submissions | API unavailable |
| Checkpoint broker | Seconds (probe) | Seconds (restart) | Delayed decisions | No preemption |
| Network partition | 30s | Variable | None if heals in grace | Affected nodes unavailable |
| VAST down | Seconds (API) | Variable | None | Staging paused |
| OpenCHAMI down | Seconds (API) | Variable | None | Provisioning paused |
| Waldur down | Seconds (API) | Variable | Billing data if buffer full | None |
| TSDB down | Seconds (query) | Variable | Metric gap | Stale scores, no autoscaling |
| PACT handoff down | 1s (timeout) | Seconds (fallback) | None | None (self-service) |
| Identity cascade exhausted | Immediate | Variable | None | New connections fail |
| Audit sink overflow | Immediate | On reconnect | Non-sensitive events | None |
| Readiness gate timeout | 30s | Immediate (proceed) | None | Possible early-start issues |
| Cert rotation failure | Immediate | Next retry | None | None (current cert serves) |
| Cgroup cleanup failure | Immediate | Manual | None | Reduced node capacity |
| Vault unavailable (startup) | Immediate | Fix Vault + restart | None | Component won't start |
| Vault key missing (startup) | Immediate | Add key + restart | None | Component won't start |
| Agent unreachable at dispatch (FM-D1) | <1s per attempt | Retry + rollback to Pending | None (pre-spawn) | One allocation, possibly escalates node to Degraded |
| Agent crashes pre-spawn (FM-D2) | heartbeat_interval + grace_period | Silent-node sweep → Failed/Requeued | None | One allocation |
| Agent crashes post-spawn, PID alive (FM-D3) | Agent restart | Reattach | None | None (workload continues) |
| Agent crashes post-spawn, PID dead (FM-D4) | Agent restart | Completion Report phase Failed | Since last checkpoint | One allocation |
| Orphan cgroup scope (FM-D5) | Agent boot | Terminate + audit | Orphan's output (unrecoverable) | One orphan |
| Rollback races completion (FM-D6) | Raft rejection | Benign, completion wins | None | None |
| Stale heartbeat replay (FM-D7) | Phase comparison | Reject + log | None | None |
| Completion buffer overflow (FM-D8) | Buffer full | Drop oldest, alarm | Intermediate telemetry | Observability degraded |
| Node with broken RunAllocation (FM-D9) | 5 consecutive dispatch failures | Degrade node | None directly | Node excluded until fixed |
| Dispatcher process crash (FM-D10) | Process restart | Re-derive state from Raft | None | Brief delay in retry |

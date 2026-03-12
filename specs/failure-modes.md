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
| **Degradation** | Node removed from scheduling during Degraded. Allocations continue if agent restarts within grace period. |
| **Recovery** | Agent restart → re-register → health check → Ready. |
| **Data loss** | Running allocation output since last checkpoint. |
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
| **Degradation** | Scheduling continues unaffected (INV-N5). Events buffered in memory (10K) then disk (100K). |
| **Recovery** | Buffered events replayed in order on reconnection. |
| **Data loss** | If both buffers full: events dropped (counter metric). Reconstructable from quorum logs via reconciliation. |
| **Unacceptable** | Scheduling blocked by accounting failure. |

### FM-14: TSDB Unavailable (VictoriaMetrics Down)

| Aspect | Detail |
|---|---|
| **Detection** | Failed metric queries |
| **Blast radius** | Cost function inputs stale (f5 data readiness, f7 energy). Autoscaling paused. Dashboards unavailable. |
| **Degradation** | Scheduling continues with stale scores. Autoscaling: no action (allocation stays at current size). |
| **Recovery** | Normal evaluation resumes on reconnection. |
| **Data loss** | Metrics during outage lost (ring buffer covers live access). |
| **Unacceptable** | Scheduling crash due to missing metrics. Autoscaling without metrics (blind scaling). |

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

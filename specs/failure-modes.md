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

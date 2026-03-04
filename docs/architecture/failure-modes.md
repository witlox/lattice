# Failure Modes and Recovery

## Design Principle

Fail-safe defaults. Running allocations survive component failures. Modeled after Slurm's proven failure patterns, mapped to Lattice's distributed architecture: requeue on node failure, state recovery on controller restart, running jobs unaffected by control plane restarts.

## Component Failures

### Quorum Member Loss

**Detection:** Raft heartbeat timeout (default: 500ms).

**Recovery:** Raft tolerates minority failure. A 3-member quorum tolerates 1 failure; a 5-member quorum tolerates 2. The remaining majority continues serving reads and commits. No scheduling disruption.

**Action:** Alert ops. Replace failed member via Raft membership change (add new → remove old). No data loss — Raft log is replicated.

### Quorum Leader Loss

**Detection:** Raft follower timeout triggers leader election.

**Recovery:** New leader elected within seconds (typically 1-3s depending on election timeout configuration). In-flight proposals that were not committed are retried by the proposing vCluster scheduler on the next scheduling cycle.

**Data loss risk:** None. Uncommitted proposals are re-proposed. Committed state is durable.

### Complete Quorum Loss

**Detection:** All quorum members unreachable. API server returns unavailable.

**Recovery:** Restore from most recent Raft snapshot + WAL replay (analogous to `slurmctld --recover`). The latest snapshot is stored on persistent storage (local SSD + replicated to S3). Recovery restores node ownership and sensitive audit state to the last committed entry.

**Impact during outage:** No new allocations can be scheduled (proposals cannot be committed). Running allocations continue — node agents operate autonomously. Node agents buffer heartbeats and replay on quorum recovery.

### Node Agent Crash

**Detection:** Heartbeat timeout (default: 30s) followed by grace period (default: 60s). Total time to `Down` transition: ~90s. Analogous to Slurm's `SlurmdTimeout`.

**Recovery:**
1. Quorum marks node as `Degraded` after first missed heartbeat
2. After grace period (default: 60s), node transitions to `Down`
3. Allocations on the node are requeued (if `requeue` policy allows) or marked `Failed`
4. Node agent restarts → re-registers with quorum → health check → re-enters scheduling pool

**Sensitive nodes:** Longer grace period (default: 5 minutes) to avoid false positives from transient issues. Sensitive allocations are never automatically requeued — operator intervention required.

### Node Hardware Failure

**Detection:** Dual-path: heartbeat timeout (node agent) + OpenCHAMI Redfish BMC polling (out-of-band).

**Recovery:** Same as agent crash, but OpenCHAMI can detect hardware failures (PSU, memory ECC uncorrectable, GPU fallen off bus) before heartbeat timeout. BMC-detected failures trigger immediate `Down` transition, skipping the grace period.

### vCluster Scheduler Crash

**Detection:** Health check failure (liveness probe).

**Recovery:** vCluster schedulers are stateless — they read pending allocations and node state from the quorum on each scheduling cycle. Restart from quorum state. No scheduling occurs for this vCluster during downtime, but running allocations continue unaffected (like slurmctld crash: running jobs are fine).

**Data loss risk:** None. Pending allocations are persisted in the quorum.

### API Server Crash

**Detection:** Load balancer health check / liveness probe.

**Recovery:** API servers are stateless. Restart and resume serving. Multiple API server replicas behind a load balancer provide redundancy. Client retries with exponential backoff. No job loss.

### Checkpoint Broker Crash

**Detection:** Health check failure.

**Recovery:** Pending checkpoint requests are lost (they were in-memory). On restart, the broker re-evaluates all running allocations against the checkpoint cost model. Allocations that should have been checkpointed will be identified on the next evaluation cycle.

**Data loss risk:** Minimal. At worst, one evaluation cycle's worth of checkpoint decisions are delayed. No allocation data is lost.

## Infrastructure Failures

### Network Partition: Node ↔ Quorum

**Detection:** Heartbeat timeout on the quorum side; connection failure on the node side.

**Recovery:**
- Quorum side: nodes marked unreachable → `Degraded` → `Down` after grace period. Allocations requeued.
- Node side: node agent continues running allocations autonomously. Buffers heartbeats and state updates. When connectivity restores, replays buffered state to quorum.
- If partition heals before grace period: node returns to `Ready`, no allocation disruption.

**Sensitive:** Extended grace period (5 minutes). Network partitions are logged as audit events.

### Network Partition: Quorum Split-Brain

**Detection:** Raft protocol prevents split-brain by design.

**Recovery:** The minority partition cannot achieve quorum and therefore cannot commit any proposals. The majority partition continues operating normally. When the partition heals, the minority members catch up via Raft log replication. No divergent state is possible.

### Storage Unavailability (VAST Down)

**Detection:** Failed VAST API calls / NFS mount timeouts.

**Impact:**
- Data staging for new allocations pauses (cannot pre-stage input data)
- Running allocations with data already mounted continue (local NVMe cache, if present, persists)
- Checkpoint writes fail → broker pauses checkpoint scheduling
- New allocation proposals that require data staging are held in queue

**Recovery:** Automatic retry with backoff. Alert raised. Staging resumes when VAST recovers. On nodes with NVMe cache, locally cached data persists through storage outage.

### OpenCHAMI Unavailable

**Detection:** Failed API calls to OpenCHAMI endpoints.

**Impact:**
- Node boot/reimaging blocked (cannot provision new nodes)
- Node wipe-on-release blocked (sensitive nodes held in quarantine state)
- Running allocations unaffected
- Scheduling of new allocations to already-booted nodes continues normally

**Recovery:** Operations that require OpenCHAMI are queued and retried. Alert raised.

## Allocation-Level Failures

### Prologue Failure (uenv Pull/Mount)

**Detection:** Node agent reports prologue error to quorum.

**Recovery:**
1. Node drained for this allocation (other allocations on the node unaffected)
2. Allocation retried on different nodes (analogous to Slurm PrologSlurmctld failure)
3. Max retries configurable (default: 3)
4. After max retries: allocation moves to `Failed` state, user notified

**Common causes:** Corrupted uenv image (hash mismatch), local cache full (if NVMe present), registry unavailable.

### Application Crash

**Detection:** Node agent detects process exit with non-zero status.

**Recovery:**
- Allocation moves to `Failed` state
- Nodes released back to scheduling pool
- If allocation has `requeue: on_node_failure` or `requeue: always`: re-enter queue
- DAG dependencies evaluated (cross-ref: [dag-scheduling.md](dag-scheduling.md))

### Walltime Exceeded

**Detection:** Node agent timer.

**Recovery:**
1. `SIGTERM` sent to all processes in the allocation
2. Grace period (default: 30s) for clean shutdown
3. `SIGKILL` if processes still running after grace period
4. Nodes released
5. Allocation marked as `Failed` with reason `walltime_exceeded`

### Walltime Exceeded During Checkpoint

If an allocation's walltime expires while a checkpoint is in progress:

1. **Walltime takes priority.** The walltime timer is not extended to accommodate an in-progress checkpoint.
2. `SIGTERM` is sent as normal. If the checkpoint completes within the SIGTERM grace period (default: 30s), the checkpoint is usable and the allocation is marked `Suspended` (can be resumed).
3. If the checkpoint does not complete within the grace period, `SIGKILL` is sent. The incomplete checkpoint is discarded and the allocation is marked `Failed` with reason `walltime_exceeded`.
4. The checkpoint broker tracks this race condition via the `lattice_checkpoint_walltime_conflict_total` counter metric.

## Recovery Matrix

| Failure | Detection | Recovery Action | Data Loss Risk |
|---------|-----------|----------------|----------------|
| Quorum member loss | Raft heartbeat | Leader election, continue | None |
| Quorum leader loss | Raft timeout | New election (1-3s) | None (uncommitted retried) |
| Complete quorum loss | All members down | Snapshot + WAL recovery | None (last committed state) |
| Node agent crash | Heartbeat timeout (30s) + grace (60s) | Degrade → Down → requeue | Running allocation output since last checkpoint |
| Node hardware failure | BMC + heartbeat | Immediate Down → requeue | Running allocation output since last checkpoint |
| vCluster scheduler crash | Health check | Stateless restart | None |
| API server crash | Health check | Stateless restart | None |
| Checkpoint broker crash | Health check | Restart, re-evaluate | Delayed checkpoint decisions |
| Network partition (node) | Heartbeat timeout | Grace period → requeue | None if heals in time |
| Network partition (quorum) | Raft protocol | Minority stalls, majority continues | None |
| VAST down | API timeout | Queue staging, continue running | None |
| OpenCHAMI down | API timeout | Queue provisioning ops | None |
| Prologue failure | Agent report | Retry on different nodes | None |
| Application crash | Process exit | Release nodes, optional requeue | Application-dependent |
| Walltime exceeded | Agent timer | SIGTERM → SIGKILL → release | Unsaved work |

## Allocation Requeue Policy

Configurable per allocation at submission time:

| Policy | Behavior |
|--------|----------|
| `never` | Allocation fails permanently on any node failure. Default for interactive sessions. |
| `on_node_failure` | Requeue only when the failure is node-side (hardware, agent crash, network partition). Default for batch allocations. |
| `always` | Requeue on any failure including application crash. Use with caution — can cause infinite loops for buggy applications. |

**Max requeue count:** Default 3. Configurable per allocation. After max requeues, allocation transitions to `Failed` regardless of policy.

**Requeue behavior:** Requeued allocations retain their original submission time for fair-share and wait-time calculations (no queue-jumping penalty, no starvation).

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — f₈ checkpoint_efficiency affects preemption cost
- [dag-scheduling.md](dag-scheduling.md) — Failure propagation in DAG workflows
- [sensitive-workloads.md](sensitive-workloads.md) — Sensitive-specific failure handling (longer grace periods, no auto-requeue)
- [accounting.md](accounting.md) — Accounting service failure buffering
- [upgrades.md](upgrades.md) — Failure detection during canary rollouts
- [sessions.md](sessions.md) — Interactive session disconnect/reconnect during node failures

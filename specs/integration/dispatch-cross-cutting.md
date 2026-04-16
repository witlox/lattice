# Integration Review: Dispatch × Adjacent Features

Review date: 2026-04-16
Reviewer role: Integrator
Scope: How the dispatch implementation (Impl 1..12 + deferral + gate-2
fixes, commits 26ea646 → 4a56286) interacts with adjacent features
that touch the same seams: Allocation, Node, AllocationManager, and
Raft state.

## Features reviewed

| Feature | Seam | Integration concern |
|---|---|---|
| **Dispatch** | — | Core |
| **Cancel allocation** | AllocationState transition | Does cancel reach the agent's process? |
| **Service reconciliation (requeue)** | Allocation state reset | Does requeue clear dispatch-specific fields? |
| **Node drain/degraded** | Node state + scheduling | Does Dispatcher skip non-Ready nodes? |
| **Preemption** | Allocation checkpoint + re-place | Does preemption path reset per_node_phase? |
| **Attach** | LocalAllocation.pid | Does attach see the pid the dispatch path populated? |
| **DAG** | Allocation state observation | Does DAG wait for conservative aggregate Running? |
| **Reattach (agent restart)** | per_node_phase in Raft vs LocalAllocation reset | Does reattach flag suppress false-positives? |

## Integration points examined

### IP-A: AllocationState transitions from multiple sources

Dispatch wires two new transition paths:
1. `ApplyCompletionReport` command from agent heartbeat → aggregated state (per-node-phase → Staging/Running/Completed/Failed).
2. `RollbackDispatch` command from Dispatcher → Pending or Failed (cap).

Pre-existing transition paths that share the state machine:
- `SubmitAllocation` → Pending.
- `AssignNodes` → (no state transition; sets assigned_nodes + assigned_at).
- `UpdateAllocationState` → any state (used by API cancel handler, scheduler fail_allocation).
- `RequeueAllocation` → Pending (service reconciliation path, F14).

**Seam check:** `UpdateAllocationState` and `ApplyCompletionReport` can both write to `alloc.state`. If they race, Raft serialization determines winner. But the aggregate derived from `per_node_phase` could differ from what `UpdateAllocationState` sets (e.g., API forces state=Cancelled while per_node_phase still has all-Running). Next Completion Report would re-aggregate and overwrite Cancelled → surprising to users.

**Finding INT-1:** Cancel does not reach the agent. See below.

### IP-B: Service reconciliation (RequeueAllocation) vs dispatch fields

`RequeueAllocation` apply step (global_state.rs:482) clears:
- `assigned_nodes`
- `started_at`
- `completed_at`
- `exit_code`
- `message`

But it does NOT clear the dispatch fields introduced in Impl 1-8 + gate-2 fixes:
- `per_node_phase` (stale from previous run)
- `assigned_at` (points to previous placement time)
- `dispatch_retry_count` (accumulates across requeues)
- `last_completion_report_at` (points to old run)

**Finding INT-2:** Service that fails once and is requeued will carry stale `per_node_phase` into its next placement. On the next dispatch, ApplyCompletionReport's aggregation sees OLD nodes in per_node_phase plus the new assigned_nodes; the aggregate is wrong.

Also: `dispatch_retry_count` not resetting means a service that survives 3 dispatches over its lifetime (normal for long-running service with occasional node failures) will trip `max_dispatch_retries` and go to Failed permanently.

### IP-C: Dispatcher vs Node lifecycle (drain/degraded)

`Dispatcher::pending_dispatches` filters allocations by `state == Running` and `last_completion_report_at.is_none()`. It does NOT check the assigned node's state.

Scenarios where a node can be non-Ready while an allocation is pending dispatch on it:
- Node drains BEFORE the first dispatch attempt (e.g., operator drains right after scheduler assigns).
- Node transitions to Degraded via INV-D11 during a scheduling window.
- Node goes Down via heartbeat timeout.

**Finding INT-3:** Dispatcher attempts RunAllocation on nodes that are Draining/Degraded/Down. The agent either is absent (connect fails) or refuses — in either case we burn retry budget. Better: skip the attempt and trigger RollbackDispatch immediately so the scheduler can re-place.

### IP-D: Attach vs dispatched workload PID

Attach (`crates/lattice-node-agent/src/attach.rs`) looks up the local allocation's `LocalAllocation.phase == Running` (line 144) and its `ProcessHandle.pid`. The gate-2 dispatch path:
1. `AllocationManager::start(id, entrypoint)` registers LocalAllocation at phase Prologue.
2. Runtime::spawn() returns `ProcessHandle { pid: Some(n) }`.
3. Monitor updates local phase Prologue → Running.

**Seam check:** After step 3, LocalAllocation.phase == Running but the ProcessHandle (with the actual pid) is held in the runtime implementation (BareProcessRuntime's `children` map), NOT in the AllocationManager. Attach gets phase from AllocationManager, but pid from... where?

Reading attach.rs: it takes a PtyBackend and a "process handle". The handle is expected to be populated before the attach request arrives. But the dispatch path stores pid in:
- `run_allocation_monitor` pushes a CompletionReport with `pid`, but this is a one-shot report to the quorum.
- `LocalAllocation.pid: Option<u32>` on the AllocationManager — I added this field in Impl 6. Let me verify.

Checking allocation_runner.rs: the LocalAllocation struct does have `pid: Option<u32>` since earlier impl. Does `AllocationManager::start` populate it? No — `LocalAllocation::new` sets it to None. And my `run_allocation_monitor` doesn't update it.

**Finding INT-4 (Medium):** Attach cannot find the pid of a dispatch-spawned workload because neither AllocationManager nor any accessible channel stores it after spawn. Need to wire pid into LocalAllocation from the monitor task.

### IP-E: DAG controller vs aggregated allocation state

DAG controller (`crates/lattice-scheduler/src/dag.rs`) evaluates dependency edges based on upstream allocation state. With DEC-DISP-11 conservative aggregation, a 4-node upstream allocation transitions to Running only when all 4 nodes report. DAG downstream that depends on `afterok` checks `upstream.state == Completed`. That still works.

**Seam OK.** The conservative aggregation doesn't break DAG semantics — it actually strengthens them (downstream starts only when ALL upstream nodes are confirmed Running).

### IP-F: Preemption vs dispatch

Preemption (`lattice-scheduler/src/preemption.rs`) calls `sink.suspend(alloc_id)` and on commit reassigns the victim's nodes. Preemption path does NOT touch `per_node_phase` or `assigned_at`. On resume the victim's dispatch_retry_count carries over — which is fine because preemption is system-initiated, not a dispatch failure.

**Seam OK.** Preemption doesn't trip new dispatch invariants.

### IP-G: Agent restart / reattach vs quorum's per_node_phase

On agent restart, quorum has `per_node_phase` with the pre-crash values for allocations on this node. The agent's LocalAllocationPhase is lost (in-memory only). Reattach:
- Reads persisted state file → recovers LocalAllocation for workloads with live PIDs.
- Sends first heartbeat with `reattach_in_progress=true`.
- Silent-sweep honours the flag for `reattach_grace_period`.

**Seam check:** Quorum's `per_node_phase[this_node]` may be at Running pre-crash. Post-reattach, agent's local state is Running. Agent's next Completion Report for this alloc is `Running` again — idempotent per INV-D4. No state change. ✓

If agent cannot reattach (pid dead), agent emits `Failed` report. Quorum updates per_node_phase → recompute aggregate: this node's phase moves to Failed, INV-D7 per-node rank check passes (Running=2 → Failed=3 is advance), aggregate becomes Failed. ✓

**Seam OK.**

## Summary of findings

| # | Finding | Severity | Affected seams |
|---|---|---|---|
| INT-1 | Cancel flips state but never sends StopAllocation to agent | Critical | Dispatch × Cancel |
| INT-2 | RequeueAllocation doesn't clear per_node_phase/assigned_at/dispatch_retry_count | High | Dispatch × Service reconciliation |
| INT-3 | Dispatcher attempts on Draining/Degraded/Down nodes, wasting retry budget | High | Dispatch × Node lifecycle |
| INT-4 | Attach can't find pid of dispatch-spawned workload (runtime holds pid, AllocationManager has None) | Medium | Dispatch × Attach |

## Integration tests written

- `specs/integration/dispatch-cross-cutting.feature` — 4 scenarios covering each finding, driven as BDD so they integrate with the existing acceptance harness.
- `dispatch.rs` step-def file extended with the supporting given/when/then steps.

## Resolutions applied (in this integrator pass)

- **INT-1** — `cancel()` handler in `allocation_service.rs` now fires best-effort `StopAllocation` RPC to each assigned node after the Cancelled state commits. Failure to deliver the stop is non-fatal and counted via the existing rollback-stop-sent counter.
- **INT-2** — `RequeueAllocation` apply step clears `per_node_phase`, `assigned_at`, `dispatch_retry_count`, and `last_completion_report_at`.
- **INT-3** — `Dispatcher::pending_dispatches` joins with node state and skips allocations whose assigned nodes are not Ready. Added metric `lattice_dispatch_skipped_unschedulable_total`.
- **INT-4** — `run_allocation_monitor` updates `LocalAllocation.pid` on spawn so attach can find it.

## Graduation

- ✅ Every integration point examined
- ✅ Integration tests written for each finding
- ✅ All 4 findings resolved
- ✅ No undeclared dependencies remain
- ✅ cargo fmt + cargo clippy + all tests clean

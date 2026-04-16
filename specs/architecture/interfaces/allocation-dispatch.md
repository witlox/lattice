# Interface: Allocation Dispatch

Derived from dispatch analyst output Layers 1-6 (2026-04-16) and adversary gate-1 findings. Defines module placement, proto extensions, data-model additions, Raft command shape, and trait boundaries. No implementation — structure and contracts only.

## Traceability

Every element here traces to:
- **Invariants:** INV-D1..INV-D13 (`specs/invariants.md`)
- **Contracts:** IP-02, IP-03, IP-13, IP-14, IP-15 (`specs/cross-context/interactions.md`)
- **Scenarios:** `specs/features/allocation_dispatch.feature`
- **Failure modes:** FM-D1..FM-D10 (`specs/failure-modes.md`)
- **Adversary findings:** `specs/findings/dispatch-analyst-review.md`

## Design decisions (resolving adversary findings)

The first block (DEC-DISP-01..06) resolves High findings from the **analyst-level** adversary pass. The second block (DEC-DISP-07..11) resolves interface-level findings from the **gate-1 architect adversary pass** (2026-04-16). Each decision updates the spec; downstream sections assume these decisions.

### Decision DEC-DISP-01 (addresses D-ADV-03): Degraded node auto-recovery + global guard

**Chosen:** Automatic probing reset + cluster-wide anomaly guard.

**Mechanism:**
- A node in `Degraded` state due to INV-D11 remains Degraded for `degraded_probe_interval` (default 5 minutes), at which point the quorum halves its `consecutive_dispatch_failures` counter and transitions it back to `Ready`. Its next placement is a "probe" — if the probe's dispatch succeeds, the counter resets to 0 and the node is fully recovered. If the probe fails, the counter returns to its pre-halving value and the node goes back to Degraded.
- Additionally, a global guard: if the number of nodes Degraded by INV-D11 exceeds `node_dispatch_failure_ratio_threshold` (default 30%) of all Ready-eligible nodes within `guard_window` (default 10 minutes), the INV-D11 auto-transition is suspended and a `cluster_wide_dispatch_degradation` alarm fires. Subsequent dispatch failures increment counters but do not transition nodes to Degraded until the ratio falls below threshold. This prevents the cluster-wide image-corruption deadlock without requiring operator intervention.

**Spec update:** INV-D11 amended with auto-recovery clause. New assumption A-D22 records the guard thresholds as site-configurable policy knobs.

### Decision DEC-DISP-02 (addresses D-ADV-04): Retry counters are Raft-committed

**Chosen:** Both `allocation.dispatch_retry_count` and `node.consecutive_dispatch_failures` live on Raft-committed entities (`Allocation` and `Node` respectively). Every increment is part of a Raft proposal; neither counter has any non-Raft mirror.

**Rationale:** In-memory alternatives lose the counter on `lattice-api` restart (D-ADV-04 evidence). Raft writes are the authoritative persistence layer for anything the scheduler/dispatcher reasons about.

**Cost:** One additional Raft proposal per Dispatch Failure. Acceptable: Dispatch Failures are rare (measured in events per hour cluster-wide), not hot-path.

### Decision DEC-DISP-03 (addresses D-ADV-05): Reattach announces itself via heartbeat flag

**Chosen:** Option (a) from the finding — agent emits an early heartbeat with `reattach_in_progress: bool = true` within half a heartbeat interval of boot; silent-sweep skips allocations whose assigned node currently holds this flag. New timeout `reattach_grace_period` (default 5 min) bounds the reattach window; if the flag remains after that, silent-sweep resumes normal evaluation (the agent has genuinely failed).

**Rationale:** Option (b) — synchronous reattach before first heartbeat — loses the "workload survives agent restart" guarantee for large cgroup scopes (memory notes 1000s of scopes per node are realistic). Option (a) preserves that guarantee at the cost of one heartbeat flag bit and one new timeout.

**Spec update:** INV-D5 amended with reattach-flag clause. INV-D8 amended to honour `reattach_in_progress`.

### Decision DEC-DISP-04 (addresses D-ADV-06): Single-leader Dispatcher via Raft leadership

**Chosen:** Only the Raft leader's `lattice-api` instance runs the Dispatcher loop. Followers observe but do not act. On leadership change, the new leader picks up where the old one left off via Raft-state replay (Dispatcher is stateless-over-Raft per FM-D10; its "state" is "this allocation is Running-with-assigned-nodes-but-no-phase-transition" which any replica can derive).

**Rationale:** Simpler than coordination via a dispatch_in_flight marker, matches how single-leader schedulers are typically implemented, eliminates the 2× retry budget cost from D-ADV-06. Raft provides the leader election for free.

**Leadership-flap note (closes D-ADV-ARCH-04):** On becoming leader, the Dispatcher pauses for one `heartbeat_interval` before its first tick. This allows any in-flight Completion Reports from the previous leader's attempts to land and advance the allocation state, preventing the new leader from starting a fresh attempt for an allocation that was already successfully dispatched. Budget amplification is bounded to at most one extra attempt per leadership change rather than a full budget reset. INV-D3 idempotency still absorbs any duplicates that do occur.

**Spec update:** IP-02 clause added: "The Dispatcher runs only on the Raft leader. On leadership acquisition, the Dispatcher waits one heartbeat_interval before beginning its loop."

### Decision DEC-DISP-05 (addresses D-ADV-07): `accepted: false` has a typed refusal reason

**Chosen:** Extend `RunAllocationResponse` with a nullable `refusal_reason` enum. Four variants with distinct retry semantics:

| `refusal_reason` | Meaning | Dispatcher behavior |
|---|---|---|
| `BUSY` | Agent temporarily saturated | Retry within current attempt budget (backoff, stay on same node) |
| `UNSUPPORTED_CAPABILITY` | Agent refuses this allocation shape (e.g., missing GPU driver, incompatible uenv) | Immediate rollback to Pending with hint; scheduler re-places on a capable node |
| `MALFORMED_REQUEST` | Allocation spec is bad (e.g., invalid image ref, missing entrypoint) | Immediate transition to Failed; user resubmits with fix |
| `ALREADY_RUNNING` | Agent already has this allocation in its AllocationManager (INV-D3 path) | Treat as success; proceed with Completion Report expectation |

**Unknown reason values (closes D-ADV-ARCH-10):** Protobuf enums are forward-compatible. A Dispatcher receiving a `refusal_reason` value it does not recognize MUST treat it as `MALFORMED_REQUEST` (safe-fail path). This prevents wasted retry budget on agent-side states that the Dispatcher cannot reason about, and surfaces the mismatch to the user who can escalate to operators. Additionally, the Dispatcher emits `lattice_dispatch_unknown_refusal_reason_total{reason_code}` so operators can detect agent/server version skew during rolling upgrades.

**Spec update:** `allocation_dispatch.feature` extended with scenarios for each refusal reason, plus one for unknown-reason-handling. IP-02 failure-modes table extended.

### Decision DEC-DISP-06 (addresses D-ADV-08): Agent address is bound to workload-cert SAN

**Chosen:** Option (a) from the finding — the agent's workload certificate must include the `agent_address` as a Subject Alternative Name (SAN, either DNS or IP). The quorum's `RegisterNode` handler validates that the requested `agent_address` is one of the certificate's SANs; mismatches are rejected.

**Rationale:** Option (b) — operator-provided address via OpenCHAMI — is more restrictive but forces a tight coupling between Lattice and the node-inventory system. Option (a) uses the identity cascade's existing machinery (`hpc-identity::IdentityCascade` issues certs with the right SANs for SPIRE-managed nodes; self-signed CA path can include SANs the operator specifies).

**Cost:** SPIRE registration entries and self-signed CSR templates must include the node's reachable address(es) as SANs. This is a deployment concern, not a runtime concern.

**Spec update:** New INV-D14 captures the cert-to-address binding requirement.

### Decision DEC-DISP-07 (addresses D-ADV-ARCH-01): Rollback is all-or-nothing across assigned nodes

**Chosen:** `RollbackDispatch` operates on the entire allocation's `assigned_nodes` set, not on a single node. On commit, ALL assigned nodes are released atomically, the allocation transitions to `Pending` (or `Failed` at cap), and any agent that had previously accepted RunAllocation on those nodes receives a `StopAllocation` cleanup RPC from the Dispatcher (see DEC-DISP-08).

**Rationale:** A Pending allocation with non-empty `assigned_nodes` violates the ownership model. Partial rollback is not a consistent state. All-or-nothing matches how multi-node HPC jobs are expected to behave ("a job runs on all its nodes or none of them"). One failing dispatch rolls the whole allocation back; the scheduler re-places it wholesale on the next cycle, potentially picking a different node set.

**Cost:** In the asymmetric-failure case (3 of 4 dispatches succeeded, 1 failed), 3 agents receive cleanup StopAllocation RPCs and their in-progress prologues are aborted. This is wasteful but rare — and it is the correct semantics for the user's workload.

**Spec update:** Raft command `RollbackDispatch` amended to take `released_nodes: Vec<NodeId>` instead of `node_id: NodeId`. Apply-step releases all listed nodes atomically in one log entry. INV-D6 statement updated.

### Decision DEC-DISP-08 (addresses D-ADV-ARCH-02): Dispatcher issues StopAllocation cleanup after every rollback

**Chosen:** After `RollbackDispatch` commits, the Dispatcher fires best-effort `NodeAgentService.StopAllocation` RPCs to every node in the released set. Fire-and-forget; no retry, no ack required. Errors are logged + counter `lattice_dispatch_rollback_stop_sent_total{result}` is incremented, but dispatch state progress is not blocked on them. Surviving orphan processes (if StopAllocation never reaches) are caught by INV-D9 orphan-cleanup on next agent boot.

**Rationale:** Covers the race window where the agent's prologue succeeded just as the Dispatcher timed out the last attempt. Without this cleanup the orphan runs until walltime (or forever for unbounded allocations). Making StopAllocation best-effort keeps the Dispatcher loop simple and non-blocking; INV-D9 is the belt-and-suspenders correctness layer.

**Spec update:** Dispatcher contract amended with `cleanup_after_rollback(allocation_id, released_nodes)` step. New scenario in `allocation_dispatch.feature`. New counter in the observability list.

### Decision DEC-DISP-09 (addresses D-ADV-ARCH-03): AllocationManager is actor-owned

**Chosen:** The `AllocationManager` is owned by a single task; all mutations go through an `mpsc::Sender<AllocationManagerCmd>`. Read-only queries also go through the channel (return channel or oneshot). This matches the existing `AgentCommand` / `cmd_rx` idiom in the agent (the same channel that was disconnected and caused the original dispatch gap — now used for its intended purpose).

**Rationale:** Lock-free by construction. No deadlocks. Correctness per INV-D3 (idempotent registration) and INV-D13 (latest-wins buffer) both require serialized access to the allocations map and the completion buffer; actor pattern provides this without explicit locks.

**Cost:** One async hop per mutation. Agent is not latency-critical at this granularity (heartbeats are 10s, RPCs are per-allocation not per-event). Throughput is bounded by the actor loop, which is adequate for the workload-count range (< 1000 per node).

**Spec update:** AllocationManager trait rewritten in terms of channel commands. All methods are `async fn` returning `Result<T, LatticeError>`.

### Decision DEC-DISP-10 (addresses D-ADV-ARCH-05): `reattach_in_progress` is Raft-committed via extended `RecordHeartbeat`

**Chosen:** The flag is a persisted field on the `Node` Raft-committed record. It is updated by extending the existing `Command::RecordHeartbeat` with a `reattach_in_progress: bool` parameter; every heartbeat's value is committed. The scheduler (including followers reading the state to simulate cycles) sees the current value.

**Rationale:** Heartbeats are already Raft-committed per IP-03 for node ownership state transitions. Adding one bool is negligible. Eliminates the ambiguity called out by D-ADV-ARCH-05 and the follower-vs-leader read inconsistency that would otherwise arise.

**Cost:** Every heartbeat now mutates Raft state (was: most heartbeats only update in-memory node health, only state changes committed). The cost is bounded — heartbeat rate is per-node-per-10s, so a 1000-node cluster sees 100 writes/sec, well within Raft throughput budget.

**Spec update:** `Command::RecordHeartbeat` gains `reattach_in_progress: bool` field. `Node` data model's flag is authoritatively owned by this command; heartbeat proto value flows into it on apply.

### Decision DEC-DISP-11 (addresses D-ADV-ARCH-08): Multi-node allocation uses conservative phase aggregation

**Chosen:** For an allocation with N assigned nodes, the global `AllocationState` is derived from the per-LocalAllocation phases as follows:
  - **Staging**: any assigned node's LocalAllocation is in `Prologue`.
  - **Running**: ALL assigned nodes' LocalAllocations are in `Running` or beyond (`Epilogue` counts as past `Running`).
  - **Completed**: ALL assigned nodes' LocalAllocations are in `Completed` with `exit_code == 0`.
  - **Failed**: ANY assigned node's LocalAllocation is in `Failed`, OR ALL are in `Completed` with at least one non-zero exit.
  - **Cancelled**: the user cancel path takes precedence over all of the above.

**Rationale:** Matches Slurm batch semantics (all-ranks-alive for `RUNNING`, any-rank-failed for `FAILED`). Matches MPI workload expectations (cannot start inter-rank communication until all ranks exist). Clear and predictable for users.

**Cost:** Slowest straggler dominates the Staging → Running transition time. Acceptable: this is intrinsic to multi-node HPC jobs; the scheduler's topology-aware placement already minimizes prologue skew.

**Spec update:** INV-D7 amended with aggregation rule. Quorum's `ApplyCompletionReport` apply-step evaluates the full assigned-node set and computes the aggregated state per this rule; allocation.state transitions only when the rule changes the aggregate. New scenario added: "Multi-node allocation remains Staging until all nodes report Running."

---

## Module placement

### Dispatcher: lives in `lattice-api`, runs only on the Raft leader

The Dispatcher is a background task owned by the `lattice-api` process. It is a consumer of `GlobalState` (read) and a producer of Raft proposals (write), plus a gRPC client of `NodeAgentService` on each target agent.

```
┌─────────────────────────────────────────────────────────────────┐
│                         lattice-api                              │
│  ┌─────────────────────────┐     ┌───────────────────────────┐   │
│  │   API handlers (gRPC,   │     │      Dispatcher           │   │
│  │   REST gateway, auth,   │     │   (Raft-leader-only)      │   │
│  │   rate limit, etc.)     │     │                           │   │
│  └─────────────────────────┘     │  Reads: GlobalState       │   │
│                                   │  Writes: Raft proposals   │   │
│                                   │  Calls: NodeAgentService  │   │
│                                   │  Runs on: Raft leader only │   │
│                                   └──────┬──────────┬──────┘   │
│                                          │          │           │
└──────────────────────────────────────────┼──────────┼───────────┘
                                           │          │
                        Raft propose ─────▶│          │
                                           │          │
                                           │          ▼
                                           ▼     gRPC (mTLS)
                                 ┌─────────────────┐        ┌─────────────────┐
                                 │ lattice-quorum  │        │ lattice-node-   │
                                 │                 │        │  agent          │
                                 │ Validates + apply│       │                 │
                                 │ INV-D6, D12, D14│        │ RunAllocation,  │
                                 └─────────────────┘        │ AllocationMgr,  │
                                                            │ Runtime trait   │
                                                            └─────────────────┘
```

**Why leader-only:** See DEC-DISP-04. No coordination needed; leadership change causes automatic handoff of dispatch responsibility.

**Not a separate binary:** The Dispatcher is tightly coupled to the Raft state reader. Co-locating with the API server minimises moving parts and deployment units. A standalone binary would duplicate config, deployment templates, TLS bundles, and observability wiring.

---

## Data model additions (shared kernel)

All additions live in `lattice-common` and are part of the Raft-committed state.

### Allocation extensions

Append to `Allocation` struct:

```
Allocation {
  // ... existing fields ...

  /// Number of Dispatch Failures this allocation has experienced across
  /// potentially-different nodes. Incremented atomically with the
  /// RollbackDispatch Raft command. Capped at `max_dispatch_retries`
  /// (default 3), at which point the allocation becomes Failed rather
  /// than Pending on next rollback.
  dispatch_retry_count: u32,

  /// Monotonic version counter for optimistic concurrency (INV-D6).
  /// Increments on every state-changing Raft command affecting this
  /// allocation (state transition, node assignment, rollback, requeue).
  /// Separate from `requeue_count` (which tracks service lifecycle).
  state_version: u64,
}
```

### Node extensions

Append to `Node` struct:

```
Node {
  // ... existing fields ...

  /// Reachable gRPC endpoint for this node's agent. Set during
  /// RegisterNode and updateable via UpdateNodeAddress. Format:
  /// "host:port" where host is resolvable on the lattice HSN/mgmt
  /// network. Must match a SAN in the agent's workload certificate
  /// (INV-D14). Empty/invalid addresses rejected (INV-D1).
  agent_address: String,

  /// Consecutive dispatch failures attributed to this node. Incremented
  /// on Dispatch Failure in the same Raft proposal as allocation
  /// rollback. Reset to 0 by any successful Dispatch on this node.
  /// Threshold `max_node_dispatch_failures` (default 5) transitions
  /// the node to Degraded (INV-D11).
  consecutive_dispatch_failures: u32,

  /// When the node entered Degraded via INV-D11. Used by the
  /// auto-recovery probing mechanism (DEC-DISP-01): after
  /// `degraded_probe_interval` since this timestamp, the node
  /// is probed back to Ready with halved counter.
  degraded_at: Option<DateTime<Utc>>,
}
```

### Heartbeat extensions

The node agent's heartbeat gains `reattach_in_progress` and a list of Completion Reports:

```
Heartbeat {
  // ... existing fields ...

  /// The agent has completed its boot sequence but is still reattaching
  /// to surviving Workload Processes. Silent-node sweep (INV-D8) skips
  /// allocations assigned to this node while this flag is set, bounded
  /// by `reattach_grace_period` (DEC-DISP-03).
  reattach_in_progress: bool,

  /// Completion Reports accumulated since the previous heartbeat,
  /// keyed by allocation_id (INV-D13 — latest-wins per allocation).
  /// Order of entries in the list is not meaningful.
  completion_reports: Vec<CompletionReport>,
}

CompletionReport {
  allocation_id: AllocId,
  phase: LocalAllocationPhase,  // Staging | Running | Completed | Failed
  pid: Option<u32>,              // Populated when transitioning to Running
  exit_code: Option<i32>,        // Populated for Completed/Failed
  reason: Option<String>,        // Free-form for Failed; short for others
}
```

---

## Proto extensions

All proto changes are additive (per assumption A-D6). Field numbers pick up from existing max in each message.

### `lattice/v1/nodes.proto`

```
// Additive to RegisterNodeRequest
message RegisterNodeRequest {
  // ... existing fields ...
  string agent_address = N;   // Required. Validated against cert SANs.
}

// New RPC for address-only updates (IP-13)
rpc UpdateNodeAddress(UpdateNodeAddressRequest)
    returns (UpdateNodeAddressResponse);

message UpdateNodeAddressRequest {
  string node_id = 1;
  string new_address = 2;  // Must match current cert SANs.
}
message UpdateNodeAddressResponse {}

// Additive to HeartbeatRequest
message HeartbeatRequest {
  // ... existing fields ...
  bool reattach_in_progress = N;
  repeated CompletionReport completion_reports = N+1;
}

message CompletionReport {
  string allocation_id = 1;
  Phase phase = 2;
  optional uint32 pid = 3;
  optional int32 exit_code = 4;
  optional string reason = 5;

  enum Phase {
    STAGING = 0;
    RUNNING = 1;
    COMPLETED = 2;
    FAILED = 3;
  }
}
```

### `lattice/v1/agent.proto`

```
// Additive to RunAllocationResponse
message RunAllocationResponse {
  bool accepted = 1;
  string message = 2;
  optional RefusalReason refusal_reason = 3;  // DEC-DISP-05
}

enum RefusalReason {
  BUSY = 0;
  UNSUPPORTED_CAPABILITY = 1;
  MALFORMED_REQUEST = 2;
  ALREADY_RUNNING = 3;  // Idempotency path; treated as success
}
```

---

## Raft command additions

All commands extend the existing `Command` enum in `lattice-quorum`. Each is validated and applied atomically by `GlobalState::apply()`.

```
Command::RollbackDispatch {
    allocation_id: AllocId,
    released_nodes: Vec<NodeId>,  // DEC-DISP-07: all assigned nodes, not just one
    observed_state_version: u64,  // Optimistic concurrency (INV-D6)
    reason: String,               // e.g., "agent_unreachable", "accepted_false_unsupported"
    failed_node: Option<NodeId>,  // The single node whose dispatch failed, for counter attribution
}
// Atomic effects (single Raft log entry):
//   allocation.state ← Pending or Failed (if at cap);
//   allocation.dispatch_retry_count += 1;
//   allocation.state_version += 1;
//   Release ownership of ALL released_nodes for allocation_id (DEC-DISP-07);
//   If failed_node is set:
//     failed_node.consecutive_dispatch_failures += 1;
//     If failed_node.consecutive_dispatch_failures == max_node_dispatch_failures
//       AND global dispatch-ratio guard allows:
//       failed_node.state ← Degraded, failed_node.degraded_at ← now();
// Post-commit (non-Raft, Dispatcher responsibility per DEC-DISP-08):
//   For each n in released_nodes where dispatch had been accepted:
//     fire-and-forget StopAllocation(allocation_id) to n;
//     increment lattice_dispatch_rollback_stop_sent_total{result};

Command::ApplyCompletionReport {
    node_id: NodeId,
    allocation_id: AllocId,
    phase: Phase,
    pid: Option<u32>,
    exit_code: Option<i32>,
    reason: Option<String>,
}
// Validates in order (per Completion Report validation order in IP-03):
//   1. INV-D12: node_id ∈ allocation.assigned_nodes — else reject
//   2. INV-D7: phase strictly advances this node's current LocalAllocation phase — else reject
//   3. INV-D4: (allocation_id, node_id, phase) not already applied — else no-op
// On apply:
//   node's LocalAllocation.phase ← phase
//   allocation.last_completion_report_at ← now()       (for INV-D8 no-progress detection)
//   allocation.state ← aggregate(all LocalAllocation phases) per DEC-DISP-11 rule
//   allocation.state_version += 1 (if aggregated state changed)
//   If phase == Staging or Running:
//     node.consecutive_dispatch_failures ← 0 (reset on successful dispatch)

Command::UpdateNodeAddress {
    node_id: NodeId,
    new_address: String,
    cert_sans: Vec<String>,  // Provided by mTLS middleware layer
}
// Validates: new_address ∈ cert_sans (INV-D14)
// Rate-limited at 1 per 5s per node_id at quorum side

// Existing Command::RecordHeartbeat extended (DEC-DISP-10):
Command::RecordHeartbeat {
    id: NodeId,
    timestamp: DateTime<Utc>,
    owner_version: u64,            // existing
    reattach_in_progress: bool,    // NEW: Raft-committed flag (DEC-DISP-10 / INV-D5)
}
// Apply effects (additive to existing heartbeat apply):
//   node.last_heartbeat ← timestamp;
//   node.reattach_in_progress ← reattach_in_progress;
// Rate-limiting per heartbeat path unchanged.

Command::ProbeReleaseDegradedNode {
    node_id: NodeId,
    expected_degraded_at: DateTime<Utc>,
}
// Fired by the Dispatcher on Raft leader every `degraded_probe_interval` tick
// Validates: node.state == Degraded AND node.degraded_at == expected_degraded_at
// On apply:
//   node.consecutive_dispatch_failures /= 2
//   node.state ← Ready
//   node.degraded_at ← None
// On next dispatch failure: restore counter + re-degrade (per DEC-DISP-01)
```

No commands are removed. The existing `AssignNodes` command already has `expected_version: Option<u64>` which covers Dispatcher's optimistic-concurrency check when assigning nodes to a re-queued allocation.

---

## Dispatcher trait (lattice-api)

```
/// Runs on the Raft leader only. Observes un-acked Running allocations and
/// performs RunAllocation RPC with bounded retry. On failure, submits
/// RollbackDispatch (all-or-nothing per DEC-DISP-07) and fires best-effort
/// StopAllocation cleanup (DEC-DISP-08).
pub trait Dispatcher: Send + Sync {
    /// Single dispatcher-loop iteration. Called every tick.
    /// Returns the number of allocations acted on this iteration.
    async fn tick(&self) -> Result<usize, LatticeError>;

    /// Called externally on Raft leadership change. When becoming leader,
    /// start dispatching; when losing leadership, halt in-flight attempts.
    fn on_leadership_change(&self, is_leader: bool);
}

/// One Dispatch Attempt targeting a specific (allocation, node). Returned by
/// an internal helper `attempt_dispatch(alloc_id, node_id)`.
pub enum DispatchOutcome {
    Accepted,
    AcceptedAlreadyRunning,  // INV-D3 short-circuit path
    TransientFailure(String),  // Will retry within budget
    RefusalBusy(String),       // Retry within budget, longer backoff
    RefusalUnsupported(String), // Rollback immediately to Pending
    RefusalMalformed(String),   // Transition to Failed
}

/// Per-allocation dispatch orchestration. DEC-DISP-07 + DEC-DISP-08 apply
/// here: on rollback, ALL assigned nodes are released atomically AND every
/// node that had accepted RunAllocation receives cleanup StopAllocation.
pub struct DispatchSession {
    allocation_id: AllocId,
    assigned_nodes: Vec<NodeId>,
    // Per-node attempt state (in-memory, rebuilt after leadership flap by
    // re-reading GlobalState).
    per_node: HashMap<NodeId, PerNodeDispatchState>,
}

pub enum PerNodeDispatchState {
    NotYetAttempted,
    InFlight { attempt: u32, started_at: Instant },
    Accepted,       // Agent returned accepted: true — StopAllocation required on rollback
    FailedFinal,    // All retries exhausted on this node — NO StopAllocation needed
}
```

### Dispatcher cleanup-after-rollback contract

When `RollbackDispatch` commits, the Dispatcher performs cleanup in the same tick:

```
async fn cleanup_after_rollback(
    &self,
    allocation_id: AllocId,
    released_nodes: Vec<NodeId>,
    accepted_nodes: Vec<NodeId>,  // Subset: those that had responded accepted: true
) -> () {
    // Best-effort — no retry, no ack, no blocking on failure.
    for node_id in accepted_nodes {
        let _ = spawn_task(async move {
            let resp = agent_client(node_id).stop_allocation(
                StopAllocationRequest {
                    allocation_id,
                    grace_period_seconds: 30,
                    reason: "dispatch_rolled_back".into(),
                }
            ).await;
            metrics::counter!(
                "lattice_dispatch_rollback_stop_sent_total",
                "result" => if resp.is_ok() { "success" } else { "failure" }
            ).increment(1);
        });
    }
    // Not awaited — fire-and-forget. INV-D9 orphan cleanup is the belt-and-
    // suspenders layer for any StopAllocation that doesn't land.
}
```

`accepted_nodes` is derived from the `PerNodeDispatchState` map maintained by the `DispatchSession`. After leadership flap, the new leader's session starts fresh with `NotYetAttempted` for all nodes — so a leadership-flap-induced rollback may skip StopAllocation for nodes that accepted under the old leader. INV-D9 backstops this case; the window is bounded by `reattach_grace_period`.

The Dispatcher does NOT own or cache any per-allocation state. All state comes from `GlobalState` reads:

```
fn pending_dispatches(&self, state: &GlobalState) -> Vec<(AllocId, Vec<NodeId>)>
    // Returns allocations in state Running whose assigned_nodes have no
    // phase-advancing Completion Report yet (allocation.state == Running
    // but no corresponding node has reported Staging/Running on this
    // state_version).
```

---

## Runtime trait (lattice-node-agent)

Existing `UenvRuntime` and `PodmanRuntime` become implementations of a new unifying trait. `BareProcessRuntime` is added.

```
pub trait Runtime: Send + Sync {
    /// Prologue: prepare the execution environment. For uenv:
    /// mount squashfs. For podman: pull image + create namespace.
    /// For bare-process: create cgroup scope, strip secret env vars
    /// per allow-list (D-ADV-13 resolution).
    async fn prepare(&self, alloc: &Allocation) -> Result<PrologueResult, RuntimeError>;

    /// Spawn the entrypoint and return a handle. Must populate
    /// WorkloadProcess.pid with the real OS PID.
    async fn spawn(
        &self,
        alloc: &Allocation,
        prologue: PrologueResult,
    ) -> Result<WorkloadProcess, RuntimeError>;

    /// Epilogue: clean up scratch, teardown namespaces, unmount,
    /// stop container. Idempotent.
    async fn epilogue(
        &self,
        alloc: &Allocation,
        process: &WorkloadProcess,
    ) -> Result<(), RuntimeError>;

    /// Which runtime variant (for refusal-reason reporting).
    fn variant(&self) -> RuntimeVariant;
}

pub enum RuntimeVariant {
    BareProcess,
    Uenv,
    Podman,
}

pub struct WorkloadProcess {
    pid: u32,
    cgroup_path: PathBuf,
    started_at: DateTime<Utc>,
}
```

**Runtime selection** (contract, not implementation):

```
fn select_runtime(alloc: &Allocation) -> Result<Box<dyn Runtime>, RuntimeError> {
    match (alloc.environment.uenv.as_ref(), alloc.environment.image.as_ref()) {
        (Some(_), Some(_)) => Err(AmbiguousRuntime),  // MalformedRequest
        (Some(_), None)    => Ok(Box::new(UenvRuntime::new(...))),
        (None,    Some(_)) => Ok(Box::new(PodmanRuntime::new(...))),
        (None,    None)    => Ok(Box::new(BareProcessRuntime::new(...))),
    }
}
```

### BareProcessRuntime: new, minimal isolation, secret-scrubbing env

```
pub struct BareProcessRuntime {
    cgroup_mgr: Arc<dyn CgroupManager>,  // from hpc-node trait
    agent_env_allow_list: Vec<String>,   // D-ADV-13 resolution
}
```

**Environment scrubbing contract** (closes D-ADV-ARCH-11):

The allocation inherits a filtered subset of the agent's environment. Stripped (block-list, case-insensitive glob match):
- `LATTICE_*` (all)
- `VAULT_*` (all)
- Any variable matching `*SECRET*`, `*TOKEN*`, `*KEY*`, `*PASSWORD*`, `*PASSWD*`
- Explicit `agent_env_block_list` from site config

Passed through (allow-list):
- Shell basics: `PATH`, `HOME`, `USER`, `LANG`, `LC_*`, `TZ`, `SHELL`, `TMPDIR`, `XDG_RUNTIME_DIR`
- HPC compute: `OMP_NUM_THREADS`, `OMP_PLACES`, `OMP_PROC_BIND`, `OMP_*` (rest of OpenMP), `MKL_NUM_THREADS`
- GPU selection: `CUDA_VISIBLE_DEVICES`, `CUDA_DEVICE_ORDER`, `ROCR_VISIBLE_DEVICES`, `HIP_VISIBLE_DEVICES`, `GPU_DEVICE_ORDINAL`
- MPI / networking: `PMI_*`, `MPI_*`, `OMPI_*`, `MPICH_*`, `FI_*` (libfabric), `NCCL_*`, `CXI_*`
- Dynamic linker: `LD_LIBRARY_PATH`, `LD_PRELOAD` — pass-through but logged at debug level; site policy may move to block-list for security-conscious sites
- Lattice allocation context: `LATTICE_ALLOC_ID`, `LATTICE_JOB_NAME`, `LATTICE_NODELIST`, `LATTICE_NNODES`, `LATTICE_NPROCS`, `LATTICE_TASK_INDEX`, `LATTICE_TASK_GROUP_ID`, `LATTICE_SUBMIT_DIR`
- Slurm compat (opt-in via `compat.set_slurm_env = true`): `SLURM_*`
- Site-configured allow-list entries

**Merge semantics with `AllocationSpec.env_vars`:** user-provided env vars from `env_vars` are applied LAST (overriding anything above), BUT are still subject to the block-list — a user cannot smuggle `LATTICE_AGENT_TOKEN` through `env_vars`. The Bare-Process runtime validates each user-provided key against the block-list at prologue time and rejects with `MALFORMED_REQUEST` if a forbidden key is present. Precedence summary: user env_vars > site allow-list > agent env (all filtered by block-list).

**No mount namespace.** Bare-Process runs in a new cgroup scope only. The entrypoint inherits the agent's mount namespace directly. For isolation, use Uenv or Podman.

---

## Agent: RunAllocation handler

```
impl NodeAgentService for NodeAgentServer {
    async fn run_allocation(
        &self,
        request: Request<RunAllocationRequest>,
    ) -> Result<Response<RunAllocationResponse>, Status> {
        // 1. Parse + validate request (malformed → RefusalMalformed)
        // 2. INV-D3 idempotency: check AllocationManager
        //    If already present → RefusalAlreadyRunning
        // 3. Capability check → RefusalUnsupported if incompatible
        // 4. Busy check (agent saturation) → RefusalBusy
        // 5. Select runtime, register allocation in AllocationManager
        //    with phase Prologue
        // 6. Return accepted: true, refusal_reason: None
        // 7. Background task: prologue → spawn → monitor → epilogue
        //    Emits Completion Reports at each phase transition:
        //      Prologue done → Staging (pid: None)
        //      Spawn done    → Running (pid: Some(n))
        //      Exit/epilogue → Completed or Failed (exit_code)
    }
}
```

**Allocation Manager (actor-owned per DEC-DISP-09):**

```
/// Commands sent to the AllocationManager actor. All mutations AND reads
/// go through this channel to guarantee serialized access without locks.
pub enum AllocationManagerCmd {
    /// Start a new local allocation. Replies with Err if the allocation is
    /// already registered (INV-D3 path — caller should respond
    /// RefusalAlreadyRunning).
    Start {
        alloc: Allocation,
        entrypoint: String,
        runtime_variant: RuntimeVariant,
        reply: oneshot::Sender<Result<(), AllocationManagerError>>,
    },

    /// Record that the runtime has successfully spawned the process.
    /// Sets pid, cgroup_path; advances phase from Prologue to Running;
    /// emits a CompletionReport into the buffer.
    RecordSpawned {
        id: AllocId,
        pid: u32,
        cgroup_path: PathBuf,
        reply: oneshot::Sender<Result<(), AllocationManagerError>>,
    },

    /// Record terminal exit. Sets exit_code; advances phase; emits terminal
    /// CompletionReport.
    RecordExited {
        id: AllocId,
        exit_code: i32,
        failure_reason: Option<String>,
        reply: oneshot::Sender<Result<(), AllocationManagerError>>,
    },

    /// Push a direct CompletionReport (e.g., reattach-detected death).
    /// Upserts per INV-D13.
    PushReport {
        report: CompletionReport,
        reply: oneshot::Sender<Result<(), AllocationManagerError>>,
    },

    /// Drain all reports for the next heartbeat. Returns a Vec sized by
    /// the number of distinct active allocations; clears buffer on return.
    DrainReports {
        reply: oneshot::Sender<Vec<CompletionReport>>,
    },

    /// Idempotency query (INV-D3). Returns true if allocation is known
    /// and not yet in a terminal phase.
    ContainsActive {
        id: AllocId,
        reply: oneshot::Sender<bool>,
    },

    /// Mark allocation as Cancelled (from StopAllocation RPC). Emits
    /// terminal CompletionReport; runs epilogue.
    Cancel {
        id: AllocId,
        grace_period: Duration,
        reply: oneshot::Sender<Result<(), AllocationManagerError>>,
    },
}

/// The actor loop. Owned by a single tokio task spawned at agent startup;
/// other components (RPC server, runtime monitors, heartbeat loop) hold
/// `mpsc::Sender<AllocationManagerCmd>` handles.
pub async fn run_allocation_manager(
    mut rx: mpsc::Receiver<AllocationManagerCmd>,
) {
    let mut state = AllocationManagerState {
        allocations: HashMap::new(),
        completion_buffer: HashMap::new(),
    };
    while let Some(cmd) = rx.recv().await {
        handle_cmd(&mut state, cmd).await;
    }
}

/// Internal state. Never exposed outside the actor.
struct AllocationManagerState {
    allocations: HashMap<AllocId, LocalAllocation>,
    completion_buffer: HashMap<AllocId, CompletionReport>,  // INV-D13 keyed map
}
```

**Lifecycle note:** the actor is spawned at agent startup, reused across the process lifetime. It has no persistence — state survives agent restart via `state::save_state()` from main.rs (existing mechanism). The actor is notified of reattach completion via an initialization command `ReinstateFromState` (not shown; trivial extension of `Start`).

`LocalAllocation` extended to track the real PID:

```
pub struct LocalAllocation {
    id: AllocId,
    phase: LocalAllocationPhase,
    entrypoint: String,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    // NEW:
    pid: Option<u32>,        // Populated when runtime.spawn() succeeds
    cgroup_path: Option<PathBuf>,
    runtime_variant: RuntimeVariant,
}
```

---

## Scheduler: Silent-node reconciliation sweep

IP-15 is realised as a new pass in the existing scheduler cycle. It runs AFTER normal placement and AFTER drain completion.

```
// In lattice-scheduler::loop_runner
impl SchedulerLoop {
    async fn run_cycle(&self, ...) {
        // existing passes:
        // 1. pending_allocations → placement decisions → AssignNodes commands
        // 2. draining nodes → drain completion checks → Drained transitions
        // 3. failed allocations → reconciliation → Requeue

        // NEW:
        // 4. silent-node sweep (IP-15)
        //    for each allocation in state Running:
        //      if any assigned node's latest_heartbeat_at > now - (heartbeat_interval + grace_period)
        //         AND node.reattach_in_progress == false
        //         AND allocation.state_version has not advanced this cycle:
        //           propose RecordCompletionReport phase=Failed reason=node_silent
    }
}
```

Trait extension:

```
pub trait SchedulerStateReader {
    // ... existing methods ...

    /// Returns nodes whose latest heartbeat is older than the grace window
    /// AND are NOT currently in reattach_in_progress. Used by silent-node sweep.
    async fn silent_nodes(
        &self,
        grace: Duration,
    ) -> Result<Vec<(NodeId, DateTime<Utc>)>, LatticeError>;
}
```

---

## Rate limiting and concurrency

Per D-ADV-12 (Medium; resolution in architecture):

```
pub struct DispatcherConfig {
    /// Max concurrent Dispatch Attempts in flight globally.
    pub max_concurrent_attempts: usize,   // default 64

    /// Max concurrent Dispatch Attempts targeting one agent.
    pub per_agent_concurrency: usize,     // default 8

    /// Per-attempt timeout (single RPC call).
    pub attempt_timeout: Duration,        // default 1s

    /// Per-allocation retry budget.
    pub max_dispatch_retries: u32,        // default 3

    /// Per-attempt backoff schedule on one node.
    pub attempt_backoff: Vec<Duration>,   // default [1s, 2s, 5s]

    /// Per-node cap on consecutive dispatch failures before Degraded.
    pub max_node_dispatch_failures: u32,  // default 5

    /// Degraded auto-recovery probe interval (DEC-DISP-01).
    pub degraded_probe_interval: Duration, // default 5 min

    /// Cluster-wide dispatch failure guard (DEC-DISP-01).
    pub node_dispatch_failure_ratio_threshold: f64,  // default 0.3
    pub guard_window: Duration,            // default 10 min

    /// Reattach grace period (DEC-DISP-03).
    pub reattach_grace_period: Duration,   // default 5 min
}
```

---

## Dispatcher ↔ Reconciler race (D-ADV-10 resolution)

Both components observe "Running allocation with no phase transition." Priority per IP-15 updated:

- **Silent-node sweep** (IP-15) filters out allocations whose assigned nodes have been heartbeating within the grace window. By construction, the sweep only fires when the node is genuinely silent. If the Dispatcher is in the middle of a retry on the same allocation, the node's heartbeat flag from a prior cycle already disqualifies the allocation from sweep. Therefore no explicit dispatch_in_flight flag is needed.
- **Tie-break if both commit anyway**: both commands carry `expected_state_version`. Whichever Raft-committed first advances the version; the loser's expected_version check fails and the proposal is rejected. Safe by INV-D6's optimistic concurrency pattern.

This resolves D-ADV-10 without new state. Updated IP-15 clause: "Silent-sweep excludes allocations whose assigned nodes have a heartbeat within the grace window (regardless of whether Dispatcher is acting); version-check resolves the residual race."

---

## Enforcement map (INV-D1..D14)

Added to `specs/architecture/enforcement-map.md`:

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| INV-D1 Node has agent address | lattice-quorum | `Command::RegisterNode` and `Command::UpdateNodeAddress` validation in `GlobalState::apply()`; empty/invalid address rejected | Gherkin scenarios in `allocation_dispatch.feature`, unit tests on Command apply |
| INV-D2 Addressless nodes scheduler-invisible | lattice-scheduler | `SchedulerStateReader::available_nodes()` impl filters on `agent_address.is_some() && !is_empty()` | Unit test on reader |
| INV-D3 Dispatch idempotency (agent) | lattice-node-agent | `NodeAgentServer::run_allocation` consults `AllocationManager::contains_active()` before Runtime.prepare | Unit tests on AllocationManager |
| INV-D4 Completion Report idempotency (quorum) | lattice-quorum | `Command::ApplyCompletionReport` evaluates idempotency at Raft-apply step, not submit | Unit tests on apply() |
| INV-D5 Running ⇒ live process or recovery | lattice-node-agent + lattice-scheduler | Joint: agent AllocationManager has PID and phase; reattach sets `reattach_in_progress` flag in heartbeat; silent-sweep (INV-D8) honours flag | Gherkin reattach scenarios |
| INV-D6 Dispatch failure atomic rollback | lattice-quorum | `Command::RollbackDispatch` is a single Raft log entry that applies state transition + retry_count + node release + counter increment atomically | Unit test on command apply |
| INV-D7 Phase monotonicity | lattice-quorum | `Command::ApplyCompletionReport` apply-step validates phase advance; counter `lattice_completion_report_phase_regression_total` on rejection | Unit test on apply() |
| INV-D8 Heartbeat-bounded completion | lattice-scheduler | Silent-node sweep in SchedulerLoop::run_cycle after placement pass; respects `reattach_in_progress` flag | Gherkin node_silent scenarios |
| INV-D9 Orphan cleanup on boot | lattice-node-agent | Agent boot sequence scans workload.slice, cross-references AgentState; unknown scopes terminated + audit event emitted | Gherkin orphan scenario |
| INV-D10 Per-attempt address resolution | lattice-api (Dispatcher) | `Dispatcher::attempt_dispatch` reads `node.agent_address` from GlobalState at the start of each attempt; no attempt-local cache | Unit test on Dispatcher |
| INV-D11 Dispatch failures degrade node | lattice-quorum | `Command::RollbackDispatch` apply step increments `node.consecutive_dispatch_failures`; threshold check transitions to Degraded when global guard permits | Unit test + integration test |
| INV-D12 Completion Report source auth | lattice-quorum | `Command::ApplyCompletionReport` apply-step checks reporting node ∈ allocation.assigned_nodes; `lattice_completion_report_cross_node_total` counter | Unit test on apply() |
| INV-D13 Buffer latest-wins per allocation | lattice-node-agent | `AllocationManager::push_report` upserts into keyed HashMap; drain into heartbeat | Unit test on AllocationManager |
| INV-D14 Agent address bound to cert SAN | lattice-quorum + lattice-api (mTLS middleware) | mTLS middleware extracts cert SANs at handshake; `Command::RegisterNode` and `UpdateNodeAddress` validate `new_address ∈ cert_sans` | Integration test (mTLS path) |

---

## Observability additions

Metrics emitted on all paths (architect-level contract; implementer wires):

| Counter | When incremented |
|---|---|
| `lattice_dispatch_attempt_total{node_id, result}` | Every dispatch attempt |
| `lattice_dispatch_rollback_total{reason}` | Every RollbackDispatch commit |
| `lattice_dispatch_rollback_raced_by_completion_total` | IP-14 race winner is completion |
| `lattice_dispatch_rollback_stop_sent_total{result}` | DEC-DISP-08 cleanup StopAllocation fire-and-forget outcome |
| `lattice_dispatch_unknown_refusal_reason_total{reason_code}` | DEC-DISP-05: agent returned a refusal_reason unknown to the current Dispatcher; surfaces rolling-upgrade skew |
| `lattice_completion_report_cross_node_total{node_id}` | INV-D12 rejection |
| `lattice_completion_report_phase_regression_total{current_phase, reported_phase}` | INV-D7 rejection |
| `lattice_completion_report_duplicate_total` | INV-D4 idempotent no-op |
| `lattice_dispatch_report_buffer_exhausted_total{node_id}` | FM-D8 cardinality anomaly |
| `lattice_node_degraded_by_dispatch_total{node_id}` | INV-D11 transition |
| `lattice_node_recovered_from_degraded_total{node_id}` | DEC-DISP-01 probe success |
| `lattice_cluster_wide_dispatch_degradation_active` | Gauge: 1 when guard fires |

---

## Gaps / uncertainties (to be attacked after the re-review)

1. **Probe-release timing precision.** DEC-DISP-01 uses `degraded_probe_interval` + halving. Under rapid failures, a node could bounce Degraded ⇄ Ready repeatedly. No upper-bound on bounce rate specified.
2. **Interaction with preemption.** Preemption pipeline is separate; if a node is Degraded mid-preemption, the behavior of the preemption retry is not specified.
3. **SPIRE/self-signed cert SAN population.** DEC-DISP-06 assumes the cert has the right SANs. Ops concern documented in architecture, but the implementation side (SPIRE registration entries, self-signed CSR templates) needs handling in deployment docs.
4. **Dispatch-session rebuild after leadership flap.** `DispatchSession.per_node` is in-memory (per DEC-DISP-09 the actor owns AllocationManager, but Dispatcher's own session state is separate). Leadership change loses this. DEC-DISP-08's cleanup may miss `Accepted` nodes whose state was set under the old leader. INV-D9 backstops; bounded-by-reattach-grace is the window.

Gap D-ADV-ARCH-02 ("multi-node asymmetric dispatch") is closed by DEC-DISP-07 (all-or-nothing rollback). Gap D-ADV-ARCH-03 ("thread safety") is closed by DEC-DISP-09 (actor-owned AllocationManager).

# Adversarial Review: Dispatch Architecture (Gate 1)

Review date: 2026-04-16
Reviewer role: Adversary
Mode: Architecture (specs + architecture interfaces; no implementation yet)
Scope: `specs/architecture/interfaces/allocation-dispatch.md` including all DEC-DISP-01..06 decisions, proto extensions, Raft command additions, Dispatcher trait, Runtime trait, Agent handler contract, AllocationManager extension, silent-sweep integration, DispatcherConfig, enforcement map, observability counters.

## Summary

| Severity | Count |
|---|---|
| Critical | 3 |
| High | 5 |
| Medium | 3 |
| Low | 0 |

Gate recommendation: **3 Critical findings must be resolved in architect or analyst phase before implementation begins.** Two of them (D-ADV-ARCH-01, D-ADV-ARCH-02) concern multi-node allocation semantics that the architect spec silently single-cases; the third is a thread-safety gap in a trait contract.

---

## Finding D-ADV-ARCH-01: Rollback semantics on multi-node allocations are undefined

**Severity:** Critical
**Category:** Correctness > Specification compliance, Implicit coupling
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § Raft command additions, `RollbackDispatch`
**Spec reference:** IP-14 talks about "an allocation" and "a node"; architect spec `RollbackDispatch` takes `{ allocation_id, node_id, observed_state_version, reason }` with singular node_id.

**Description:** Given a 4-node allocation where Nodes 1-3 successfully accept RunAllocation and Node 4's dispatch fails, what does `RollbackDispatch` do?
  - If it releases ONLY Node 4: the allocation's `assigned_nodes` becomes [N1, N2, N3] but INV-D6 says rollback also transitions state to Pending. A Pending allocation with non-empty `assigned_nodes` violates the ownership model.
  - If it releases ALL assigned nodes: Nodes 1-3 are running Workload Processes that are now orphaned from their allocation (which is now Pending). Who sends StopAllocation to them?

The architect spec does not distinguish. `node_id` is singular, implying per-node rollback, but no mechanism exists to reconcile the resulting partial state.

**Evidence:** This is not hypothetical. HPC allocations regularly span 4-64 nodes. Probability of partial dispatch failure grows linearly with node count. A single stuck agent at scale cascades into malformed allocation state.

**Suggested resolution:** The architect must pick:
  - (a) **All-or-nothing dispatch**: `RollbackDispatch` releases all assigned nodes and sends `StopAllocation` to agents that had already accepted. This is the clean semantics but introduces a cleanup RPC burst on partial-success cases.
  - (b) **Per-node retry**: keep the singular `node_id` but introduce per-node retry tracking. Only rollback the whole allocation when no assignable nodes remain. This is more forgiving but requires a new Allocation field (`dispatched_nodes: Vec<NodeId>` separate from `assigned_nodes`) and a more complex state machine.

My recommendation: (a). Simpler, matches how multi-node HPC jobs expect to be scheduled ("all or none").

Update INV-D6 statement to say "releases all assigned nodes" and add a new failure scenario to `allocation_dispatch.feature` covering cleanup of partial successes.

---

## Finding D-ADV-ARCH-02: Orphaned Workload Process after rollback race

**Severity:** Critical
**Category:** Correctness > Concurrency, Failure cascades
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § Dispatcher trait, RollbackDispatch apply step
**Spec reference:** `RollbackDispatch` releases node ownership. No mention of sending `StopAllocation` to the agent.

**Description:** Timeline:
  1. Dispatcher issues `RunAllocation(alloc_id)` to agent on Node A (attempt 3/3).
  2. Agent receives the RPC but the response packet is lost (network blip).
  3. Dispatcher times out, calls it failure, submits `RollbackDispatch(alloc_id, node_id=A)`.
  4. `RollbackDispatch` commits — state is now Pending, Node A's ownership released.
  5. Meanwhile, the agent (which DID receive the RPC successfully) has begun prologue. It spawns the Workload Process.
  6. First Completion Report (phase `Staging`, pid set) arrives on the next heartbeat. INV-D12 rejects it because Node A is no longer in `assigned_nodes` (released by rollback).
  7. Completion Report is dropped. Workload Process keeps running on Node A, consuming resources, with no link to any allocation.

No `StopAllocation` is sent by the Dispatcher on rollback. No orphan-sweep on the agent kills it (orphan cleanup only runs at boot, INV-D9). The Workload Process runs until the entrypoint exits naturally or walltime expires (if walltime is enforced at all — walltime enforcement is INV-E4 on `lattice-node-agent` which requires the allocation to be tracked).

**Evidence:** Any lossy network. Any in-flight agent crash that still manages to spawn. Any "attempt budget exhausted just as attempt succeeded" race. All plausible at cluster scale.

**Suggested resolution:** `RollbackDispatch` MUST include a `StopAllocation` dispatch to each agent whose `node_id` is being released. The stop is best-effort (may itself fail, but INV-D9 orphan cleanup will catch survivors on next agent boot).

Specifically:
  1. Add to the Dispatcher contract: after `RollbackDispatch` commits, fire-and-forget `StopAllocation(alloc_id)` to each released agent. Errors logged, not retried (the allocation is already rolled back; we're doing cleanup).
  2. Add a scenario to `allocation_dispatch.feature`: "Dispatcher sends StopAllocation to agents whose RunAllocation attempt was rolled back."
  3. Add a metric: `lattice_dispatch_rollback_stop_sent_total{result}`.

This resolves the orphan-before-next-boot gap without making rollback a blocking operation.

---

## Finding D-ADV-ARCH-03: AllocationManager.completion_buffer thread-safety is unspecified

**Severity:** Critical
**Category:** Correctness > Concurrency
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § Agent: RunAllocation handler, AllocationManager extension
**Spec reference:** "NEW: completion_buffer is a keyed map per INV-D13" — typed as `HashMap<AllocId, CompletionReport>`.

**Description:** The architect spec describes multiple concurrent producers and one consumer:
  - Producers: per-runtime monitor tasks (spawned by background workers for prologue/monitor/epilogue per `run_allocation`). With N active allocations there are N concurrent monitor tasks, each of which may call `push_report`.
  - Consumer: the heartbeat loop, calling `drain_reports`.

Plain `HashMap<K,V>` in Rust is not `Sync`. Multiple concurrent `push_report` calls would require either:
  - `Arc<Mutex<HashMap>>` — explicit lock.
  - `DashMap` or similar concurrent map.
  - Actor/channel pattern — a single owner, updates via channel.

The architect spec commits to none of these. An implementer could choose any, and each has distinct performance characteristics (lock contention on heartbeat drain; actor-hop latency; DashMap memory overhead). More importantly, the choice affects correctness — for example, if `drain_reports` and `push_report` both hold locks, they can deadlock against other AllocationManager operations.

**Evidence:** Standard Rust concurrency discipline: if the spec doesn't commit, implementers improvise inconsistently across reviews.

**Suggested resolution:** Architect chooses ONE pattern and writes it into the AllocationManager contract:

Recommended: **actor-owned** — AllocationManager is owned by a single task; all operations go through an `mpsc::Sender<AllocationManagerCmd>`. This matches the existing `AgentCommand` pattern (`cmd_rx` channel in agent main.rs, which is what we're fixing in the first place). Trivially correct, matches the codebase idiom, no lock contention.

If the actor pattern is unacceptable for latency reasons (unlikely at current scale), fall back to `Arc<Mutex<>>` with explicit contention metrics (`lattice_allocation_manager_lock_wait_seconds` histogram).

Update the interface spec to name the chosen pattern and update the trait signatures (if actor: methods take `&self` and return channel futures; if mutex: no change but callers must acquire lock).

---

## Finding D-ADV-ARCH-04: Leadership flap effectively doubles retry budget

**Severity:** High
**Category:** Correctness > Concurrency, Failure cascades
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § DEC-DISP-04 (single-leader Dispatcher)
**Spec reference:** "On leadership change, the new leader picks up where the old one left off via Raft-state replay"

**Description:** DEC-DISP-04 correctly notes that in-flight RPCs are absorbed by INV-D3 (agent-side idempotency). It does NOT address the retry-budget side effect:
  - L1 (old leader) has made attempts 1/3 and 2/3 on allocation X. Both fail-with-timeout.
  - L1 loses leadership before attempt 3/3. L2 becomes leader.
  - L2 reads GlobalState: allocation X is Running, assigned to Node A, no phase transition. L2's Dispatcher makes attempt 1/3.

L2 has no knowledge that L1 already made two attempts. The retry budget is effectively per-leader, not per-allocation. A cluster with frequent leadership flaps (network partitions during normal operation, or an unhealthy Raft cluster) burns through many more attempts than `max_dispatch_retries = 3`.

The retry counter that WOULD prevent this is `allocation.dispatch_retry_count`, but per DEC-DISP-02 that counter increments only on `RollbackDispatch` — i.e., only after the full attempt budget is exhausted. L1's in-flight attempts don't touch the counter.

**Evidence:** Raft leadership is expected to flap in real clusters: leader dies, election; leader network-isolated, election; rolling upgrade, election. A single allocation could see 10+ RPC attempts before reaching rollback under pathological conditions.

**Suggested resolution:** The architect must either:
  - (a) **Track attempts in Raft** — increment a separate `in_flight_attempts` counter on the allocation (or the agent) per attempt. Raft writes per RPC is expensive; architect previously rejected a similar pattern for node counter. But the Dispatcher attempts are much rarer than agent-to-quorum reports.
  - (b) **Accept the budget amplification** — document that under leadership flap, effective budget is up to `(leaders × max_dispatch_retries)`. Idempotency (INV-D3) ensures correctness; budget is soft. Update DEC-DISP-04 to acknowledge this explicitly.
  - (c) **On leadership change, wait one heartbeat before dispatching** — the new leader pauses the Dispatcher for `heartbeat_interval`, allowing any in-flight Completion Reports from the old leader's attempts to land. Reduces budget amplification to ~1 extra attempt per flap rather than a full budget reset. Cheap, imperfect.

Recommend (c) + (b) as documentation. (a) is over-engineering for the actual risk.

---

## Finding D-ADV-ARCH-05: `reattach_in_progress` flag location is ambiguous

**Severity:** High
**Category:** Correctness > Semantic drift
**Location:** Architect spec names the flag in two places:
  1. `specs/architecture/interfaces/allocation-dispatch.md` § Proto extensions: "`bool reattach_in_progress`" as a field in `HeartbeatRequest`.
  2. `specs/architecture/data-models/shared-kernel.md` § Node: "reattach_in_progress: bool // Flag from heartbeat; suppresses silent-sweep (INV-D8)".

**Description:** Is the flag:
  - (a) a heartbeat-only transient signal (never enters Raft state)?
  - (b) a persisted Node field updated on every heartbeat?

Case (a): scheduler reads the latest heartbeat via some in-memory cache. No `Command::RecordHeartbeat`-style Raft write for it. `available_nodes()` on a follower reads from Raft state which won't have the flag — INV-D2 violation candidate.

Case (b): needs a dedicated Raft command or heartbeat extension that writes it. Heartbeat writes to Raft are currently `RecordHeartbeat { id, timestamp, owner_version }` — no flag field. Must extend.

The architect spec's data-model update says "Flag from heartbeat" which suggests (a), but also lists it as a Node field which suggests (b).

**Evidence:** Implementer picks either in isolation. If (a), silent-sweep on a follower (scheduler cycle running on follower) misses the flag and triggers the bug DEC-DISP-03 was supposed to fix. If (b), every heartbeat writes to Raft, increasing commit pressure.

**Suggested resolution:** Commit to (b), but piggyback on the existing `RecordHeartbeat` command. Extend that command:
```
Command::RecordHeartbeat {
    id: NodeId,
    timestamp: DateTime<Utc>,
    owner_version: u64,
    reattach_in_progress: bool,  // NEW
}
```
Heartbeat writes are already Raft-committed per IP-03 (which notes "node ownership changes... ARE Raft-committed"). Adding one bool field is negligible.

Update the architect spec to unambiguously list `reattach_in_progress` as a Raft-committed Node field updated via `RecordHeartbeat`.

---

## Finding D-ADV-ARCH-06: Malicious agent can suppress silent-sweep via `reattach_in_progress`

**Severity:** High
**Category:** Security > Trust boundaries, Input validation
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § DEC-DISP-03 (reattach flag)
**Spec reference:** "If the flag remains after `reattach_grace_period` (default 5 min), silent-sweep resumes normal evaluation."

**Description:** A compromised agent can keep `reattach_in_progress: true` indefinitely. The architect spec says grace period bounds this at 5 minutes, but "what clears the flag" is unspecified. If the agent never sets it to false, the scheduler can only fall back to "ignore the flag after `reattach_grace_period`." During that 5-minute window, the malicious agent can:
  - Report false Completion Reports with impunity (some get rejected by INV-D12, but if the agent is on the allocation's assigned_nodes, they're accepted).
  - Absorb allocations and never execute them, with no silent-sweep rescue.

5 minutes × (number of compromised agents) × (throughput of spoofable allocations) can be large.

**Evidence:** Security-sensitive sites treat any 5-minute "trust the agent" window as a weakness. The grace period was designed for legitimate slow-reattach; it is also a weakness vector.

**Suggested resolution:** Tighten the flag lifecycle:
  1. `reattach_in_progress = true` may only be set in the FIRST heartbeat after an observable agent-restart signal (e.g., the quorum records agent restart via `RegisterNode` following `UpdateNodeAddress` or node state transition Ready → Ready-with-fresh-registration).
  2. After the first heartbeat sets the flag to true, subsequent heartbeats within the grace period may keep it true but CANNOT re-trigger the grace timer. The grace timer starts at the first `reattach_in_progress: true` heartbeat and expires absolutely after `reattach_grace_period` regardless of subsequent flag values.
  3. Set-to-false is a one-way transition within the lifetime of a registration. Once cleared, cannot be re-set without re-registration.

This limits the window to 5 minutes per restart, not per-whim.

Update INV-D5 and DEC-DISP-03 with the lifecycle rules.

---

## Finding D-ADV-ARCH-07: Silent-sweep races freshly-placed allocations

**Severity:** High
**Category:** Correctness > Edge cases, Concurrency
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § Scheduler: Silent-node reconciliation sweep
**Spec reference:** "sweep runs AFTER normal placement and AFTER drain completion."

**Description:** In a single scheduling cycle:
  1. Placement pass assigns Node A to allocation X. `allocation.state = Running`, `assigned_nodes = [A]`.
  2. Dispatcher (asynchronous to the scheduler cycle) will pick this up at next tick and call RunAllocation on Node A.
  3. Silent-sweep pass runs in the same cycle. It looks for allocations `Running` whose nodes are silent. Node A may not have heartbeated since allocation X was placed — or may have, but the most recent heartbeat is before `last_cycle_start`. Sweep could flag X as node-silent on Node A.

Sweep fires, submits `ApplyCompletionReport(phase=Failed, reason=node_silent)`. Allocation X transitions to Failed before Dispatcher even attempted dispatch.

**Evidence:** The timeline is plausible because placement and sweep are serial in one cycle but heartbeats are asynchronous at 10s cadence. If the cycle is 5s, sweep runs before Node A's next heartbeat.

**Suggested resolution:** Add precondition to silent-sweep: only consider allocations whose `assigned_at` is at least `heartbeat_interval + grace_period` in the past. Freshly-assigned allocations are exempt from sweep until they've had a reasonable chance to deliver their first heartbeat.

Update the silent-sweep pseudocode in the architect spec:
```
for each allocation in state Running:
    if now() - allocation.assigned_at < heartbeat_interval + grace_period:
        continue  // too fresh; skip
    if any assigned node has no heartbeat within grace and not reattach_in_progress:
        propose ApplyCompletionReport(phase=Failed, reason=node_silent)
```

Add a scenario to `allocation_dispatch.feature` covering fresh-placement immunity from sweep.

---

## Finding D-ADV-ARCH-08: Multi-node allocation phase aggregation is undefined

**Severity:** High
**Category:** Correctness > Specification compliance
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § Data model additions, `Local Allocation Phase → Allocation State` mapping.
**Spec reference:** INV-D7 and the ubiquitous language map: "`Prologue` → `Staging`, `Running`/`Epilogue` → `Running`, `Completed` → `Completed`, `Failed` → `Failed`." This is per-LocalAllocation.

**Description:** For a 4-node allocation, each node has its own LocalAllocation with its own phase. The GlobalState has one `allocation.state` for the whole allocation. What is the rule for aggregating 4 phases into one state?
  - Conservative: all 4 must report Running before allocation.state = Running. Any report Failed fails the whole allocation.
  - Optimistic: first Running report advances the allocation to Running; later-arriving reports only matter if they introduce a phase regression or failure.
  - Majority: whichever phase is most common wins.

The architect spec doesn't say. Different choices produce different user-visible behavior:
  - If conservative: multi-node allocation takes `max(prologue time across all nodes)` to reach Running. Slow straggler drags the whole allocation.
  - If optimistic: user sees `Running` when only some nodes are running. Misleading.
  - If majority: 2-node allocation splits 1/1 → tie.

**Evidence:** MPI jobs typically need all ranks running before they are "started." Slurm reports `RUNNING` once the first step begins. Users have different expectations.

**Suggested resolution:** Define the aggregation rule explicitly in INV-D7 or in a new section of the architect spec. My recommendation:
  - `Staging`: any LocalAllocation is `Prologue`.
  - `Running`: ALL LocalAllocations are `Running` or beyond.
  - `Completed`: ALL LocalAllocations are `Completed` with exit_code == 0.
  - `Failed`: ANY LocalAllocation is `Failed`, OR ALL are `Completed` with some non-zero exit.

This matches Slurm batch semantics (all-ranks-alive) and is the least surprising for MPI workloads.

Add a scenario: "Multi-node allocation remains Staging until all nodes report Running."

---

## Finding D-ADV-ARCH-09: ALREADY_RUNNING ghost case has long detection window

**Severity:** High
**Category:** Security > Trust boundaries
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § DEC-DISP-05 (refusal_reason)
**Spec reference:** "ALREADY_RUNNING — Treat as success; proceed with Completion Report expectation"

**Description:** An agent responds `accepted: true, refusal_reason: ALREADY_RUNNING` claiming it already has the allocation in its AllocationManager. If the agent is telling the truth (e.g., retry after a successful first dispatch), this is correct — INV-D3 path.

If the agent is lying (bug, compromise, or simply never-had-this-allocation-due-to-state-file-corruption), no Completion Report ever arrives. The Dispatcher moves on. The allocation sits in Running forever until silent-sweep catches it — but silent-sweep relies on heartbeat absence, and the agent IS heartbeating. Silent-sweep therefore does NOT catch this case.

What DOES catch it: INV-D8's precondition is "no heartbeat OR no Completion Report within grace window for allocations on this node." The current architect-spec silent-sweep checks heartbeat absence only.

**Evidence:** The silent-sweep pseudocode (Finding D-ADV-ARCH-07 suggested resolution included) checks heartbeat absence. Needs extension to also check "no Completion Report for allocation X within grace window even though node is heartbeating."

**Suggested resolution:** Extend the silent-sweep predicate:
```
for each allocation in state Running:
    if freshly-placed guard: continue
    for each assigned node N:
        node_silent = (now() - N.last_heartbeat > grace_period) AND !N.reattach_in_progress
        no_progress = (now() - allocation.last_completion_report_at > grace_window)
        if node_silent OR no_progress:
            propose ApplyCompletionReport(phase=Failed, reason=node_silent_or_no_progress)
```

Tracks `allocation.last_completion_report_at` as part of the allocation record (incremented whenever any `ApplyCompletionReport` commits for that allocation). Allows detection of ghosting agents without requiring the node to actually go silent.

Update the spec + add scenario: "Agent claims ALREADY_RUNNING but never reports; silent-sweep detects via missing Completion Report progress."

---

## Finding D-ADV-ARCH-10: Unknown `refusal_reason` has no defined handling

**Severity:** Medium
**Category:** Correctness > Missing negatives, Forward compatibility
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § DEC-DISP-05
**Spec reference:** Four enum variants listed with retry semantics.

**Description:** Protobuf enums are forward-compatible: new variants can be added. An old Dispatcher receiving a new `refusal_reason` from a newer agent sees an unknown variant. What does it do?
  - Treat as `BUSY`? Wastes retry budget on a reason the server told us was informative.
  - Treat as `MALFORMED_REQUEST`? Fails a legitimate allocation.
  - Treat as "accepted: false without reason"? Undefined per current spec.

Update path matters: agents may roll out ahead of lattice-api replicas.

**Suggested resolution:** Add a fifth default variant `REFUSAL_UNKNOWN = 255` OR explicitly specify that unknown `refusal_reason` values are treated as `MALFORMED_REQUEST` (safe-fail). Document in DEC-DISP-05.

---

## Finding D-ADV-ARCH-11: BareProcessRuntime env allow-list omits common HPC variables

**Severity:** Medium
**Category:** Correctness > Edge cases
**Location:** `specs/architecture/interfaces/allocation-dispatch.md` § BareProcessRuntime env scrubbing
**Spec reference:** "Variables explicitly allowed: PATH, HOME, USER, LANG, TZ, SLURM_*, LATTICE_ALLOC_ID, LATTICE_JOB_NAME, LATTICE_NODELIST, LATTICE_NNODES"

**Description:** Typical HPC workloads depend on many env vars not in this list. Missing any of these breaks user workloads silently (process starts but runs incorrectly):
  - `OMP_NUM_THREADS`, `OMP_PLACES`, `OMP_PROC_BIND` — OpenMP threading
  - `CUDA_VISIBLE_DEVICES`, `ROCR_VISIBLE_DEVICES` — GPU selection
  - `TMPDIR`, `XDG_RUNTIME_DIR` — temporary file paths
  - `LD_LIBRARY_PATH`, `LD_PRELOAD` — shared-library search (security-sensitive but essential)
  - `PMI_*`, `MPI_*`, `FI_*` (libfabric), `NCCL_*` — MPI/networking
  - `SHELL`, `PS1` — interactive contexts

Also: user-specified env vars via `AllocationSpec.env_vars` — the architect spec doesn't say how they merge with the allow-list. Are they passed through verbatim? Subject to the block-list?

**Evidence:** Slurm's `--export` defaults to NONE (clean env) but allows `--export=ALL` or named vars. Bare-Process in the current architecture spec is stricter than Slurm's most-restrictive mode, which will cause user friction.

**Suggested resolution:** Two changes:
  1. Extend the allow-list with the set above. Alternatively, make allow-list site-configurable (default as architect speced, operators extend).
  2. Explicitly state how `AllocationSpec.env_vars` merges: user-provided vars pass through, subject to the block-list (can't override secrets). Precedence: user > site allow-list > agent env.

Update BareProcessRuntime contract in architect spec. Add unit test scenarios.

---

## Gates

- **Before implementation:** findings D-ADV-ARCH-01, D-ADV-ARCH-02, D-ADV-ARCH-03 (Critical) MUST be resolved. All three concern multi-node or concurrency correctness.
- **Before implementation (strongly advised):** findings D-ADV-ARCH-04 through D-ADV-ARCH-09 (High) should be resolved or explicitly accepted as risks.
- **Before ship:** findings D-ADV-ARCH-10, D-ADV-ARCH-11 (Medium) addressed in specs and/or implementation.

My recommendation per diamond workflow: **back to architect for D-ADV-ARCH-01 (multi-node rollback), D-ADV-ARCH-02 (orphan workload), D-ADV-ARCH-03 (thread safety), D-ADV-ARCH-05 (reattach flag Raft-or-not), D-ADV-ARCH-08 (multi-node aggregation).** These five produce interface changes that the architect owns. The remaining findings (D-ADV-ARCH-04, D-ADV-ARCH-06, D-ADV-ARCH-07, D-ADV-ARCH-09, D-ADV-ARCH-10, D-ADV-ARCH-11) can be closed with text edits by analyst + architect together.

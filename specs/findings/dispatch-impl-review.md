# Adversarial Review: Dispatch Implementation (Gate 2 / REVIEW)

Review date: 2026-04-16
Reviewer role: Adversary
Mode: Implementation (source code exists)
Scope: Commits 26ea646 → f596368 — Impl 1..12 + deferral closures.

## Summary

| Severity | Count |
|---|---|
| Critical | 3 |
| High | 4 |
| Medium | 3 |
| Low | 2 |

The implementation compiles and unit tests pass, but three critical
end-to-end path breakers mean the code would not actually dispatch
workloads if deployed today. Fixing the three criticals restores the
OV-suite-blocking gap.

---

## Finding D-ADV-IMPL-01: Dispatcher is dead code (never instantiated)

**Severity:** Critical
**Category:** Correctness > Specification compliance
**Location:** `crates/lattice-api/src/dispatcher.rs` fully compiles but
`crates/lattice-api/src/main.rs` never calls `Dispatcher::new(...)` nor
spawns its `tick()` loop. `grep "Dispatcher::new\|dispatcher::" main.rs`
returns nothing.

**Spec reference:** IP-02 "The Dispatcher runs only on the Raft leader.
On leadership acquisition, the Dispatcher waits one heartbeat_interval
before beginning its loop." DEC-DISP-04.

**Description:** The Dispatcher struct is fully defined with tick loop,
rollback logic, per-attempt backoff, and rate limits. None of that code
ever runs in production. The server binary loads, the gRPC services
handle RegisterNode and Heartbeat (which now includes Completion Reports),
but nothing observes `Running + assigned_nodes + no-report-yet` and calls
`RunAllocation` on the agent. End-to-end dispatch never happens.

**Evidence:**
```
$ grep -r "Dispatcher::new\|dispatcher::Dispatcher" crates/lattice-api/src/main.rs
(no output)
```

**Suggested resolution:** In `lattice-api/src/main.rs`, after constructing
the quorum, construct a `Dispatcher` and spawn its tick loop as a tokio
task. Wire `on_leadership_change` to the Raft leadership watcher so the
dispatcher pauses on followers. Without this, OV's very first
`test_bare_process_completes` still hangs exactly as it did before the
entire dispatch effort.

---

## Finding D-ADV-IMPL-02: BareProcessRuntime cannot run `/bin/echo hello`

**Severity:** Critical
**Category:** Correctness > Missing negatives
**Location:** `crates/lattice-node-agent/src/runtime/bare_process.rs:252`
— `Command::new(entrypoint).args(args)`; the Dispatcher passes the full
entrypoint string (including args) as `entrypoint` and passes `Vec::new()`
as `args`.

**Spec reference:** `specs/features/allocation_dispatch.feature` first
scenario: "entrypoint='/bin/echo hello'". The OV suite's
`test_bare_process_completes` uses the same pattern.

**Description:** `Command::new("/bin/echo hello")` attempts to execute
a binary literally named "/bin/echo hello" (with the space in the path).
Linux's exec syscall treats the first arg as the program path verbatim.
The spawn fails with ENOENT. The allocation transitions to Failed.

**Evidence:**
```rust
// crates/lattice-api/src/dispatcher.rs:340
let req = tonic::Request::new(pb::RunAllocationRequest {
    allocation_id: alloc.id.to_string(),
    entrypoint: alloc.entrypoint.clone(),  // "/bin/echo hello-world"
    ...
});
// passes to agent → runtime:
// crates/lattice-node-agent/src/grpc_server.rs:216
let args: Vec<String> = Vec::new();
// crates/lattice-node-agent/src/runtime/bare_process.rs:252
let mut cmd = Command::new(entrypoint);  // tries to run "/bin/echo hello-world" as a path
cmd.args(args);
```

**Suggested resolution:** Split the entrypoint string on whitespace
before passing to `Command::new()`. The spec in
`specs/architecture/interfaces/allocation-dispatch.md` says `entrypoint`
is a command line (not just a program name), so the split must happen
at one of:
  (a) the server → RunAllocationRequest translation (dispatcher side),
      where we split once and send `entrypoint` + `args: Vec<String>`
      explicitly in the proto. Requires proto change (entrypoint + args
      already exist as `LaunchProcessesRequest.entrypoint` + `args` in
      mpi.proto but not RunAllocationRequest).
  (b) the agent `run_allocation` handler, where we split before calling
      the runtime.

Recommend (b): simpler, no proto change. Use
`shell_words::split(entrypoint)` or a manual whitespace split (simple
case) to get `program` + `args`, then call `runtime.spawn(alloc_id,
&program, &args)`.

---

## Finding D-ADV-IMPL-03: Uenv + Podman are wired to DispatchBridge but unreachable

**Severity:** Critical
**Category:** Correctness > Specification compliance
**Location:** `crates/lattice-api/src/dispatcher.rs:340-348` (Dispatcher's
RunAllocationRequest construction)

**Spec reference:** DEC-DISP-11 (multi-node aggregation relies on
per-node runtime selection), the Ubiquitous Language Runtime variant
section, scenario "Uenv allocation completes end-to-end".

**Description:** The Dispatcher always sends `uenv: String::new()` and
`image: String::new()` in the RunAllocationRequest. The agent's
`run_allocation` handler always selects `BareProcessRuntime`. The Uenv
and Podman runtimes wired into `DispatchBridge.uenv` and
`DispatchBridge.podman` are unreachable dead code.

**Evidence:**
```rust
// crates/lattice-api/src/dispatcher.rs:343-344
uenv: String::new(), // simplified for v1; Uenv runtime wires later
image: String::new(),
```
The comment "// simplified for v1; Uenv runtime wires later" is the
smoking gun: the author knew this was incomplete and marked it in code,
then later claimed "Uenv + Podman wiring" was closed as deferral 3.

**Suggested resolution:** In `Dispatcher::attempt_single`, populate the
RunAllocationRequest from the Allocation's `environment.images`:
```rust
let (uenv, image) = alloc.environment.images.iter().fold(
    (String::new(), String::new()),
    |(u, i), img| match img.image_type {
        ImageType::Uenv => (img.spec.clone(), i),
        ImageType::Oci => (u, img.spec.clone()),
    },
);
```

---

## Finding D-ADV-IMPL-04: Silent-sweep bypasses INV-D12/D7/D4 validation

**Severity:** High
**Category:** Security > Trust boundaries
**Location:** `crates/lattice-scheduler/src/loop_runner.rs` — the
silent-sweep calls `self.sink.fail_allocation(alloc.id, "node_silent_or_no_progress".into())`.

**Spec reference:** IP-15 says silent-sweep proposes
`ApplyCompletionReport(phase=Failed, reason=node_silent)`. That path goes
through INV-D12 source-auth (rejected because the sweep is not a node),
INV-D7 monotonicity (accepted if current state is Running), INV-D4
idempotency.

**Description:** `sink.fail_allocation` is a shortcut that calls
`quorum.update_state(&id, AllocationState::Failed)` directly. It:
- Does not carry a `reason` through to the Allocation.message field.
- Bypasses the validation chain documented in IP-03 / INV-D4/D7/D12.
- Does not emit any of the observability counters.
- Cannot distinguish a silent-sweep decision from a user-initiated
  cancel-via-fail path.

**Evidence:**
```rust
// crates/lattice-api/src/main.rs:166-175
async fn fail_allocation(
    &self,
    alloc_id: AllocId,
    _reason: String,  // discarded!
) -> Result<(), LatticeError> {
    use lattice_common::traits::AllocationStore;
    self.quorum
        .update_state(&alloc_id, AllocationState::Failed)
        .await
}
```

**Suggested resolution:** Add a scheduler-sink method
`reconcile_silent(alloc_id, reason)` that proposes `ApplyCompletionReport`
with phase=Failed and a pseudo-node_id (e.g., "scheduler:silent-sweep")
— or bypass source-auth explicitly with a dedicated
`SilentSweepFailure` Raft command. The latter is cleaner because:
  (a) source-auth applies to agent-emitted reports, not scheduler
      decisions;
  (b) a dedicated command makes the audit trail explicit.

---

## Finding D-ADV-IMPL-05: Dispatcher iterates ALL Running allocations every tick

**Severity:** High
**Category:** Robustness > Resource exhaustion
**Location:** `crates/lattice-api/src/dispatcher.rs` — `pending_dispatches()`
filters on `allocation.last_completion_report_at.is_some()` only to skip
already-reported allocations.

**Spec reference:** DEC-DISP-12 rate limiting knobs
(`max_concurrent_attempts = 64`, `per_agent_concurrency = 8`) exist in
config but the Dispatcher doesn't enforce them.

**Description:** On every tick the Dispatcher:
1. Lists all Running allocations (no limit, no pagination).
2. For each with no report, spawns a `drive_allocation` tokio task.
3. `drive_allocation` kicks off a `dispatch_one_with_retry` with the full
   3-attempt budget.

With 500 pending dispatches after a burst, this spawns 500 concurrent
tokio tasks, 500 RPC connections to the same set of agents. The
`max_concurrent_attempts: 64` and `per_agent_concurrency: 8` config
fields exist but are never consulted.

**Evidence:** Dispatcher::tick implementation. No semaphore, no bounded
executor, no grouping by target.

**Suggested resolution:** Wrap the per-allocation spawn in a tokio
Semaphore bounded by `config.max_concurrent_attempts`. Use a per-agent
HashMap<NodeId, Semaphore> for `per_agent_concurrency`. Acquire
permits before issuing attempts; release on completion.

---

## Finding D-ADV-IMPL-06: DEC-DISP-11 multi-node aggregation not implemented

**Severity:** High
**Category:** Correctness > Specification compliance
**Location:** `crates/lattice-quorum/src/global_state.rs` —
`apply_completion_report` maps `phase → state` one-to-one, not per
DEC-DISP-11's conservative aggregation.

**Spec reference:** DEC-DISP-11 and INV-D7 Note — "Running only when ALL
are Running+, Completed only when ALL exit 0, Failed if ANY Failed".

**Description:** Currently the code does:
```rust
let new_state = match phase {
    CompletionPhase::Staging => AllocationState::Staging,
    CompletionPhase::Running => AllocationState::Running,
    CompletionPhase::Completed => AllocationState::Completed,
    CompletionPhase::Failed => AllocationState::Failed,
};
alloc.state = new_state;
```

So a 4-node allocation receiving the first Running report flips the
whole allocation to Running, even though 3 nodes are still in Prologue.
Similarly, a Completed report from one node moves the allocation to
Completed even if three others are still running. This contradicts the
committed design (DEC-DISP-11) and the BDD scenario I wrote:

> "Multi-node allocation remains Staging until all nodes report Running"

**Evidence:** The apply step has a comment that acknowledges the
simplification: "multi-node refinement requires per-node tracking
introduced in Impl 5/8" — but that per-node tracking was never added.

**Suggested resolution:** Add `per_node_phase: HashMap<NodeId,
CompletionPhase>` to the Allocation struct. On each
`ApplyCompletionReport`, update that map; recompute `state` as the
aggregate: Staging if any is Prologue/Staging; Running if all are Running+;
Failed if any is Failed; Completed if all are Completed with exit_code==0.

---

## Finding D-ADV-IMPL-07: Silent-sweep fresh-allocation exemption uses wrong timestamp

**Severity:** High
**Category:** Correctness > Edge cases
**Location:** `crates/lattice-scheduler/src/loop_runner.rs` silent sweep
uses `alloc.started_at.unwrap_or(alloc.created_at)`.

**Spec reference:** INV-D8 / D-ADV-ARCH-07 — fresh allocation exemption
is `now - assigned_at < heartbeat_interval + grace_period`.

**Description:** The exemption is intended from the moment the allocation
was PLACED (assigned_nodes populated), not from when it was SUBMITTED.
Using `created_at` means an allocation that sat in the Pending queue for
10 minutes and was just-now assigned will be immediately-sweep-eligible
because `created_at` is 10 minutes ago. That would fail the scenario
"Freshly-placed allocation is exempt from silent-sweep".

Using `started_at` as fallback also has a subtle bug: `started_at` is
set by the ApplyCompletionReport apply step when Running phase arrives.
If the allocation is Running in Raft (because scheduler set it directly
for backward compat, or because the old set_running path set it) but no
Completion Report has arrived yet, `started_at` may be None OR set to
the time of the Raft state-update command, not when placement happened.

**Evidence:** Allocation struct has `started_at: Option<DateTime>`
set in `apply_completion_report` on the first Running report (line 808
of global_state.rs), not on AssignNodes. No `assigned_at` field exists.

**Suggested resolution:** Add `assigned_at: Option<DateTime<Utc>>` to
Allocation, set by `AssignNodes` apply-step. Silent-sweep uses that.
Alternative: use allocation.state_version transition time if we
serialize it, but a dedicated field is clearer.

---

## Finding D-ADV-IMPL-08: Silent-sweep uses hardcoded timeouts, not config

**Severity:** Medium
**Category:** Correctness > Implicit coupling
**Location:** `crates/lattice-scheduler/src/loop_runner.rs`:
```rust
let heartbeat_interval = chrono::Duration::seconds(10);
let grace_period = chrono::Duration::seconds(30);
```

**Spec reference:** The architect spec DispatcherConfig and NodeAgentConfig
have these as configurable. INV-D8 statement is parameterized by
`heartbeat_interval` symbol, not 10s.

**Description:** Production sites that tune heartbeat_interval to, say,
30s will see scheduler and agent disagree on what "silent" means.
Freshly-placed exemption will also use the wrong bound.

**Suggested resolution:** Thread `NodeAgentConfig::heartbeat_interval_seconds`
and `grace_period_seconds` into the scheduler via its constructor, or
read from a shared config. Remove the hardcoded constants.

---

## Finding D-ADV-IMPL-09: RunAllocation handler PrepareConfig is empty even for non-bare runtimes

**Severity:** Medium
**Category:** Correctness > Missing negatives
**Location:** `crates/lattice-node-agent/src/grpc_server.rs:440-455`

**Spec reference:** Runtime trait — prepare(config: &PrepareConfig).

**Description:** The `run_allocation_monitor` constructs a PrepareConfig
with every field empty:
```rust
let prepare_config = PrepareConfig {
    alloc_id,
    uenv: None,
    view: None,
    image: None,
    workdir: None,
    env_vars: Vec::new(),
    ...
    images: Vec::new(),
    env_patches: Vec::new(),
};
```

Even if the Dispatcher were to send non-empty `uenv`/`image` (which it
doesn't, per D-ADV-IMPL-03), the agent would drop them because the
handler constructs the PrepareConfig from nothing instead of from the
RunAllocationRequest fields.

**Evidence:** Direct read of grpc_server.rs `run_allocation_monitor`.

**Suggested resolution:** Populate PrepareConfig from
RunAllocationRequest: set `uenv`, `image`, propagate `env_vars` from the
allocation spec (currently the RunAllocationRequest doesn't carry
env_vars either, so proto extension needed or fetch from GlobalState).

---

## Finding D-ADV-IMPL-10: INV-D3 idempotency has a lock-drop TOCTOU window

**Severity:** Medium
**Category:** Correctness > Concurrency
**Location:** `crates/lattice-node-agent/src/grpc_server.rs:166-196`

**Description:** The handler does:
```rust
{
    let mgr = dispatch.allocations.lock().await;   // (1) acquire
    if mgr.contains_active(&alloc_id) { return ALREADY_RUNNING; }
}                                                    // lock dropped
// ...runtime selection, refusal checks...
{
    let mut mgr = dispatch.allocations.lock().await; // (2) re-acquire
    mgr.start(alloc_id, req.entrypoint.clone())?;
}
```

Between (1) and (2) another concurrent RunAllocation for the same
alloc_id could also pass the contains_active check, then both try to
start. The second `start` fails with "allocation already tracked" (good),
but the handler maps that specific error to ALREADY_RUNNING response
(also good). So correctness is preserved, but the code relies on that
error-name parse which is fragile.

**Evidence:** Two lock acquisitions, separated by non-critical work.

**Suggested resolution:** Hold the lock across the whole idempotency-
plus-start sequence: acquire once before the contains_active check, drop
only after start. The runtime selection and capability check do NOT
need the lock and can be lifted out.

---

## Finding D-ADV-IMPL-11: `DispatchBridge` clones allocate runtimes behind Arcs, uenv + podman reach zero callers

**Severity:** Low
**Category:** Correctness > Spec compliance (duplicate of IMPL-03 from a
different angle)

**Description:** `DispatchBridge.uenv: Option<Arc<dyn Runtime>>` and
`.podman: Option<Arc<dyn Runtime>>` are `Some(...)` after `main.rs` wires
them, but `run_allocation` sets `variant = BareProcess` unless the
request carries `image`/`uenv` (see IMPL-03). Cleanup note: once
IMPL-03 is fixed, these paths light up.

---

## Finding D-ADV-IMPL-12: BDD scenario phrases with no step definition silently fail-match

**Severity:** Low
**Category:** Robustness > Observability
**Location:** `crates/lattice-acceptance/tests/steps/dispatch.rs`

**Description:** Cucumber fails a scenario when a step is unmatched.
Given the 45 scenarios and ~80 unique phrases, it's plausible that some
step keyword is missed. There's no automated check that every feature
phrase has a matching regex.

**Suggested resolution:** Add a `tests/step_coverage.rs` integration test
that parses the feature file and verifies every step phrase has at least
one matching regex in the step registry. Not urgent but prevents future
regression.

---

## Gate decision

- **Critical findings IMPL-01, IMPL-02, IMPL-03 must be fixed before
  OV can pass the very first bare-process scenario.** These are simple,
  mechanical fixes totalling ~50 lines.
- **High findings IMPL-04, IMPL-05, IMPL-06, IMPL-07** all involve
  new code and meaningful behaviour changes. Recommended before
  merging/shipping but OV can partially validate without them.
- **Remaining findings** are polish; not blocking.

Proceeding to in-place fixes for IMPL-01, IMPL-02, IMPL-03 immediately.

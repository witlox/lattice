# Adversarial Review: Dispatch Analyst Output

Review date: 2026-04-16
Reviewer role: Adversary
Mode: Architecture (specs only, no implementation yet)
Scope: Layers 1–6 of the allocation-dispatch analyst work
  (`specs/domain-model.md` + `specs/ubiquitous-language.md` dispatch section,
  `specs/invariants.md` INV-D1..D11,
  `specs/features/allocation_dispatch.feature`,
  `specs/cross-context/interactions.md` IP-02/03/13/14/15,
  `specs/failure-modes.md` FM-D1..D10,
  `specs/assumptions.md` A-D2..D21)

## Summary

| Severity | Count |
|---|---|
| Critical | 2 |
| High | 5 |
| Medium | 5 |
| Low | 2 |

Three findings are gates that must be resolved in analyst or architect phase before implementation. The rest are concrete design work the architect must address, but none require re-opening the domain model.

---

## Finding D-ADV-01: Completion Report source-authentication is missing

**Severity:** Critical
**Category:** Security > Authentication & authorization, Trust boundaries
**Location:** `specs/invariants.md` INV-D4 (idempotency), INV-D7 (monotonicity); `specs/cross-context/interactions.md` IP-03
**Spec reference:** IP-03 says Completion Reports are Raft-committed after idempotency + monotonicity check. No check that the reporting node is actually one of the allocation's `assigned_nodes`.

**Description:** A heartbeat arrives from node B carrying a Completion Report for allocation X, which is actually assigned to node A. The quorum currently applies the state transition because INV-D4 (key by allocation_id, phase) and INV-D7 (phase monotonicity) are both satisfied. There is no invariant requiring that the reporting node be in the allocation's `assigned_nodes` list.

**Evidence:** In a cluster with a compromised agent on any node, that agent can:
- Mark any Running allocation as Failed (denial of service on another tenant's workload)
- Mark its own failed workload as Completed (fraudulent accounting, evades requeue)
- Mark allocations as Running that it is not actually executing (ghosting)

mTLS (INV-A6) authenticates the node's identity but does not prevent that identity from reporting on allocations assigned elsewhere.

**Suggested resolution:** Add INV-D12: "A Completion Report for allocation X delivered on a heartbeat from node N is applied only if N ∈ X.assigned_nodes at the current state_version." Rejection is logged with a `cross_node_report` anomaly counter.

---

## Finding D-ADV-02: Final-state report preservation lacks a mechanism

**Severity:** Critical
**Category:** Correctness > Specification compliance
**Location:** `specs/failure-modes.md` FM-D8; `specs/assumptions.md` A-D19
**Spec reference:** FM-D8 asserts "final-state reports are always kept" as a correctness guarantee; A-D19 concedes no mechanism exists.

**Description:** FM-D8's degradation story is that intermediate reports drop but terminal ones (`Completed`, `Failed`) are preserved. Without a preservation mechanism this is aspirational. If the buffer fills with intermediate `Running` reports from a chatty set of allocations and a terminal `Failed` arrives next, FIFO eviction could drop the terminal report in favor of older intermediates.

**Evidence:** Imagine 300 short-lived allocations completing in one heartbeat window. Default buffer 256. FIFO eviction under FM-D8's drop-oldest policy drops the first 44 reports, which are likely `Staging` and `Running` reports for the earliest-completing allocations — AND their `Completed` reports, because terminal status is not intrinsically "newer" under a time-ordered eviction policy.

**Suggested resolution:** Architect must choose ONE of:
  - (a) Priority queue: terminal-state reports never evicted while any intermediate report is evictable.
  - (b) Two-tier buffer: intermediate reports go in bounded FIFO; terminal reports go in a separate unbounded-until-flush list.
  - (c) Prove by construction that terminal reports are always newer than intermediates in the same buffer (requires that Completion Reports for an allocation monotonically advance and that intermediate reports are removed on transition). This is the leanest option.

Add to `specs/invariants.md` as INV-D13 with the chosen mechanism. Without it FM-D8 is an unprovable claim.

---

## Finding D-ADV-03: Cluster-wide image corruption causes cluster-wide deadlock

**Severity:** High
**Category:** Robustness > Failure cascades
**Location:** `specs/invariants.md` INV-D11
**Spec reference:** INV-D11 transitions nodes to Degraded after `max_node_dispatch_failures` consecutive dispatch failures.

**Description:** Suppose a bad image digest (or bad uenv manifest) is pushed to the shared registry and every agent's pull fails with the same error during Dispatch. Every allocation placement triggers a Dispatch Failure on every node. Every node accumulates `consecutive_dispatch_failures ≥ max_node_dispatch_failures` within minutes. `available_nodes()` returns empty. All pending allocations are stuck. All new allocations are also stuck. The fault is cluster-wide and transient (fixable by fixing the registry) but the self-protection converts it into a cluster-wide deadlock with no automatic recovery path.

**Evidence:** Scenario: operator force-pushes `uenv/prgenv-gnu:v1` to the registry with a bad manifest. Dispatcher observes 50 pending allocations, places each on a different node, each Dispatch fails, every node reaches the `max_node_dispatch_failures` threshold. Cluster is now dark for scheduling. Only manual operator intervention (clearing the Degraded state on every node) recovers.

**Suggested resolution:** Add to INV-D11 an exponential-backoff-style reset mechanism: once a node has been Degraded for `degraded_reset_interval` (default 5 min), its `consecutive_dispatch_failures` is halved and state probes back to Ready for a trial dispatch. This breaks the all-node-simultaneously-stuck case without requiring cluster-wide operator action. Alternative: a global "dispatch failure ratio" guard — if N% of nodes are Degraded simultaneously, suspend the INV-D11 auto-transition because the cause is likely shared, not per-node.

---

## Finding D-ADV-04: Retry counter location is undefined; leak path exists

**Severity:** High
**Category:** Correctness > Implicit coupling, Concurrency
**Location:** `specs/assumptions.md` A-D16 (in-memory vs Raft-committed is "architect must resolve")
**Spec reference:** IP-14 uses `allocation.dispatch_retry_count` without saying where it is stored.

**Description:** If `dispatch_retry_count` is held in-memory on the Dispatcher process, a Dispatcher crash during a rollback attempt resets the counter to 0. The allocation can then be retried indefinitely across crashes, never reaching the `max_dispatch_retries` cap. If held in Raft state on the Allocation entity, it survives crashes but requires Raft writes on every rollback. The spec claims the cap-to-Failed behavior works without saying where the counter lives.

**Evidence:** Typical production scenario: the Dispatcher encounters a genuinely broken allocation (bad image digest that no node can handle). With in-memory counters and periodic Dispatcher restarts (during lattice-api rolling upgrades), the allocation keeps getting retried every time the Dispatcher restarts, never reaching `max_dispatch_retries`. User's allocation appears healthy-ish forever.

**Suggested resolution:** Architect must commit the counter to Raft. Add to assumption A-D16 that `dispatch_retry_count` specifically MUST be persisted. Only `node.consecutive_dispatch_failures` has the option of being derived-from-Raft-log-scan instead of a field. Update INV-D6 statement to require the counter be part of the atomic rollback proposal payload.

---

## Finding D-ADV-05: INV-D5 joint invariant permits a reachable contradiction

**Severity:** High
**Category:** Correctness > Concurrency, temporal coupling
**Location:** `specs/invariants.md` INV-D5; `specs/failure-modes.md` FM-D3 vs INV-D8
**Spec reference:** INV-D5 says Running ⇒ (live process) OR (reattach in progress) OR (buffered Completion Report). INV-D8 says silent-node sweep declares Failed after `heartbeat_interval + grace_period`.

**Description:** The spec does not bound how long "reattach in progress" may last. A slow reattach (large state file, slow disk, kernel pause) could legitimately take longer than `heartbeat_interval + grace_period`. During that window:
  - INV-D5 (b) is satisfied: reattach is in progress.
  - INV-D8 fires: silent-sweep declares the allocation Failed (no heartbeat delivered in the window).

Then reattach completes, the Workload Process is alive, the first heartbeat arrives with `Running`. The allocation is now:
  - `Failed` in GlobalState (committed by silent-sweep).
  - INV-D5 (a) is satisfied (live process).
  - Next Completion Report violates INV-D7 (phase regression: arriving `Running` after state is `Failed`).

**Evidence:** Happens deterministically whenever reattach exceeds the silent-sweep window. Not hypothetical — agent reboot on a node with thousands of cgroup scopes to scan can easily exceed 40s.

**Suggested resolution:** Two options — architect picks.
  - (a) Make reattach announce itself: the agent emits a heartbeat with a `reattach_in_progress: true` flag on startup, within `heartbeat_interval/2`. Silent-sweep suppresses its trigger for allocations on nodes with a fresh `reattach_in_progress` flag. Add reattach_grace_period (default: 5 min) as a new timeout.
  - (b) Agent startup is synchronous and must complete reattach before first heartbeat. If reattach takes longer than `heartbeat_interval + grace_period`, the agent has genuinely failed; silent-sweep is correct to Fail the allocations. This is simpler but loses the "workload survives slow reattach" property.

Revise INV-D5 explicitly and update FM-D3 accordingly.

---

## Finding D-ADV-06: Two-instance Dispatcher multiplies retry budget

**Severity:** High
**Category:** Correctness > Concurrency
**Location:** `specs/assumptions.md` A-D20; `specs/failure-modes.md` FM-D10
**Spec reference:** A-D20 claims no leader election needed; FM-D10 claims Dispatcher is stateless-over-Raft.

**Description:** If `lattice-api` is deployed with multiple replicas (HA), each has its own Dispatcher. Both observe the same Running-but-un-acked allocation. Both start retrying. INV-D3 absorbs duplicate RunAllocation on the agent side, but the retry budget is effectively 2× per Dispatcher instance. With the Raft-backed retry counter (from finding D-ADV-04), only one succeeds in incrementing and rolling back; the other's attempts are redundant. With in-memory counters, each instance independently reaches 3, submits conflicting rollback proposals — one wins via version check, the other's 3 attempts were pure waste.

**Evidence:** 2-replica lattice-api, broken agent, 3-attempt budget. Expected network traffic: 3 attempts. Actual traffic: 6. Budget effective: 3 × replicas.

**Suggested resolution:** Architect must either (a) elect a single Dispatcher leader (simpler path: only the Raft leader runs the Dispatcher; followers skip), or (b) coordinate via a Raft-backed dispatch_in_flight marker per allocation so only one instance owns each dispatch. Option (a) is simpler and matches how most single-leader schedulers are implemented. Update A-D20 to commit to one approach.

---

## Finding D-ADV-07: `accepted: false` RunAllocation response has no defined semantics

**Severity:** High
**Category:** Correctness > Missing negatives
**Location:** `proto/lattice/v1/agent.proto:57-60` RunAllocationResponse has `accepted: bool`; no spec describes what to do when `accepted: false`.

**Description:** The agent's RunAllocation handler may legitimately refuse to accept a dispatch: incompatible capabilities, missing prerequisites, wrong node mode, already at max concurrency. The RPC returns `accepted: false` with a message. The Dispatcher spec in IP-02 does not say whether this counts toward the retry budget, triggers immediate rollback, or is silently treated as a retryable timeout.

**Evidence:** If treated as success (bug), allocation appears Running forever because no Completion Report arrives. If treated as retryable, we burn the budget on a deterministic refusal. If treated as terminal-fail, a transient refusal (e.g., agent busy) prevents legitimate dispatch.

**Suggested resolution:** Add scenario to `allocation_dispatch.feature`: "Agent returns accepted: false with reason: [terminal|retryable]". Define the reason taxonomy (e.g., `busy` = retry, `unsupported_capability` = immediate rollback to Pending with placement hint, `malformed_request` = immediate Fail). Update IP-02 contract accordingly.

---

## Finding D-ADV-08: Agent can self-claim arbitrary `agent_address`

**Severity:** High
**Category:** Security > Trust boundaries, Input validation
**Location:** `specs/cross-context/interactions.md` IP-13
**Spec reference:** IP-13 "Agent calls RegisterNode with address 'X'" — the agent writes its own address into the Node record.

**Description:** mTLS (INV-A6) authenticates the agent's identity (via workload certificate) but does not bind that identity to a specific network address. A compromised agent (or a misconfigured one with a bad `--grpc-addr` flag) can write any reachable address — including one pointing to a different host that the attacker controls. Subsequent Dispatch RPCs are directed to the attacker's host, where the attacker serves fake `accepted: true` responses and fake Completion Reports (though finding D-ADV-01 still applies to the Completion Reports).

**Evidence:** Attacker compromises a single agent's identity (e.g., stolen workload cert). Attacker starts a fake agent on their own host. Fake agent calls RegisterNode with the stolen node_id and the attacker's address. Dispatch traffic for that node is now redirected. Attacker can ghost allocations, collect billable usage, or serve malicious workloads.

**Suggested resolution:** Architect must choose ONE of:
  - (a) Bind the address to the agent's cert: the workload cert's SAN (subject alternative name) must match the registered `agent_address`. Quorum validates at RegisterNode time.
  - (b) Operator-side address allocation: agent_address is set by OpenCHAMI/inventory, not by the agent, and shipped to the agent at boot. Agent cannot self-claim.

Option (a) is more flexible; option (b) is more restrictive but closer to how HPC sites typically manage node inventories. Either requires an invariant addition — call it INV-D14: "agent_address is bound to the agent's authenticated identity."

---

## Finding D-ADV-09: Idempotency check at submit-time vs apply-time

**Severity:** Medium
**Category:** Correctness > Concurrency
**Location:** `specs/invariants.md` INV-D4
**Spec reference:** INV-D4 describes check-and-apply but doesn't specify when the check happens.

**Description:** If the quorum's "has this transition been applied?" check happens at proposal-submit time, two concurrent duplicate Completion Reports both pass the check and both submit proposals. Raft serializes them, and only one applies (the other is a no-op by virtue of finding the state already transitioned). But this only works because Raft serializes — if any implementation choice puts the idempotency check outside the Raft apply step, duplicate side effects are possible.

**Evidence:** Typical bug pattern: "we checked, then submitted." If the check is a read from a follower and the submit goes through the leader, the check may race.

**Suggested resolution:** INV-D4 must specify: "The idempotency check is part of the command's apply step, not a pre-submit read." Adds one sentence to the invariant; blocks a class of implementation errors.

---

## Finding D-ADV-10: Dispatcher vs Reconciler race has undefined winner

**Severity:** Medium
**Category:** Correctness > Concurrency
**Location:** `specs/cross-context/interactions.md` IP-02 (dispatch retry) vs IP-15 (silent-node reconciliation)
**Spec reference:** Neither IP specifies priority when both observe the same allocation.

**Description:** Both components look at "Running allocation, assigned_nodes nonempty, no phase transition received." Both can act: Dispatcher attempts another RunAllocation; Reconciler declares `node_silent` → Failed or Pending. If the Dispatcher's attempt is in flight when the Reconciler submits its proposal, the outcome depends on which Raft proposal lands first. The allocation may end up Pending (from Dispatcher rollback) or Failed (from Reconciler). Different outcomes for the same underlying condition.

**Evidence:** Agent is dead but Dispatcher doesn't know yet; Dispatcher submits attempt 2 (which will fail); Reconciler's silent-sweep fires concurrently. Dispatcher's attempt completes, submits rollback (version 5). Reconciler submits Failed transition (also version 5). Raft applies one; the other fails version check. Winner depends on network timing.

**Suggested resolution:** Define a priority: "Reconciler does not fire while a Dispatcher attempt is in flight for the same allocation." Implementation: a flag `dispatch_in_flight: bool` on the allocation, cleared when the attempt resolves. Reconciler skips allocations with this flag set. Adds IP-15 precondition: "allocation has no in-flight dispatch attempt."

---

## Finding D-ADV-11: Phase-regression anomaly is a log line, not a metric

**Severity:** Medium
**Category:** Robustness > Observability gaps
**Location:** `specs/invariants.md` INV-D7
**Spec reference:** "rejection produces an anomaly log line with (node_id, allocation_id, current_state, reported_phase)"

**Description:** Operators in production don't grep logs — they watch metrics and alerts. An unstructured log line is invisible to monitoring. Phase regression is a correctness-adjacent anomaly that could signal (a) agent bug, (b) replay attack, (c) race we missed. All three deserve alerts.

**Evidence:** Every production system hits "we had the log but missed the problem" at some point. INV-D7's current enforcement is precisely that pattern.

**Suggested resolution:** Require a Prometheus counter `lattice_completion_report_phase_regression_total` with labels `(node_id, allocation_id_hash, current_phase, reported_phase)`. Log line in addition, not instead. Update INV-D7 enforcement section.

---

## Finding D-ADV-12: Dispatch traffic rate has no limit

**Severity:** Medium
**Category:** Robustness > Resource exhaustion
**Location:** `specs/cross-context/interactions.md` IP-02
**Spec reference:** IP-02 does not specify a rate limit or batching policy.

**Description:** The Dispatcher observes all un-acked Running allocations on every iteration. In a backlog burst (e.g., 500 allocations drop into the cluster when a checkpoint is released), the Dispatcher attempts 500 RunAllocation RPCs in one pass. Each agent may receive tens of attempts. This is an implementation concern that the spec should at least gesture at.

**Evidence:** Not hypothetical — in practice, clusters get 1000-job arrays submitted at 09:00 Monday. If the Dispatcher fans out concurrently with no cap, agents see thundering-herd RPCs at exactly the moment they are also under scheduling-decision load.

**Suggested resolution:** IP-02 should specify either (a) a per-agent concurrency cap (default 8 simultaneous dispatches per agent), (b) a total concurrency cap on the Dispatcher (default 64), or both. Leave exact numbers to architect. Add to A-D9 as a policy knob.

---

## Finding D-ADV-13: Bare-Process runtime inherits agent secrets by design

**Severity:** Low
**Category:** Security > Secrets & configuration
**Location:** `specs/assumptions.md` A-D10
**Spec reference:** "A Bare-Process Runtime spawns the entrypoint with the lattice-agent process's environment."

**Description:** The lattice-agent process environment may contain `LATTICE_AGENT_TOKEN`, `VAULT_SECRET_ID`, or other sensitive variables. A Bare-Process workload inherits them by default. A compromised or hostile workload can read them and exfiltrate.

**Evidence:** Any process spawned with environment inheritance gets the parent's env by default. Slurm has exactly this issue and deals with it by allow-lists / block-lists.

**Suggested resolution:** A-D10 should explicitly enumerate a block-list of variables stripped from the Bare-Process environment (at minimum `LATTICE_*`, `VAULT_*`, anything matching `*_SECRET*`, `*_TOKEN*`, `*_KEY*`). Document in `docs/architecture/security.md`. Not critical — but should be designed in, not retrofitted.

---

## Finding D-ADV-14: Orphan cleanup trusts cgroup presence as "scope has a live process"

**Severity:** Low
**Category:** Correctness > Edge cases
**Location:** `specs/invariants.md` INV-D9; `specs/failure-modes.md` FM-D5
**Spec reference:** "terminate and remove unknown scopes"

**Description:** INV-D9 scans `workload.slice/` and cleans up scopes whose allocation_id is not in AgentState. "Clean up" includes process termination. But a scope may exist without any live process (process exited, scope leaked). Sending SIGTERM to an empty cgroup is a no-op, but the spec conflates "terminate + remove" as if a live process is assumed. Minor — worth tightening.

**Suggested resolution:** Rewrite the enforcement: "enumerate scopes under workload.slice/; for each not in AgentState, (a) read the scope's PID list, (b) if nonempty, terminate; (c) remove the scope." Makes the no-process case explicit.

---

## Gates for next phase

- **Before architect phase:** findings D-ADV-01, D-ADV-02 (both Critical) must be addressed in the spec. They are spec-level gaps that will re-surface as design holes otherwise.
- **During architect phase:** findings D-ADV-03 through D-ADV-08 (all High) are architect decisions. Each has a suggested resolution; architect picks one and updates specs accordingly.
- **Before implementation:** findings D-ADV-09 through D-ADV-14 (Medium/Low) must be reflected in design artifacts but don't gate architect handoff.

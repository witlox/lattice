# Preemption Policy

## Design Principle

Preemption is a last resort for resource rebalancing. The scheduler prefers waiting, backfill, and elastic borrowing over preemption. When preemption is necessary, it targets allocations with the lowest preemption cost (fast checkpoint, low priority, short remaining runtime). Medical allocations are never preempted.

## Preemption Classes

Each allocation has a `preemption_class` (0-10):

| Class | Meaning | Typical Use | Preemptible By |
|-------|---------|-------------|----------------|
| 0 | Best-effort | Scavenger jobs, testing | Any higher class |
| 1-3 | Low priority | Batch exploration, sweeps | Class 4+ |
| 4-6 | Normal | Production training, simulation | Class 7+ |
| 7-9 | High priority | Time-sensitive production | Class 10 only |
| 10 | Critical / Medical | Medical claims, emergency | Never preempted |

**Rule:** Preemption only moves down — a class-5 allocation can preempt class 0-4 allocations but never class 5+.

**Enforcement:** The `preemption_class` range (0-10) is validated at API admission. Values outside this range are rejected with a `400 Bad Request` error before reaching the scheduler.

**Tie-breaking within class:** If multiple allocations have the same preemption class, the scheduler prefers to preempt the one with the lowest checkpoint cost (f₈).

## Preemption Triggers

### 1. Higher-Priority Demand

A pending allocation with class N cannot be scheduled because all suitable nodes are occupied by lower-class allocations. The scheduler evaluates whether preempting one or more lower-class allocations would free enough resources.

### 2. Elastic Reclamation

A vCluster's idle nodes were borrowed by another vCluster (elastic sharing). The home vCluster now needs them back. Borrowed nodes carry an implicit preemption risk — the checkpoint cost model (f₈) accounts for this.

### 3. Medical Node Claim

A medical user claims nodes that are currently occupied by non-medical allocations. Medical claims are class 10 (highest). The scheduler triggers immediate checkpoint + preemption of the occupying allocations.

### 4. Quota Enforcement

A tenant exceeds their hard quota due to a race condition (two concurrent proposals, first committed). The quorum rejects the second proposal — this is not preemption but rejection. Running allocations are never preempted for quota enforcement.

## Preemption Decision Algorithm

```
PreemptionDecision(pending_job, candidates):

1. Filter candidates:
   - Only allocations with preemption_class < pending_job.preemption_class
   - Exclude medical allocations (never preempted)
   - Exclude allocations in Checkpointing state (already being preempted)

2. Score each candidate by preemption cost:
   preemption_cost(c) = checkpoint_time(c)
                       + recompute_if_no_checkpoint(c)
                       + remaining_walltime_value(c)

   checkpoint_time(c):
     If checkpoint == Auto: estimated_checkpoint_minutes from f₈
     If checkpoint == Manual: assume application handles it, use configured timeout
     If checkpoint == None: recompute_if_no_checkpoint applies

   recompute_if_no_checkpoint(c):
     time_since_last_checkpoint(c) × node_count(c) × gpu_per_node
     (GPU-hours that would be lost)

   remaining_walltime_value(c):
     If c is near completion (>90% walltime used): high cost (let it finish)
     If c just started (<10% walltime used): low cost (little invested)

3. Select victim set:
   Greedy: pick candidates with lowest preemption_cost until enough nodes freed.
   Constraint: freed nodes must satisfy pending_job's topology/conformance requirements.

4. If no valid victim set exists: pending_job stays queued (preemption not possible).

5. If valid victim set found: initiate preemption sequence.
```

## Preemption Sequence

```
1. Scheduler identifies victim allocations
2. For each victim:
   a. If checkpoint == Auto or Manual:
      - Checkpoint broker sends CHECKPOINT_HINT to node agents
      - Application checkpoints (signal, shmem, or gRPC callback)
      - Timeout: checkpoint_timeout (default: 10 minutes)
   b. If checkpoint == None:
      - SIGTERM sent immediately
      - Grace period (30s) → SIGKILL
3. When checkpoint completes (or timeout):
   - Allocation transitions to Suspended state
   - Nodes released to quorum (Raft commit)
4. Freed nodes assigned to pending allocation
5. Suspended allocations re-enter queue with:
   - Original submission time preserved (no wait-time penalty)
   - Resume-from-checkpoint flag set
   - Preempted-count incremented
```

## Checkpoint Timeout Handling

When a checkpointing allocation fails to complete within the timeout:

| Scenario | Action |
|----------|--------|
| Application responds but slow | Extend timeout by 50%, once |
| Application unresponsive | SIGTERM → grace period → SIGKILL. Mark as failed (not suspended). Requeue if policy allows. |
| gRPC callback: application requests deferral | Grant deferral up to `max_deferral` (default: 5 minutes). Then force. |

## Multi-Victim Preemption

Sometimes freeing one allocation isn't enough. The scheduler can preempt multiple allocations in a single decision:

**Constraints:**
- Maximum victims per decision: configurable (default: 3)
- All victims must have lower preemption class than the pending job
- Total preemption cost must be less than the pending job's estimated value
- Scheduler prefers preempting fewer, larger allocations over many small ones

**Ordering:** Victims are preempted in parallel (all receive checkpoint hints simultaneously). The pending job starts once all victims have released their nodes.

## Per-vCluster Preemption Policy

| vCluster Type | Preemption Allowed | Notes |
|---------------|-------------------|-------|
| HPC Batch | Yes | Class-based, checkpoint-aware |
| ML Training | Yes | Checkpoint cost heavily weighted (w₈=0.15) |
| Service | Yes (borrowed nodes only) | Services on home nodes are not preempted; borrowed nodes reclaimable |
| Medical | Never preempted | Class 10, no exceptions |
| Interactive | Yes | Short-lived, low cost to preempt |

## Non-Preemptible Allocations

An allocation is effectively non-preemptible when:

1. `checkpoint: None` AND `preemption_class >= 7` — High cost to preempt (all progress lost), high priority
2. Medical allocations (always class 10)
3. Allocations within 5 minutes of walltime completion (configurable: `near_completion_threshold`)

The scheduler avoids placing non-preemptible allocations on borrowed nodes, since those nodes may need to be reclaimed.

## Preemption Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `lattice_preemptions_total` | counter | Labels: `vcluster`, `reason` (priority/reclaim/medical) |
| `lattice_preemption_checkpoint_duration_seconds` | histogram | Time from hint to checkpoint completion |
| `lattice_preemption_victim_requeue_total` | counter | Preempted allocations re-entering queue |
| `lattice_preemption_failed_checkpoint_total` | counter | Checkpoint timeouts during preemption |

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — f₈ checkpoint_efficiency in cost function
- [checkpoint-broker.md](checkpoint-broker.md) — Checkpoint cost model and application protocol
- [failure-modes.md](failure-modes.md) — Requeue policy for preempted allocations
- [node-lifecycle.md](node-lifecycle.md) — Node state transitions during preemption
- [sensitive-workloads.md](sensitive-workloads.md) — Medical allocations never preempted

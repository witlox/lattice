# Quota Enforcement

## Design Principle

Two-tier enforcement matching the two consistency domains (ADR-004). Hard limits enforced at the quorum (strong consistency, cannot be violated). Soft limits enforced at the scheduler (eventual consistency, may temporarily overshoot, self-correcting).

## Hard Quotas (Quorum-Enforced)

Hard quotas are checked during Raft proposal validation, before commit. A proposal that would violate a hard quota is rejected immediately.

| Quota | Scope | Enforcement |
|-------|-------|-------------|
| `max_nodes` | Per tenant | Quorum rejects allocation proposals that would exceed the tenant's maximum concurrent node count |
| `max_concurrent_allocations` | Per tenant | Quorum rejects proposals that would exceed the tenant's maximum number of running allocations |
| `sensitive_pool_size` | System-wide | Hard limit on the number of nodes that can be claimed for sensitive use |

**Guarantees:** These quotas cannot be violated, even momentarily. Two vCluster schedulers proposing conflicting allocations that together would exceed a hard quota: the first committed wins, the second is rejected and retried next cycle.

**Error handling:** Hard quota rejection returns a clear error to the user:
```
allocation rejected: tenant "physics" would exceed max_nodes quota (current: 195, requested: 10, limit: 200)
```

## Soft Quotas (Scheduler-Level)

Soft quotas are tracked with eventual consistency. They influence scheduling decisions through the cost function but do not hard-block allocations.

### GPU-Hours Budget

```
gpu_hours_budget: 100000  # per billing period (month)
gpu_hours_used: 87500     # eventually consistent counter
```

**Behavior:** The scheduler uses remaining budget as a penalty in the cost function. As budget depletes:
- 0-80% used: no penalty
- 80-100% used: increasing penalty (lower scheduling priority)
- >100% used: very low score (effective starvation for new allocations, but not hard rejection)

**Consistency window:** Up to ~30 seconds of lag. Acceptable because: (a) scheduling cycle is 5-30s, (b) over-allocation is self-correcting via fair-share scoring, (c) GPU-hours tracking is for billing, not safety.

### Fair Share Target

```
fair_share_target: 0.15  # tenant should get ~15% of system capacity
```

**Behavior:** Feeds into f₃ (fair_share_deficit) in the cost function. Tenants below their share get priority; tenants above are deprioritized. Not a hard ceiling — a tenant can use more than their share when resources are idle.

### Burst Allowance

```
burst_allowance: 1.5  # allow up to 150% of fair share when resources idle
```

**Behavior:** Allows temporary over-allocation when the system has spare capacity. When demand increases and other tenants need their share, burst allocations are the first candidates for preemption (via checkpoint cost model).

## Internal Budget Ledger

When Waldur is unavailable or not configured, the scheduler computes GPU-hours consumption internally from allocation records in the quorum. This replaces the previously empty `budget_utilization` map in the cost function.

### Computation

Two metrics are tracked:

```
node_hours_used = Σ (end_time - started_at).hours × assigned_nodes.len()
gpu_hours_used  = Σ (end_time - started_at).hours × Σ gpu_count_per_node
```

- For running allocations: `end_time = now`
- For completed/failed/cancelled: `end_time = completed_at`
- Only allocations within the configured `budget_period_days` (default: 90 days, rolling window) are included
- Node GPU count looked up from current hardware inventory; unknown nodes default to 1 GPU
- Node-hours is the universal metric (works for CPU-only and GPU nodes)
- When both `gpu_hours_budget` and `node_hours_budget` are set, the **worse** (higher) utilization fraction drives the budget penalty

### Budget Period

Configurable via `scheduling.budget_period_days` (default: 90). This is a **rolling window**, not a calendar-aligned reset. Calendar-aligned resets require Waldur to push new `gpu_hours_budget` values at period boundaries.

### Waldur Override

When Waldur is available, its `remaining_budget()` response takes precedence over the internal ledger. When Waldur is unavailable (transient failure), the internal ledger provides fallback data so budget enforcement continues.

### API Access

- `GET /api/v1/tenants/{id}/usage?days=90` — tenant GPU-hours usage
- `GET /api/v1/usage?user=alice&days=90` — per-user usage across tenants
- `lattice usage --tenant physics` — CLI convenience
- `lattice usage` — show usage across all tenants for current user

## Exhausted Budget Behavior

### GPU-Hours Budget Exhausted

1. New allocations for this tenant receive a very low scheduling score (effective starvation, not hard rejection)
2. Tenant admin notified via API event
3. Running allocations continue to completion (no preemption for budget reasons)
4. If Waldur integration enabled: Waldur can update the budget (cross-ref: [accounting.md](accounting.md))
5. Tenant admin can request budget increase through Waldur self-service portal

### Max Nodes Exhausted

1. Hard rejection at quorum — clear error returned to user
2. User must wait for running allocations to complete or cancel existing allocations
3. No waiting queue for hard-quota-blocked allocations (submit is rejected, user resubmits when capacity is available)

## Quota Update Flow

### Administrative Update

System admin updates tenant quotas via API:
```
PUT /v1/tenants/{id}/quotas
{
  "max_nodes": 250,
  "max_concurrent_allocations": 50,
  "gpu_hours_budget": 150000
}
```

Hard quota changes are Raft-committed (immediate effect). Soft quota changes propagate eventually.

### Waldur-Driven Update

When Waldur integration is enabled, Waldur can push quota changes:

1. Waldur determines budget exhaustion or contract change
2. Waldur calls lattice-api: `PUT /v1/tenants/{id}/quotas` (authenticated with Waldur service token)
3. Hard quotas committed via Raft; soft quotas propagated to schedulers
4. Reducing `max_nodes` below current usage does not preempt running allocations — it prevents new ones

## Quota Reduction While Allocations Are Running

When a quota is reduced below current usage (e.g., Waldur reduces `max_nodes` from 200 to 100, but tenant is currently using 150):

### Hard Quota Reduction

- **Running allocations are not preempted.** The reduced quota only blocks new allocations.
- Current usage (150) exceeds new limit (100): all new proposals for this tenant are rejected until usage drops below 100.
- The user receives a clear error on new submissions:
  ```
  allocation rejected: tenant "physics" exceeds max_nodes quota
    Current usage: 150 nodes
    New limit: 100 nodes
    Hint: Wait for running allocations to complete, or contact your tenant admin.
  ```
- As running allocations complete naturally, usage drops. When usage < new limit: new allocations are accepted again.

### Soft Quota Reduction

- Reduced `gpu_hours_budget`: scheduling score penalty increases. Pending allocations get lower priority but are not rejected.
- Reduced `fair_share_target`: tenant gets deprioritized but can still schedule when resources are idle.
- No immediate impact on running allocations.

### Pending Allocations

Allocations that are `Pending` (in the scheduler queue but not yet committed) when a hard quota is reduced:
- They are not retroactively cancelled.
- If proposed to quorum, the proposal is rejected due to the new quota.
- The scheduler will not re-propose them until quota headroom exists.
- User sees allocation stuck in `Pending` state. `lattice status` shows the reason: `"waiting for quota headroom"`.

## Sensitive Quota Considerations

Sensitive quotas are always hard quotas:

- `sensitive_pool_size` — System-wide hard limit, quorum-enforced
- Sensitive node claims always go through quorum (strong consistency)
- No soft/eventual quota mechanisms for sensitive resources
- Idle sensitive nodes (claimed but unused) are not reclaimable — they remain allocated to the claiming user

Cross-ref: [sensitive-workloads.md](sensitive-workloads.md) for the full sensitive workload model.

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — f₃ fair_share_deficit uses soft quota targets
- [accounting.md](accounting.md) — Waldur quota feedback loop
- [sensitive-workloads.md](sensitive-workloads.md) — Sensitive quotas are always hard
- [autoscaling.md](autoscaling.md) — Scale-up respects hard quota limits

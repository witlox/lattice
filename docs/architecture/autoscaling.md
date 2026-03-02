# Autoscaling

## Design Principle

Simple, metric-driven scaling. No complex control theory. The scheduler adjusts node count within bounds based on a single metric threshold. Users set bounds, the scheduler respects them.

## Reactive Lifecycle

Defined in `crates/lattice-common/src/types.rs` (`LifecycleType::Reactive`):

```
Reactive {
    min_nodes: u32,
    max_nodes: u32,
    metric: String,       // e.g., "gpu_utilization", "queue_depth", "request_rate"
    target: String,       // e.g., "0.80" (80% GPU utilization target)
}
```

Reactive allocations are unbounded in duration (like services) but have variable node count.

## Scaling Loop

1. **Start:** Allocation begins with `min_nodes`
2. **Evaluate:** Every evaluation interval (default: 60s), the scheduler queries TSDB for the allocation's metric
3. **Scale up:** If metric > target for `scale_up_window` (default: 2 minutes):
   - Propose adding 1 node (conservative: avoid large jumps)
   - Quorum validates the node addition (ownership transfer)
   - Node agent starts processes on the new node
   - Repeat until metric ≤ target or `max_nodes` reached
4. **Scale down:** If metric < target × `scale_down_threshold` (default: 0.5) for `scale_down_window` (default: 5 minutes):
   - Propose removing 1 node (least-loaded or most-recently-added)
   - Graceful drain: stop sending work to the node, wait for in-flight requests
   - Node released back to scheduling pool
   - Repeat until metric ≥ target × scale_down_threshold or `min_nodes` reached
5. **Cooldown:** After any scale event, no further scaling for `cooldown_period` (default: 3 minutes)

### Why Conservative Scaling

- Adding 1 node at a time prevents overshooting (workloads often have non-linear resource curves)
- Scale-down windows are longer than scale-up windows (scale down is more disruptive)
- Cooldown prevents oscillation from metric noise

## Built-In Scaling Metrics

| Metric | Description | Source | Best For |
|--------|-------------|--------|----------|
| `gpu_utilization` | Mean GPU SM occupancy across allocation | eBPF / NVML | ML inference services |
| `cpu_utilization` | Mean CPU usage across allocation | eBPF | CPU-bound services |
| `request_rate` | Inbound requests per second | eBPF (network flow tracking) | API/web services |
| `queue_depth` | Pending request queue length | Application-reported or eBPF | Batch-processing services |

### Custom Metrics

Any metric available in TSDB can be used for scaling by specifying a label matcher:

```yaml
lifecycle:
  type: reactive
  min_nodes: 2
  max_nodes: 20
  metric: "custom_metric{job='my-inference'}"
  target: "100"  # e.g., 100 pending requests
```

The scheduler queries TSDB with the label matcher scoped to the allocation's nodes.

## Configuration Defaults

| Parameter | Default | Configurable |
|-----------|---------|--------------|
| `evaluation_interval` | 60s | Per allocation |
| `scale_up_window` | 2 minutes | Per allocation |
| `scale_down_window` | 5 minutes | Per allocation |
| `scale_down_threshold` | 0.5 (50% of target) | Per allocation |
| `cooldown_period` | 3 minutes | Per allocation |

## Quota Interaction

Scale-up respects the tenant's `max_nodes` hard quota (cross-ref: [quota-enforcement.md](quota-enforcement.md)):

- Before proposing a scale-up, the scheduler checks if the tenant has remaining node capacity
- If `max_nodes` would be exceeded: scale-up is a no-op, allocation continues at current size
- No error raised — the allocation operates within its current bounds
- If quota is later increased (e.g., via Waldur), scaling resumes automatically

## Preemption Interaction

Borrowed nodes (from elastic resource sharing) are valid targets for reactive scaling, but they carry a preemption risk:

- Scaling onto borrowed nodes gives the allocation more capacity temporarily
- If the home vCluster reclaims the node: reactive allocation scales down gracefully
- Minimum guarantee: `min_nodes` always come from the allocation's home vCluster (not borrowed)

## Error Handling

### Metric Query Failure (TSDB Down)

If the scheduler cannot query TSDB for the scaling metric:

1. First failure: skip this evaluation cycle, log warning
2. Consecutive failures (3+): alert raised (`lattice_autoscaling_metric_query_failures_total`)
3. No scaling decisions made while metric is unavailable — allocation stays at current size
4. When TSDB recovers: normal evaluation resumes on next cycle

The allocation is never scaled blindly. No metric = no action.

### Scale-Up Proposal Rejected

If the quorum rejects a scale-up proposal (e.g., race condition with another vCluster):

1. Retry on next evaluation cycle (60s later)
2. Maximum 3 consecutive retries for the same scale-up
3. After 3 rejections: log warning, back off for 2 cooldown periods
4. Scale-up resumes when conditions change (nodes become available)

### Scale-Down During Borrowed Node Reclamation

If a borrowed node is reclaimed by the home vCluster while the reactive allocation is scaling down:

1. The reclamation takes priority (home vCluster always wins)
2. The reactive allocation loses the node immediately (graceful drain attempted, but not guaranteed)
3. If this drops below `min_nodes`: scheduler attempts to acquire a replacement node from the home vCluster
4. If no replacement available: allocation operates below `min_nodes` temporarily, alert raised

### Metric Oscillation

If the metric oscillates around the target, causing repeated scale-up/scale-down:

- The cooldown period (default: 3 minutes) prevents rapid oscillation
- If scale events alternate for more than 5 cycles: alert raised suggesting the user adjust their target or increase cooldown
- No automatic target adjustment — the user must update the configuration

### Preemption During Scale-Up

If a reactive allocation is scaling up while simultaneously being preempted (e.g., a higher-priority job arrives):

1. The preemption takes priority — the checkpoint/preemption sequence begins
2. Any in-flight scale-up proposals are cancelled (quorum rejects proposals for allocations in `Checkpointing` state)
3. After preemption completes: the allocation is suspended with its last stable node count
4. When resumed: scaling restarts from `min_nodes`, re-evaluating the metric from scratch
5. The cooldown period applies after resume to prevent immediate re-scaling

If preemption and scale-up proposals race at the quorum:
- The quorum serializes all proposals — one wins, the other is rejected
- The rejected proposal is retried on the next scheduling cycle (if still applicable)

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — Reactive allocations scored by the knapsack solver like any allocation
- [quota-enforcement.md](quota-enforcement.md) — Hard quota limits on scale-up
- [telemetry.md](telemetry.md) — Metric sources for scaling decisions
- [preemption.md](preemption.md) — Borrowed node reclamation
- types.rs — `LifecycleType::Reactive` definition

# Checkpoint Broker

## Purpose

The checkpoint broker coordinates between the scheduler's resource management decisions and running applications' checkpoint capabilities. It enables cost-aware preemption: the scheduler can reclaim resources from running jobs by triggering checkpoints, with the decision driven by an economic cost function.

## Cost Model

### When to Checkpoint

```
Should_checkpoint(j, t) = Value(j, t) > Cost(j, t)
```

### Cost Components

```
Cost(j, t) = write_time(j) + compute_waste(j) + storage_cost(j)

write_time(j):
  Estimated from: checkpoint_size(j) / storage_write_bandwidth
  checkpoint_size(j) estimated from: GPU memory usage × node count
  storage_write_bandwidth from: VAST API current throughput metrics

compute_waste(j):
  GPU-seconds lost during checkpoint I/O
  = write_time(j) × node_count(j) × gpu_per_node

storage_cost(j):
  = checkpoint_size(j) × cost_per_GB_on_target_tier
```

### Value Components

```
Value(j, t) = recompute_saved(j, t) + preemptability(j, t) + backlog_relief(t)

recompute_saved(j, t):
  GPU-hours that would be lost if the job fails and restarts from scratch
  = time_since_last_checkpoint(j) × node_count(j) × gpu_per_node
  Weighted by failure_probability(j, t) which increases with:
    - Job duration (longer jobs more likely to hit hardware issues)
    - Node health signals (ECC errors, thermal warnings from BMC)

preemptability(j, t):
  Value of being able to preempt this job if a higher-priority job arrives
  = Σ (waiting_higher_priority_jobs × their urgency) × preemption_probability
  High when higher-priority work is queued and this job sits on reclaimable nodes

backlog_relief(t):
  = backlog_pressure(t) × estimated_queue_wait_reduction_if_nodes_freed
  Global signal: how much would freeing these nodes help the overall queue?
```

### Decision Dynamics

| Scenario | backlog | preempt demand | node health | Decision |
|---|---|---|---|---|
| Quiet system, healthy nodes | Low | Low | Good | Checkpoint infrequently (every 6h) |
| Deep queue, medical job waiting | High | High | Good | Checkpoint now, preempt |
| Node ECC errors increasing | Low | Low | Degrading | Checkpoint proactively, migrate |
| Large job nearing walltime | Low | Low | Good | Checkpoint for restart capability |

## Application Protocol

### Three Communication Modes

Applications opt into checkpoint coordination via one of three mechanisms:

**1. Signal-based (legacy compatibility)**
```
Node agent sends SIGUSR1 to the application's process group.
Application catches signal, writes checkpoint, signals completion via exit of a sentinel file.
Timeout: if no completion signal within checkpoint_timeout, assume non-checkpointable.
```

**2. Shared memory flag (low-latency)**
```
Node agent sets a flag in a shared memory region mapped at a well-known path.
Application polls the flag (or uses futex wait) and initiates checkpoint.
Completion: application clears the flag and sets a "done" flag.
Best for performance-sensitive applications that can't afford signal handler overhead.
```

**3. gRPC callback (agent-aware applications)**
```
Application registers a checkpoint endpoint with the node agent at startup.
Node agent calls the endpoint when checkpoint is requested.
Application responds with estimated completion time, then streams progress.
Most expressive: supports negotiation (application can request deferral).
```

### Checkpoint Destinations

Checkpoints are written to a standard location:
```
s3://{tenant}/{project}/{allocation_id}/checkpoints/{checkpoint_id}/
```
Or, if NFS is preferred for POSIX-style checkpoint (e.g., MPI checkpoint/restart):
```
/scratch/{tenant}/{project}/{allocation_id}/checkpoints/{checkpoint_id}/
```

The checkpoint broker coordinates with the data plane to ensure bandwidth is available.

### Non-Checkpointable Applications

If an application declares `checkpoint: none` or fails to respond to checkpoint hints:
- The allocation is marked as non-preemptible in the cost function
- It receives a penalty in the knapsack solver (ties up resources without flexibility)
- The scheduler avoids placing it on borrowed/elastic nodes

Fallback option: DMTCP (Distributed MultiThreaded Checkpointing) for transparent process-level checkpointing. Higher overhead, but works for unmodified applications.

## Integration with Scheduler

The checkpoint broker runs as part of the scheduler plane, with access to:
- Running allocation state (from quorum)
- Node health telemetry (from eBPF/OpenCHAMI)
- Storage metrics (from VAST API)
- Queue state (from vCluster schedulers)

It evaluates the cost function continuously (every 30-60 seconds for each running allocation) and issues checkpoint hints when the threshold is crossed.

## Storage Outage Behavior

When the checkpoint destination (VAST S3 or NFS) is unavailable:

1. **Detection:** Checkpoint broker detects storage unavailability via failed write probes or VAST API health checks
2. **Immediate effect:** All pending checkpoint requests are paused (not cancelled)
3. **Cost function adjustment:** `storage_write_bandwidth` drops to 0, making `write_time(j)` infinite — the cost function naturally suppresses checkpoint decisions
4. **Running allocations:** Continue running. They are effectively non-preemptible during the outage (no checkpoint possible)
5. **Preemption requests:** If preemption is forced (e.g., medical claim), the victim receives SIGTERM without checkpoint. The allocation is marked `Failed` (not `Suspended`) since no checkpoint was written
6. **Recovery:** When storage recovers, the broker re-evaluates all running allocations on the next cycle. Allocations with high `recompute_saved` value are prioritized for immediate checkpoint
7. **Alert:** `lattice_checkpoint_storage_unavailable` gauge set to 1; critical alert fired

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — f₈ checkpoint_efficiency in the cost function
- [preemption.md](preemption.md) — Preemption sequence and checkpoint timeout handling
- [failure-modes.md](failure-modes.md) — Checkpoint broker crash recovery
- [telemetry.md](telemetry.md) — Node health signals (ECC errors) feeding into checkpoint urgency
- [sensitive-workloads.md](sensitive-workloads.md) — Medical allocations and checkpoint constraints
- [data-staging.md](data-staging.md) — Storage bandwidth sharing with checkpoint writes

# Checkpoint Module Interfaces (lattice-checkpoint)

## CheckpointBroker Trait

Defined in lattice-common, implemented in lattice-checkpoint.

```
trait CheckpointBroker: Send + Sync
  ├── evaluate(alloc: &Allocation) → Result<Option<CheckpointRequest>>
  │     Should this allocation checkpoint now?
  │     Returns None if Value ≤ Cost.
  │
  └── initiate(request: CheckpointRequest) → Result<CheckpointResponse>
        Deliver checkpoint signal to node agent(s).
        Handles partial failures (some nodes succeed, some fail).
```

**Spec source:** IP-04 Path B (checkpoint/preemption), FM-08 (broker crash), cross-context scenario "Allocation completes naturally while checkpoint hint is in flight"

## LatticeCheckpointBroker

```
LatticeCheckpointBroker
  ├── new() → Self
  ├── with_nfs(nfs_path: PathBuf) → Self
  ├── with_agent_pool(pool: Arc<dyn NodeAgentPool>) → Self
  ├── with_allocation_store(store: Arc<dyn AllocationStore>) → Self
  │
  ├── evaluate_batch(allocations: &[Allocation]) → Vec<CheckpointRequest>
  │     Batch evaluation of all running allocations.
  │     Called during scheduling cycle when preemption is needed.
  │
  └── evaluate_single(alloc: &Allocation) → Option<CheckpointRequest>
        Single allocation evaluation.
```

## Cost Model

```
evaluate_checkpoint(alloc, params, ctx) → CheckpointEvaluation

CheckpointEvaluation {
  should_checkpoint: bool,          // Value > Cost
  value: f64,                       // recompute_saved + preemptability + backlog_relief
  cost: f64,                        // write_time + compute_waste + storage_cost
  reason: String,                   // Human-readable explanation
}

CheckpointParams {
  write_time_estimate: Duration,    // Based on checkpoint size and storage bandwidth
  storage_cost_per_gb: f64,         // S3/NFS storage cost
  recompute_value_per_hour: f64,    // Value of compute time saved on restart
}
```

**Decision factors:**
- Backlog pressure increases checkpoint aggressiveness (more pending work → more value in preemptability)
- Checkpoint size and storage bandwidth determine write_time cost
- Time since last checkpoint determines recompute value

## NodeAgentPool Trait

Defined in lattice-common, used by checkpoint broker.

```
trait NodeAgentPool: Send + Sync
  └── send_checkpoint(alloc_id: &AllocId, nodes: &[NodeId]) → Result<CheckpointResponse>
        Deliver CHECKPOINT_HINT to all node agents for this allocation.
        Partial failure handling: reports which nodes succeeded/failed.
```

**Contract:**
- Hint is advisory (INV-E4 — walltime always wins)
- Timeout: checkpoint_timeout (default 10min), one 50% extension for slow-but-responsive apps
- On timeout: SIGTERM → SIGKILL → allocation Failed (not Suspended)
- On node crash during checkpoint: allocation Failed, requeued per policy (FM: Node crashes during checkpoint)

## Checkpoint Protocol

```
CheckpointProtocol
  ├── Signal       — SIGUSR1 to application process
  ├── Shmem        — Set futex/flag in shared memory region
  └── GrpcCallback — gRPC call to application's checkpoint endpoint
```

## Checkpoint Policy

```
CheckpointPolicy {
  min_interval: Duration,           // Don't checkpoint more often than this
  max_interval: Duration,           // Force checkpoint if exceeded
  min_disk_free_gb: u64,           // Don't checkpoint if storage low
}
```

# Event Catalog

All events the system produces or consumes. Events are the integration mechanism between bounded contexts where asynchronous communication is acceptable.

## Event Categories

1. **Raft commands** — Synchronous state mutations (not events per se, but drive all state changes)
2. **Streaming events** — Real-time notifications for Watch/SSE subscribers
3. **Accounting events** — Async push to Waldur (IP-10)
4. **Telemetry events** — Metrics/logs pushed to external systems (IP-05)
5. **Checkpoint events** — Advisory signals between broker and node agents (IP-04)

## 1. Raft Commands (Synchronous State Mutations)

See `specs/architecture/interfaces/consensus.md` — Command enum. These are not events in the traditional sense but are the primary mechanism for state change propagation within the strongly-consistent domain.

## 2. Streaming Events (AllocationEvent)

Published via EventBus. Consumed by Watch/SSE subscribers.

| Event | Producer | Trigger | Payload |
|---|---|---|---|
| StateChanged | lattice-quorum | Allocation state transition | `{ alloc_id, old_state, new_state, timestamp, message }` |
| NodesAssigned | lattice-quorum | Placement committed | `{ alloc_id, nodes: Vec<NodeId>, timestamp }` |
| NodesReleased | lattice-quorum | Allocation completed/failed | `{ alloc_id, nodes: Vec<NodeId>, timestamp }` |
| CheckpointStarted | lattice-checkpoint | Checkpoint hint sent | `{ alloc_id, timestamp }` |
| CheckpointCompleted | lattice-node-agent | Checkpoint written | `{ alloc_id, path, size_bytes, timestamp }` |
| CheckpointFailed | lattice-node-agent | Checkpoint timed out | `{ alloc_id, reason, timestamp }` |
| PreemptionStarted | lattice-scheduler | Preemption decision | `{ alloc_id, preempted_by, timestamp }` |
| WalltimeWarning | lattice-node-agent | 90% walltime reached | `{ alloc_id, remaining_seconds, timestamp }` |
| WalltimeExpired | lattice-node-agent | Walltime reached | `{ alloc_id, timestamp }` |

**Delivery:** At-most-once (in-process channel). Missed events recoverable via Get/List queries.

**Spec source:** Watch RPC, cross-context scenarios (preemption vs completion, walltime vs checkpoint)

## 3. Accounting Events (Waldur Push)

Async push at configurable interval (default 60s). At-least-once delivery.

| Event | Trigger | Payload |
|---|---|---|
| allocation.started | Allocation enters Running | `{ alloc_id, tenant, vcluster, user, nodes, gpu_count, start_time }` |
| allocation.completed | Allocation exits (any terminal state) | `{ alloc_id, tenant, end_time, exit_code, duration, gpu_hours }` |
| allocation.checkpointed | Successful checkpoint | `{ alloc_id, checkpoint_size, storage_cost }` |
| node.claimed | Sensitive node claimed | `{ node_id, tenant, user, claim_time }` |
| node.released | Node released from allocation | `{ node_id, tenant, release_time, was_sensitive }` |

**Buffer:** Memory (10K) → Disk WAL (100K) → Drop with `lattice_accounting_events_dropped_total` counter.

**Guarantee:** Accounting never blocks scheduling (INV-N5). Dropped events reconstructable from quorum logs via `lattice admin accounting reconcile`.

**Spec source:** IP-10 (Scheduling → Accounting), FM-13 (Waldur unavailable), cross-context accounting scenarios.

## 4. Telemetry Events (Metrics/Logs Push)

### Metrics

Pushed to VictoriaMetrics every push_interval (default 30s).

| Metric | Source | Labels | Description |
|---|---|---|---|
| `lattice_cpu_utilization` | /proc/stat | node_id, alloc_id | CPU usage % |
| `lattice_memory_used_bytes` | /proc/meminfo | node_id, alloc_id | Memory consumption |
| `lattice_gpu_utilization` | nvml/rocm-smi | node_id, alloc_id, gpu_index | GPU compute % |
| `lattice_gpu_memory_used_bytes` | nvml/rocm-smi | node_id, alloc_id, gpu_index | GPU memory |
| `lattice_network_tx_bytes` | /proc/net/dev | node_id, alloc_id | Network egress |
| `lattice_network_rx_bytes` | /proc/net/dev | node_id, alloc_id | Network ingress |
| `lattice_io_read_bytes` | /proc/diskstats | node_id, alloc_id | Disk read |
| `lattice_io_write_bytes` | /proc/diskstats | node_id, alloc_id | Disk write |
| `lattice_gpu_power_watts` | nvml/rocm-smi | node_id, gpu_index | Power draw |
| `lattice_gpu_temperature` | nvml/rocm-smi | node_id, gpu_index | Temperature |

### System Metrics (Prometheus /metrics endpoint)

| Metric | Type | Description |
|---|---|---|
| `lattice_allocations_total` | Counter | Allocations submitted |
| `lattice_allocations_active` | Gauge | Currently running |
| `lattice_scheduling_cycle_duration_seconds` | Histogram | Scheduling cycle time |
| `lattice_raft_commit_duration_seconds` | Histogram | Raft commit latency |
| `lattice_raft_sensitive_audit_entries_total` | Counter | Audit entries written |
| `lattice_node_heartbeat_age_seconds` | Gauge | Time since last heartbeat |
| `lattice_preemptions_total` | Counter | Preemptions executed |
| `lattice_checkpoint_walltime_conflict_total` | Counter | Walltime vs checkpoint conflicts |
| `lattice_accounting_buffer_size` | Gauge | Pending accounting events |
| `lattice_accounting_events_dropped_total` | Counter | Dropped accounting events |
| `lattice_network_vni_setup_duration_seconds` | Histogram | VNI propagation time |

### Logs

Dual-path: ring buffer (live streaming) + S3 (persistent storage).

| Log Event | Source | Destination |
|---|---|---|
| Application stdout/stderr | Node agent process capture | Ring buffer → S3 |
| System events | Node agent lifecycle | Ring buffer → S3 |
| Sensitive access logs | API middleware | Raft audit log + S3 |

## 5. Checkpoint Events (Advisory Signals)

| Signal | Direction | Delivery | Spec Source |
|---|---|---|---|
| CHECKPOINT_HINT | Checkpoint broker → Node agent | gRPC `send_checkpoint()` | IP-04 Path B |
| SIGUSR1/SIGUSR2 | Node agent → Application process | OS signal | Checkpoint protocol |
| Checkpoint complete | Node agent → Quorum | gRPC + Raft commit | IP-04 |
| Checkpoint timeout | Node agent (timer) | Internal state transition | FM-18 |

## Event Flow: Preemption with Checkpoint

```
Scheduler ──(preemption decision)──► Checkpoint Broker
  │
  │  evaluates Value > Cost
  │
  ▼
Checkpoint Broker ──(CHECKPOINT_HINT)──► Node Agent(s) [gRPC]
  │
  │  forwards to application
  │
  ▼
Node Agent ──(SIGUSR1)──► Application Process
  │
  │  application writes checkpoint
  │
  ▼
Node Agent ──(checkpoint_complete)──► Quorum [Raft commit]
  │
  │  allocation → Suspended, nodes released
  │
  ▼
Scheduler ──(freed nodes)──► proposes for higher-priority allocation
```

## Event Ordering Guarantees

| Pair | Ordering | Mechanism |
|---|---|---|
| Audit entry → Sensitive action | Strict (INV-O3) | Raft commit gate |
| Node assignment → Prologue start | Strict (INV-O1) | gRPC response after Raft commit |
| Prologue → Entrypoint | Strict (INV-O2) | State machine sequencing |
| Walltime expiry → Checkpoint | Walltime wins (INV-E4) | Independent timer |
| Accounting events | At-least-once, ordered per allocation | Buffered push |
| Streaming events | At-most-once, no ordering guarantee | In-process channel |

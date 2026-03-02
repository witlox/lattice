# Troubleshooting Guide

## Allocation Stuck in Pending

**Symptom:** `lattice status` shows allocation in `Pending` for longer than expected.

**Diagnosis:**

```bash
# Check why the allocation isn't being scheduled
lattice status 12345 --verbose
```

| Verbose Output | Cause | Fix |
|----------------|-------|-----|
| `waiting for quota headroom` | Tenant hard quota (`max_nodes` or `max_concurrent_allocations`) exceeded | Cancel other allocations or request quota increase |
| `no nodes matching constraints` | No nodes with requested GPU type, features, or topology | Relax constraints (`--topology=any`), check `lattice nodes --state=ready` |
| `data staging in progress` | Input data being pre-staged from warm/cold tier | Wait (check progress with `lattice status 12345 --verbose`), or submit without `tier_hint: hot` |
| `insufficient conformance group` | Not enough nodes with matching conformance fingerprint for multi-node job | Reduce node count, or wait for OpenCHAMI to remediate drifted nodes |
| `all suitable nodes occupied` | Resources are busy; allocation is queued normally | Wait; check queue depth with `lattice status --state=pending` |
| `soft quota penalty (low score)` | GPU-hours budget nearly exhausted; allocation deprioritized | Request budget increase from tenant admin or Waldur portal |

**Deeper investigation:**

```bash
# Check scheduler cycle is running
lattice admin scheduler status --vcluster=hpc-batch

# Check if proposals are being rejected
lattice admin raft status

# View scheduling metrics
# (high proposal rejection rate may indicate race conditions or quota contention)
```

## Scheduling Cycle Slow

**Symptom:** `lattice_scheduling_cycle_duration_seconds` p99 > 30s.

**Diagnosis:**

| Check | Command | What to Look For |
|-------|---------|-----------------|
| Queue depth | `lattice status --state=pending --count` | > 500 pending allocations |
| Cost function time | Grafana: `lattice_scheduling_cost_function_duration_seconds` | Dominant component of cycle |
| Conformance group fragmentation | `lattice nodes -o wide \| sort -k7 \| uniq -c` | Many small groups |
| Topology solver | Grafana: cycle time breakdown | Multi-group spanning expensive |

**Fixes:**

| Cause | Fix |
|-------|-----|
| Too many pending allocations | Increase cycle interval to batch more proposals |
| Cost function slow | Check if custom metrics (f₅ data_readiness) are causing TSDB query delays |
| Conformance fragmented | Standardize firmware, or reduce w₉ for tolerant workloads |
| Topology solver | Reduce backfill depth, or allow `topology: any` for more jobs |

## Node Stuck in Degraded/Down

**Symptom:** Node shows `Degraded` or `Down` in `lattice nodes`.

**Diagnosis:**

```bash
# Check node details
lattice nodes x1000c0s0b0n0

# Check heartbeat
# If heartbeat missing: node agent may be down or network partitioned
```

| State | Duration | Likely Cause | Fix |
|-------|----------|-------------|-----|
| Degraded, < 2 min | Transient network blip | Wait; likely self-resolves |
| Degraded, > 5 min | Agent crash or network partition | SSH to node, check agent: `systemctl status lattice-agent` |
| Down | Agent not recovering | Check BMC via OpenCHAMI: `manta node status x1000c0s0b0n0` |
| Down, BMC unreachable | Hardware failure | Physical inspection required |

**Recovery:**

```bash
# If agent crashed, restart it
ssh x1000c0s0b0n0 systemctl restart lattice-agent

# If node needs reboot
lattice node disable x1000c0s0b0n0
# (coordinate with OpenCHAMI for reboot)
lattice node undrain x1000c0s0b0n0  # after reboot + health check
```

## Raft Commit Latency High

**Symptom:** `lattice_raft_commit_latency_seconds` p99 > 1s.

**Diagnosis:**

| Check | What to Look For |
|-------|-----------------|
| Disk I/O on quorum members | WAL write latency. Quorum members need fast SSD. |
| Network between quorum members | Packet loss or high latency between quorum nodes |
| Leader overloaded | Too many proposals per second |
| Log compaction | Snapshot in progress (one-time spike, normal) |

**Fixes:**

| Cause | Fix |
|-------|-----|
| Slow disk | Move WAL to dedicated NVMe SSD |
| Network latency | Ensure quorum members are on low-latency network (same rack or switch) |
| Leader overload | Increase scheduling cycle interval to reduce proposal rate |
| Log too large | Reduce snapshot interval (more frequent snapshots = smaller log) |

## Allocation Fails During Prologue

**Symptom:** Allocation moves from `Running` to `Failed` within seconds of starting.

**Diagnosis:**

```bash
lattice logs 12345
# Look for prologue errors:
#   "uenv pull failed: hash mismatch"
#   "mount failed: ENOSPC"
#   "NFS mount timeout"
```

| Error | Cause | Fix |
|-------|-------|-----|
| Hash mismatch | Corrupted image in cache or registry | `lattice cache evict --image=... --node=...` and retry |
| ENOSPC | Node-local cache full, eviction couldn't free space | Check cache status: `lattice cache status --node=...`. Evict unused images manually. |
| NFS mount timeout | VAST unavailable or network issue | Check VAST health. Check Slingshot storage traffic class. |
| Image not found | uenv name/version doesn't exist in registry | Verify with `lattice cache status --node=...` or check the uenv registry directly |

## Preemption Not Working

**Symptom:** Higher-priority allocation waiting despite lower-priority allocations running on suitable nodes.

**Diagnosis:**

```bash
lattice status 12345 --verbose
# Check if preemption is enabled for this vCluster
lattice admin vcluster show hpc-batch
```

| Cause | Fix |
|-------|-----|
| Pending job's priority class ≤ running jobs' class | Preemption only works downward. Check priority classes. |
| Running jobs are non-preemptible (`checkpoint: none` + high class) | Wait for them to complete |
| Running jobs are near completion (>90% walltime) | Scheduler avoids preempting near-completion jobs. Wait. |
| vCluster doesn't allow preemption | Check vCluster config. Service vClusters only preempt borrowed nodes. |

## Autoscaling Not Triggering

**Symptom:** Reactive allocation stays at `min_nodes` despite high metric value.

**Diagnosis:**

```bash
# Check current metric value
lattice top 12345 --metric=gpu_utilization

# Check scaling events
lattice status 12345 --verbose
```

| Cause | Fix |
|-------|-----|
| Metric below target | Scaling only triggers when metric > target for `scale_up_window` (2 min) |
| Cooldown period active | Recent scale event; wait for cooldown (3 min default) |
| TSDB query failing | Check `lattice_autoscaling_metric_query_failures_total` metric |
| Tenant quota exhausted | `max_nodes` reached; scale-up is a no-op |
| Metric name wrong | Verify metric exists in TSDB: `lattice top 12345 --metric=<name>` |

## Medical Node Won't Accept Claims

**Symptom:** Medical node claim rejected.

**Diagnosis:**

| Check | What to Look For |
|-------|-----------------|
| `lattice nodes <id>` | Is node in `Ready` state? (Not `Degraded`, `Down`, `Draining`) |
| Conformance | Is node's conformance fingerprint matching the medical baseline? |
| Pool size | Is `medical_pool_size` quota exhausted? |
| Previous wipe | Was the node properly wiped after last medical use? |

**Fix:**

```bash
# Check conformance
lattice nodes x1000c0s0b0n0 -o wide
# If drifted: coordinate with OpenCHAMI for remediation

# Check medical pool
lattice admin tenant show hospital-a --quotas
# If exhausted: release unused medical nodes or increase pool
```

## Log Collection

When filing a bug report or escalating, collect:

```bash
# System overview
lattice admin raft status > diag/raft.txt
lattice nodes -o json > diag/nodes.json
lattice status --all -o json > diag/allocations.json

# Recent scheduler metrics (last hour)
lattice admin metrics dump --component=scheduler --duration=1h > diag/scheduler-metrics.json

# Specific node agent logs (if relevant)
ssh x1000c0s0b0n0 journalctl -u lattice-agent --since="1 hour ago" > diag/agent.log
```

## Cross-References

- [failure-modes.md](failure-modes.md) — Expected failure patterns and recovery
- [node-lifecycle.md](node-lifecycle.md) — Node state transitions and timeouts
- [preemption.md](preemption.md) — Preemption policy and classes
- [autoscaling.md](autoscaling.md) — Scaling loop and error handling
- [data-staging.md](data-staging.md) — Cache management and staging pipeline
- [tuning-guide.md](tuning-guide.md) — Cost function tuning for performance issues

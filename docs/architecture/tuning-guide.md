# Performance Tuning Guide

## Design Principle

Tuning Lattice is primarily about tuning the cost function weights per vCluster. The RM-Replay simulator is the primary tool: capture production traces, replay with different weights, measure outcomes, deploy with confidence.

## Cost Function Sensitivity

### Weight Impact Matrix

Each cost function weight controls a trade-off. Increasing one weight reduces the influence of others:

| Weight Increased | Positive Effect | Negative Effect | When to Increase |
|-----------------|-----------------|-----------------|------------------|
| w₁ (priority) | High-priority jobs scheduled faster | Low-priority jobs starve longer | Many priority levels with strict SLAs |
| w₂ (wait_time) | Better anti-starvation, fairer wait distribution | May schedule low-value jobs before high-value ones | Long tail of wait times |
| w₃ (fair_share) | Tenants get closer to contracted share | May reduce overall utilization (leaving resources idle) | Multi-tenant with strict fairness requirements |
| w₄ (topology) | Better placement, higher network performance | May increase wait time (holding out for ideal placement) | Network-sensitive workloads (NCCL, MPI allreduce) |
| w₅ (data_readiness) | Less I/O stall at job start | May delay jobs whose data isn't pre-staged | Large-dataset workloads |
| w₆ (backlog) | System responds to queue pressure | May destabilize scheduling when queue fluctuates | Bursty submission patterns |
| w₇ (energy) | Lower electricity costs | Jobs may wait for cheap-energy windows | Time-flexible workloads, sites with TOU pricing |
| w₈ (checkpoint) | More flexible resource rebalancing | Overhead from frequent checkpointing | Preemption-heavy environments |
| w₉ (conformance) | Fewer driver-mismatch issues | Fewer candidate nodes (smaller conformance groups) | Multi-node GPU workloads |

### Common Trade-offs

**Throughput vs. Fairness (w₃):**
- Low w₃ (0.05): maximize utilization — schedule whatever fits, regardless of tenant share
- High w₃ (0.35): enforce fairness — tenants below their share get priority even if it means idle resources

Typical compromise: w₃ = 0.15-0.25

**Wait Time vs. Topology (w₂ vs. w₄):**
- High w₂, low w₄: schedule quickly in any topology — reduces wait but may hurt network performance
- Low w₂, high w₄: wait for good topology — increases wait but improves job runtime

Typical for HPC: w₂ = 0.25, w₄ = 0.15
Typical for ML training: w₂ = 0.10, w₄ = 0.30

**Utilization vs. Energy (w₇):**
- w₇ = 0.00: schedule immediately regardless of energy cost (default for most sites)
- w₇ = 0.10-0.15: delay time-flexible jobs to cheap-energy windows

Only relevant for sites with significant time-of-use electricity pricing.

## Using RM-Replay

### Overview

RM-Replay replays production workload traces through the scheduler in simulation mode. No real resources are used. Simulation runs in seconds, not hours.

Reference: Martinasso et al., "RM-Replay: A High-Fidelity Tuning, Optimization and Exploration Tool for Resource Management" (SC18).

### Step 1: Capture Traces

Record workload traces from production (or synthetic workloads):

```bash
# Enable trace capture (writes to S3)
lattice admin config set scheduler.trace_capture=true
lattice admin config set scheduler.trace_path="s3://lattice-traces/"

# Capture for a representative period (1 week recommended)
# Traces include:
#   - Allocation submissions (arrival time, resources, constraints, tenant, priority)
#   - Allocation completions (actual duration, exit status)
#   - Node inventory (capabilities, topology, conformance groups)
```

Trace format is a timestamped event log (JSON lines):
```json
{"ts": "2026-03-01T00:00:01Z", "type": "submit", "alloc": {"nodes": 64, "gpu_type": "GH200", "walltime": "72h", "tenant": "physics", "priority": 4}}
{"ts": "2026-03-01T00:00:05Z", "type": "complete", "alloc_id": "abc-123", "duration": "68h", "exit": 0}
```

### Step 2: Configure Weights

Create weight profiles to compare:

```yaml
# profiles/baseline.yaml (current production weights)
hpc-batch:
  priority: 0.20
  wait_time: 0.25
  fair_share: 0.25
  topology: 0.15
  data_readiness: 0.10
  backlog: 0.05
  energy: 0.00
  checkpoint: 0.00
  conformance: 0.10

# profiles/fairness-boost.yaml (experiment: more fairness)
hpc-batch:
  priority: 0.15
  wait_time: 0.20
  fair_share: 0.35        # increased
  topology: 0.15
  data_readiness: 0.10
  backlog: 0.05
  energy: 0.00
  checkpoint: 0.00
  conformance: 0.10
```

### Step 3: Replay

```bash
# Replay with baseline weights
rm-replay --trace=traces/week-2026-03.jsonl \
          --weights=profiles/baseline.yaml \
          --nodes=inventory/alps.yaml \
          --output=results/baseline/

# Replay with experimental weights
rm-replay --trace=traces/week-2026-03.jsonl \
          --weights=profiles/fairness-boost.yaml \
          --nodes=inventory/alps.yaml \
          --output=results/fairness-boost/
```

### Step 4: Evaluate

RM-Replay produces a summary report:

```
=== RM-Replay Results: fairness-boost ===

Utilization:
  GPU-hours consumed: 1,234,567 / 1,500,000 available (82.3%)
  ↓ 2.1% vs baseline (84.4%)

Wait Time:
  p50: 12 min  (baseline: 10 min)  ↑ 20%
  p95: 2.1 hr  (baseline: 2.5 hr)  ↓ 16%
  p99: 8.3 hr  (baseline: 12.1 hr) ↓ 31%

Fairness (Jain's Index):
  0.94 (baseline: 0.87)  ↑ 8%

Tenant Share Deviation:
  Max deviation: 3.2%  (baseline: 8.7%)  ↓ 63%

Backfill:
  Backfill jobs: 342 (baseline: 367)  ↓ 7%

Preemptions:
  Total: 15 (baseline: 12)  ↑ 25%
```

### Step 5: Decide and Deploy

Compare results across profiles. When satisfied:

```bash
# Deploy new weights (hot-reloadable, no restart)
lattice admin vcluster set-weights --name=hpc-batch \
  --priority=0.15 --wait-time=0.20 --fair-share=0.35 \
  --topology=0.15 --data-readiness=0.10 --backlog=0.05 \
  --energy=0.00 --checkpoint=0.00 --conformance=0.10
```

Weights take effect on the next scheduling cycle.

## Scheduling Cycle Tuning

The scheduling cycle interval affects responsiveness vs. overhead:

| Interval | Effect | Recommended For |
|----------|--------|-----------------|
| 5s | Fast scheduling, higher CPU on scheduler | Interactive vCluster, small clusters |
| 15s | Balanced | HPC batch, ML training |
| 30s | Lower overhead, slower response | Large clusters (5000+ nodes), service vCluster |

```bash
lattice admin vcluster set-config --name=hpc-batch --cycle-interval=15s
```

## Backfill Tuning

Backfill depth controls how many future reservations the solver considers:

| Depth | Effect |
|-------|--------|
| 0 | No backfill (only first-fit) — simple but low utilization |
| 10 | Moderate backfill — good balance |
| 50 | Deep backfill — higher utilization but longer cycle time |

For most sites, depth 10-20 is optimal. Increase if utilization is below target.

## Conformance Group Sizing

If conformance groups are too small (many distinct fingerprints), multi-node jobs have fewer candidate sets:

- **Symptom:** High wait times for multi-node jobs, f₉ scores consistently low
- **Diagnosis:** `lattice nodes -o wide` shows many distinct conformance hashes
- **Fix:** Coordinate with OpenCHAMI to standardize firmware versions. Prioritize GPU driver and NIC firmware alignment.
- **Workaround:** Reduce w₉ for tolerant workloads (services, interactive)

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — Cost function definition, weight profiles
- [testing-strategy.md](testing-strategy.md) — RM-Replay regression suite
- [conformance.md](conformance.md) — Conformance groups and drift
- [telemetry.md](telemetry.md) — Scheduler self-monitoring metrics for observing tuning impact

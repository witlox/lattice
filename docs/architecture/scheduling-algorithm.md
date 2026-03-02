# Scheduling Algorithm

## Overview

Lattice uses a multi-dimensional knapsack formulation with a composite cost function, executed independently by each vCluster scheduler. The quorum provides global coordination.

## The Knapsack Formulation

### Resources (Knapsack Dimensions)

Each scheduling decision must respect multiple resource constraints simultaneously:

| Dimension | Unit | Source |
|---|---|---|
| Nodes | count | Quorum (available nodes owned by or borrowable by vCluster) |
| GPU-hours | nodes × walltime | Derived from allocation request |
| Topology span | group count | Topology model (dragonfly groups consumed) |
| Storage I/O bandwidth | GB/s | VAST API (current utilization + allocation estimate) |
| Power budget | kW | OpenCHAMI BMC telemetry (per-node power draw) |

### Value (Cost Function)

```
Score(j) = Σ wᵢ · fᵢ(j)
```

#### Component Functions

**f₁: priority_class(j)** — Static priority tier (0-10). Medical claims are highest. Preemption only moves down tiers.

**f₂: wait_time_factor(j)** — Anti-starvation. Increases monotonically with time in queue.
```
f₂(j) = log(1 + wait_seconds / reference_wait)
```
`reference_wait` is tunable (default: 1 hour). Log prevents wait time from dominating all other factors.

**f₃: fair_share_deficit(j)** — How far the tenant is from their contracted share. See [quota-enforcement.md](quota-enforcement.md) for hard vs. soft quota semantics.
```
f₃(j) = max(0, target_share(tenant) - actual_usage(tenant)) / target_share(tenant)
```
Ranges from 0 (tenant at or above share) to 1 (tenant has used nothing). Tenants below their share get priority.

**f₄: topology_fitness(j)** — How well the job fits available topology. For intra-node GPU topology, see [gpu-topology.md](gpu-topology.md).
```
f₄(j) = 1.0 - (groups_needed(j) / max_groups_available)
```
Jobs that fit in a single group score highest. Penalty for spanning groups scales with group count.

**f₅: data_readiness(j)** — Is the job's input data on hot tier?
```
f₅(j) = fraction_of_input_data_on_hot_tier(j)
```
If unknown (user didn't specify data requirements), defaults to 0.5 (neutral).

**f₆: backlog_pressure(t)** — Global signal, not per-job. High when queue is deep.
```
f₆(t) = min(1.0, queued_gpu_hours / running_gpu_hours)
```
Capped at 1.0. Affects all jobs equally — it's a system-level urgency signal.

**f₇: energy_cost(j, t)** — Time-varying electricity price at scheduling time.
```
f₇(j, t) = 1.0 - normalized_energy_price(t)
```
Jobs score higher when energy is cheap. In federated mode, extends to `energy_cost(j, t, site)`.

**f₈: checkpoint_efficiency(j)** — How cheaply can this job be preempted?
```
f₈(j) = 1.0 / (1.0 + estimated_checkpoint_minutes(j))
```
Jobs with fast checkpointing are more attractive to schedule on borrowed/preemptible nodes.

**f₉: conformance_fitness(j, candidates)** — How well do the candidate nodes match each other's configuration?
```
f₉(j, candidates) = largest_conformance_group_size(candidates) / j.requested_nodes
```
Scores 1.0 when all candidate nodes share the same conformance fingerprint, lower when the node set is heterogeneous. Critical for multi-node jobs where driver/firmware mismatches cause subtle performance degradation or correctness issues (e.g., NCCL hangs from mismatched NIC firmware).

The conformance fingerprint is a hash of: GPU driver version, NIC firmware version, BIOS/BMC firmware version, and kernel parameters. The node agent computes and reports this fingerprint alongside health data. Nodes with identical fingerprints belong to the same **conformance group**.

This factor is evaluated during node selection (step 2a in the solver), not during scoring. The solver prefers to select nodes from the largest available conformance group that satisfies the allocation's constraints.

See [data-staging.md](data-staging.md) for details on how input data is pre-staged during queue wait to improve f₅ scores. See [preemption.md](preemption.md) for how preemption classes interact with f₁ priority scoring. See [network-domains.md](network-domains.md) for the VNI assignment that enables topology-aware placement (f₄).

#### Weight Profiles

| Weight | HPC Batch | ML Training | Service | Medical | Interactive |
|---|---|---|---|---|---|
| w₁ (priority) | 0.15 | 0.10 | 0.15 | 0.90 | 0.10 |
| w₂ (wait_time) | 0.20 | 0.10 | 0.05 | 0.00 | 0.30 |
| w₃ (fair_share) | 0.20 | 0.10 | 0.10 | 0.00 | 0.10 |
| w₄ (topology) | 0.15 | 0.25 | 0.05 | 0.00 | 0.00 |
| w₅ (data_ready) | 0.10 | 0.15 | 0.10 | 0.00 | 0.05 |
| w₆ (backlog) | 0.05 | 0.05 | 0.05 | 0.00 | 0.15 |
| w₇ (energy) | 0.00 | 0.05 | 0.10 | 0.00 | 0.00 |
| w₈ (checkpoint) | 0.05 | 0.10 | 0.10 | 0.00 | 0.00 |
| w₉ (conformance) | 0.10 | 0.10 | 0.30 | 0.10 | 0.30 |

Medical scheduler is degenerate: priority dominates because node claims are non-negotiable (w₁=0.90). Conformance (w₉=0.10) acts as a tiebreaker among conformant nodes; non-conformant nodes are excluded entirely as a hard constraint at the solver level (step 2a), not via the weight system.

**Note:** The `CostWeights::default()` in `crates/lattice-common/src/types.rs` provides a "balanced HPC" baseline (w₁=0.20, w₂=0.20, w₃=0.20, w₄=0.15, w₅=0.10, w₆=0.05, w₇=0.00, w₈=0.00, w₉=0.10). This is not identical to any named profile in the table above — it is a general-purpose starting point. Each vCluster should have its weights tuned for its workload type, either manually or via RM-Replay simulation.

### Solver

The multi-dimensional knapsack is NP-hard in general. For our scale (tens to hundreds of pending large allocations), a greedy heuristic with backfill is sufficient:

```
Algorithm: GreedyTopologyAwareBackfill

1. Sort pending allocations by Score(j) descending
2. For each allocation j in sorted order:
   a. Find the smallest set of available nodes that satisfies:
      - Node count >= j.requested_nodes
      - All nodes in fewest possible dragonfly groups
      - All nodes in same conformance group (prefer) or fewest groups (fallback)
      - Constraints satisfied (GPU type, features, etc.)
      - Power budget not exceeded
   b. If nodes found: PROPOSE allocation to quorum
   c. If not found: try backfill (can j fit in gaps left by higher-priority reservations?)
3. Collect quorum responses (commit or reject)
4. For rejected proposals: re-queue, will try next cycle

Scheduling cycle: every 5-30 seconds (configurable per vCluster)
```

### DAG Dependencies

DAGs (directed acyclic graphs) are first-class workflow primitives. Individual allocations within a DAG are scored by the knapsack solver like any other allocation — the DAG structure controls when allocations enter the queue, not how they are scored. Root allocations enter immediately; downstream allocations enter when their dependency conditions are satisfied. See [dag-scheduling.md](dag-scheduling.md) for the full DAG lifecycle and dependency conditions.

### Reactive Scaling

Reactive allocations (autoscaling services) start at `min_nodes` and scale based on metric thresholds. Scale-up and scale-down are proposed as node ownership changes through the quorum. The knapsack solver handles each scale proposal as a regular allocation change. See [autoscaling.md](autoscaling.md) for the scaling loop, metrics, and cooldown behavior.

### Elastic Resource Sharing

Nodes can be "borrowed" across vClusters:

```
vCluster A: 200 dedicated nodes, currently using 150
  → 50 idle nodes advertised as "borrowable" to other vClusters

vCluster B: 100 dedicated nodes, needs 120 for a pending job
  → Borrows 20 nodes from vCluster A's idle pool
  → These borrowed nodes have a preemption penalty in the cost function
  → If vCluster A needs them back: checkpoint + reclaim
```

The quorum tracks ownership at two levels:
- **Home vCluster**: permanent assignment (based on tenant contracts)
- **Current vCluster**: who is actually using the node right now

## Checkpoint Cost Model

See [checkpoint-broker.md](checkpoint-broker.md) for the full checkpoint decision framework.

Summary: checkpoint when `Value > Cost`, where value includes recompute_saved + preemptability + backlog_relief, and cost includes write_time + compute_waste + storage_cost. Backlog pressure increases checkpoint aggressiveness.

## Simulation and Tuning

Use RM-Replay (tools/rm-replay/) to test scheduling configurations:

1. Capture production workload traces
2. Configure weight profiles
3. Replay through simulator
4. Evaluate: utilization, wait times, QoS compliance, fairness
5. Iterate on weights before deploying to production

Reference: Martinasso et al., "RM-Replay: A High-Fidelity Tuning, Optimization and Exploration Tool for Resource Management" (SC18).

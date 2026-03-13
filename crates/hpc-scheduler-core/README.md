# hpc-scheduler-core

Core scheduling algorithms for HPC job schedulers, extracted from the [Lattice](https://github.com/witlox/lattice) project.

This crate is **pure synchronous Rust** with no async runtime, no gRPC, and no protobuf dependencies, making it easy to embed in any HPC scheduler.

## Features

- **Composite cost function** — 9-factor scoring (`priority`, `wait_time`, `fair_share`, `topology`, `data_readiness`, `backlog`, `energy`, `checkpoint_efficiency`, `conformance`) with tunable weights
- **Knapsack solver** — greedy topology-aware placement with reservation-based backfill for large jobs
- **Topology-aware placement** — dragonfly group packing with tight/spread/any strategies
- **Conformance grouping** — driver/firmware homogeneity enforcement for multi-node jobs
- **Resource timeline** — tracks future node releases to safely backfill jobs without delaying reservations
- **Preemption** — burst-aware victim selection with checkpoint cost awareness
- **Walltime enforcement** — two-phase SIGTERM/SIGKILL protocol
- **Memory topology** — NUMA, HBM, CXL-aware placement and locality scoring

## Quick start

Add to your `Cargo.toml`:

```toml
[dependencies]
hpc-scheduler-core = "2026.1"
```

### 1. Implement the traits

The crate is generic over your job and node types. Implement [`Job`] and [`ComputeNode`]:

```rust
use chrono::{DateTime, Utc};
use hpc_scheduler_core::{Job, ComputeNode, CheckpointKind, TopologyPreference, NodeConstraints};

struct MyJob {
    id: uuid::Uuid,
    tenant: String,
    nodes: u32,
    walltime_secs: u64,
    priority: u8,
    created: DateTime<Utc>,
    // ...
}

impl Job for MyJob {
    fn id(&self) -> uuid::Uuid { self.id }
    fn tenant_id(&self) -> &str { &self.tenant }
    fn node_count_min(&self) -> u32 { self.nodes }
    fn node_count_max(&self) -> Option<u32> { None } // fixed-size
    fn walltime(&self) -> Option<chrono::Duration> {
        Some(chrono::Duration::seconds(self.walltime_secs as i64))
    }
    fn preemption_class(&self) -> u8 { self.priority }
    fn created_at(&self) -> DateTime<Utc> { self.created }
    fn started_at(&self) -> Option<DateTime<Utc>> { None }
    fn assigned_nodes(&self) -> &[String] { &[] }
    fn checkpoint_kind(&self) -> CheckpointKind { CheckpointKind::Auto }
    fn is_running(&self) -> bool { false }
    fn is_sensitive(&self) -> bool { false }
    fn prefer_same_numa(&self) -> bool { false }
    fn topology_preference(&self) -> Option<TopologyPreference> { None }
    fn constraints(&self) -> NodeConstraints { NodeConstraints::default() }
}

struct MyNode {
    name: String,
    group: u32,
    // ...
}

impl ComputeNode for MyNode {
    fn id(&self) -> &str { &self.name }
    fn group(&self) -> u32 { self.group }
    fn is_available(&self) -> bool { true }
    fn conformance_fingerprint(&self) -> Option<&str> { None }
    fn gpu_type(&self) -> Option<&str> { None }
    fn features(&self) -> &[String] { &[] }
    fn cpu_cores(&self) -> u32 { 64 }
    fn gpu_count(&self) -> u32 { 4 }
    fn memory_topology(&self) -> Option<hpc_scheduler_core::MemoryTopologyInfo> { None }
}
```

### 2. Run a scheduling cycle

```rust
use hpc_scheduler_core::*;
use std::collections::HashMap;

// Build context
let ctx = CostContext {
    tenant_usage: HashMap::new(),
    budget_utilization: HashMap::new(),
    backlog: BacklogMetrics::default(),
    energy_price: 0.5,
    data_readiness: HashMap::new(),
    reference_wait_seconds: 3600.0,
    max_groups: 4,
    now: chrono::Utc::now(),
    memory_locality: HashMap::new(),
    conformance_fitness: HashMap::new(),
};

// Build topology model
let topology = TopologyModel {
    groups: vec![
        TopologyGroup { id: 0, nodes: vec!["node-0".into(), "node-1".into()], adjacent_groups: vec![1] },
        TopologyGroup { id: 1, nodes: vec!["node-2".into(), "node-3".into()], adjacent_groups: vec![0] },
    ],
};

// Empty timeline (no running jobs to track)
let timeline = ResourceTimeline::default();

// Solve
let solver = KnapsackSolver::new(CostWeights::default());
let result = solver.solve(&pending_jobs, &nodes, &topology, &ctx, &timeline);

for decision in &result.decisions {
    match decision {
        PlacementDecision::Place { allocation_id, nodes } => {
            println!("Place job {} on nodes {:?}", allocation_id, nodes);
        }
        PlacementDecision::Defer { allocation_id, reason } => {
            println!("Defer job {}: {}", allocation_id, reason);
        }
        PlacementDecision::Backfill { allocation_id, nodes } => {
            println!("Backfill job {} on nodes {:?}", allocation_id, nodes);
        }
        _ => {}
    }
}
```

## Cost function

The composite score is `Score(j) = Sum(w_i * f_i(j))` where:

| Factor | Description |
|--------|-------------|
| `f1` priority | Preemption tier (higher class = higher score) |
| `f2` wait_time | Anti-starvation, ages with queue time |
| `f3` fair_share | Tenant equity (penalizes over-quota usage) |
| `f4` topology | Dragonfly group packing fitness |
| `f5` data_readiness | Is input data on hot tier? |
| `f6` backlog | Queue depth pressure |
| `f7` energy | Time-varying electricity price |
| `f8` checkpoint | Preemption cost (easier to checkpoint = higher score) |
| `f9` conformance | Node config homogeneity for multi-node jobs |

Weights are fully tunable via `CostWeights`. Use the defaults as a starting point and adjust based on your cluster's priorities.

## Backfill scheduling

The knapsack solver uses a two-pass algorithm:

1. **Pass 1 (greedy)**: Score all pending jobs, assign nodes to the highest-scoring jobs that fit. The highest-priority deferred job gets a *reservation* (future time slot).
2. **Pass 2 (backfill)**: Smaller deferred jobs are placed if they can complete before the reservation's start time, ensuring large jobs are never starved.

The `ResourceTimeline` tracks when running jobs will release nodes, enabling safe backfill decisions.

## Optional features

- **`serde`** — Enables `Serialize`/`Deserialize` on all public types

## License

Apache-2.0

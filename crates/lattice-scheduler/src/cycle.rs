//! Scheduling cycle orchestrator.
//!
//! Runs the main scheduling loop: fetches pending allocations,
//! scores them, runs the solver, and returns placement decisions.

use std::collections::HashMap;

use lattice_common::types::*;

use crate::conformance::memory_locality_score;
use crate::cost::{BacklogMetrics, CostContext};
use crate::knapsack::KnapsackSolver;
use crate::placement::SchedulingResult;
use crate::quota::compute_tenant_usage;

/// Input to a scheduling cycle.
#[derive(Debug, Clone)]
pub struct CycleInput {
    /// Pending allocations to schedule
    pub pending: Vec<Allocation>,
    /// Currently running allocations (for backlog, fair-share)
    pub running: Vec<Allocation>,
    /// All operational nodes
    pub nodes: Vec<Node>,
    /// Known tenants
    pub tenants: Vec<Tenant>,
    /// Topology model
    pub topology: TopologyModel,
    /// Pre-fetched data readiness scores
    pub data_readiness: HashMap<AllocId, f64>,
    /// Normalized energy price (0.0-1.0)
    pub energy_price: f64,
}

/// Run a single scheduling cycle.
///
/// Returns placement decisions for each pending allocation.
pub fn run_cycle(input: &CycleInput, weights: &CostWeights) -> SchedulingResult {
    let total_nodes = input.nodes.len() as u32;

    // Compute tenant usage for fair-share
    let all_allocs: Vec<Allocation> = input
        .running
        .iter()
        .chain(input.pending.iter())
        .cloned()
        .collect();
    let tenant_usage = compute_tenant_usage(&input.tenants, &all_allocs, total_nodes);

    // Compute backlog metrics
    let queued_gpu_hours = compute_gpu_hours(&input.pending);
    let running_gpu_hours = compute_gpu_hours(&input.running).max(1.0);

    // Build cost context
    let ctx = CostContext {
        tenant_usage,
        budget_utilization: std::collections::HashMap::new(),
        backlog: BacklogMetrics {
            queued_gpu_hours,
            running_gpu_hours,
        },
        energy_price: input.energy_price,
        data_readiness: input.data_readiness.clone(),
        reference_wait_seconds: 3600.0,
        max_groups: input.topology.groups.len().max(1) as u32,
        now: chrono::Utc::now(),
        memory_locality: compute_memory_locality(&input.nodes),
    };

    // Run the knapsack solver
    let solver = KnapsackSolver::new(weights.clone());
    solver.solve(&input.pending, &input.nodes, &input.topology, &ctx)
}

/// Compute per-node memory locality scores.
fn compute_memory_locality(nodes: &[Node]) -> HashMap<NodeId, f64> {
    nodes
        .iter()
        .map(|n| (n.id.clone(), memory_locality_score(n)))
        .collect()
}

/// Estimate GPU-hours for a set of allocations.
fn compute_gpu_hours(allocs: &[Allocation]) -> f64 {
    allocs
        .iter()
        .map(|a| {
            let nodes = match a.resources.nodes {
                NodeCount::Exact(n) => n as f64,
                NodeCount::Range { min, .. } => min as f64,
            };
            let hours = match &a.lifecycle.lifecycle_type {
                LifecycleType::Bounded { walltime } => walltime.num_hours() as f64,
                LifecycleType::Unbounded => 24.0, // estimate for unbounded
                LifecycleType::Reactive { min_nodes, .. } => *min_nodes as f64 * 24.0,
            };
            nodes * hours
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{
        create_node_batch, create_test_topology, AllocationBuilder, TenantBuilder,
    };

    #[test]
    fn empty_cycle_returns_empty() {
        let input = CycleInput {
            pending: vec![],
            running: vec![],
            nodes: create_node_batch(4, 0),
            tenants: vec![TenantBuilder::new("t1").build()],
            topology: create_test_topology(1, 4),
            data_readiness: HashMap::new(),
            energy_price: 0.5,
        };

        let result = run_cycle(&input, &CostWeights::default());
        assert!(result.decisions.is_empty());
    }

    #[test]
    fn single_allocation_placed() {
        let alloc = AllocationBuilder::new().tenant("t1").nodes(2).build();

        let input = CycleInput {
            pending: vec![alloc.clone()],
            running: vec![],
            nodes: create_node_batch(4, 0),
            tenants: vec![TenantBuilder::new("t1").build()],
            topology: create_test_topology(1, 4),
            data_readiness: HashMap::new(),
            energy_price: 0.5,
        };

        let result = run_cycle(&input, &CostWeights::default());
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.placed()[0].allocation_id(), alloc.id);
    }

    #[test]
    fn multiple_allocations_priority_order() {
        let high = AllocationBuilder::new()
            .tenant("t1")
            .nodes(2)
            .preemption_class(8)
            .build();
        let low = AllocationBuilder::new()
            .tenant("t1")
            .nodes(2)
            .preemption_class(1)
            .build();

        let input = CycleInput {
            pending: vec![low.clone(), high.clone()],
            running: vec![],
            nodes: create_node_batch(2, 0),
            tenants: vec![TenantBuilder::new("t1").build()],
            topology: create_test_topology(1, 2),
            data_readiness: HashMap::new(),
            energy_price: 0.5,
        };

        let weights = CostWeights {
            priority: 1.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        };

        let result = run_cycle(&input, &weights);
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.placed()[0].allocation_id(), high.id);
    }

    #[test]
    fn deferred_when_no_resources() {
        let alloc = AllocationBuilder::new().tenant("t1").nodes(10).build();

        let input = CycleInput {
            pending: vec![alloc],
            running: vec![],
            nodes: create_node_batch(4, 0),
            tenants: vec![TenantBuilder::new("t1").build()],
            topology: create_test_topology(1, 4),
            data_readiness: HashMap::new(),
            energy_price: 0.5,
        };

        let result = run_cycle(&input, &CostWeights::default());
        assert_eq!(result.deferred().len(), 1);
    }

    #[test]
    fn gpu_hours_calculation() {
        let a1 = AllocationBuilder::new()
            .nodes(4)
            .lifecycle_bounded(2)
            .build();
        let a2 = AllocationBuilder::new()
            .nodes(2)
            .lifecycle_bounded(8)
            .build();

        let hours = compute_gpu_hours(&[a1, a2]);
        // 4*2 + 2*8 = 8 + 16 = 24
        assert!((hours - 24.0).abs() < f64::EPSILON);
    }
}

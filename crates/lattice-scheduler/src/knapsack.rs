//! Greedy topology-aware backfill knapsack solver.
//!
//! Implements the `GreedyTopologyAwareBackfill` algorithm from
//! docs/architecture/scheduling-algorithm.md.

use std::collections::HashSet;

use lattice_common::types::*;

use crate::conformance::{filter_by_constraints, select_conformant_nodes};
use crate::cost::{CostContext, CostEvaluator};
use crate::placement::{PlacementDecision, SchedulingResult};
use crate::topology::select_nodes_topology_aware;

/// The greedy knapsack solver.
pub struct KnapsackSolver {
    evaluator: CostEvaluator,
}

impl KnapsackSolver {
    pub fn new(weights: CostWeights) -> Self {
        Self {
            evaluator: CostEvaluator::new(weights),
        }
    }

    /// Run a scheduling cycle: score pending allocations, assign nodes greedily.
    pub fn solve(
        &self,
        pending: &[Allocation],
        available_nodes: &[Node],
        topology: &TopologyModel,
        ctx: &CostContext,
    ) -> SchedulingResult {
        if pending.is_empty() || available_nodes.is_empty() {
            return SchedulingResult::default();
        }

        // 1. Score and sort pending allocations descending
        let mut scored: Vec<(&Allocation, f64)> = pending
            .iter()
            .map(|a| (a, self.evaluator.score(a, ctx)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Track which nodes are still available
        let mut used_nodes: HashSet<NodeId> = HashSet::new();
        let mut decisions = Vec::new();

        // 2. Greedy assignment
        for (alloc, _score) in &scored {
            let requested = match alloc.resources.nodes {
                NodeCount::Exact(n) => n,
                NodeCount::Range { min, .. } => min,
            };

            // Filter: only operational, unused nodes matching constraints
            let candidates: Vec<&Node> = available_nodes
                .iter()
                .filter(|n| !used_nodes.contains(&n.id))
                .filter(|n| n.state.is_operational())
                .filter(|n| n.owner.is_none())
                .collect();

            let constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

            if (constrained.len() as u32) < requested {
                decisions.push(PlacementDecision::Defer {
                    allocation_id: alloc.id,
                    reason: format!(
                        "insufficient nodes: need {}, available {}",
                        requested,
                        constrained.len()
                    ),
                });
                continue;
            }

            // 2a. Try conformance-aware selection first
            let selected = select_conformant_nodes(requested, &constrained).or_else(|| {
                // Fallback: topology-aware selection without strict conformance
                select_nodes_topology_aware(
                    requested,
                    alloc.resources.constraints.topology.as_ref(),
                    &constrained,
                    topology,
                )
            });

            match selected {
                Some(nodes) => {
                    for n in &nodes {
                        used_nodes.insert(n.clone());
                    }
                    decisions.push(PlacementDecision::Place {
                        allocation_id: alloc.id,
                        nodes,
                    });
                }
                None => {
                    decisions.push(PlacementDecision::Defer {
                        allocation_id: alloc.id,
                        reason: "no suitable node set found".into(),
                    });
                }
            }
        }

        SchedulingResult { decisions }
    }

    /// Get the underlying cost evaluator (for testing/inspection).
    pub fn evaluator(&self) -> &CostEvaluator {
        &self.evaluator
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{
        create_node_batch, create_test_topology, AllocationBuilder, NodeBuilder,
    };

    fn default_ctx() -> CostContext {
        CostContext::default()
    }

    #[test]
    fn empty_input_returns_empty_result() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let topology = create_test_topology(1, 4);
        let result = solver.solve(&[], &[], &topology, &default_ctx());
        assert!(result.decisions.is_empty());
    }

    #[test]
    fn single_allocation_single_node() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let result = solver.solve(
            std::slice::from_ref(&alloc),
            &nodes,
            &topology,
            &default_ctx(),
        );
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.placed()[0].allocation_id(), alloc.id);
    }

    #[test]
    fn multiple_allocations_fit() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let a1 = AllocationBuilder::new().nodes(2).build();
        let a2 = AllocationBuilder::new().nodes(2).build();
        let nodes = create_node_batch(8, 0);
        let topology = create_test_topology(1, 8);

        let result = solver.solve(&[a1, a2], &nodes, &topology, &default_ctx());
        assert_eq!(result.placed().len(), 2);
    }

    #[test]
    fn allocation_deferred_when_not_enough_nodes() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(10).build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let result = solver.solve(&[alloc], &nodes, &topology, &default_ctx());
        assert_eq!(result.deferred().len(), 1);
    }

    #[test]
    fn higher_priority_scheduled_first() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        });
        // Only 2 nodes available; both jobs need 2
        let high = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(8)
            .build();
        let low = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(1)
            .build();
        let nodes = create_node_batch(2, 0);
        let topology = create_test_topology(1, 2);

        let result = solver.solve(
            &[low.clone(), high.clone()],
            &nodes,
            &topology,
            &default_ctx(),
        );
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.placed()[0].allocation_id(), high.id);
        assert_eq!(result.deferred().len(), 1);
        assert_eq!(result.deferred()[0].allocation_id(), low.id);
    }

    #[test]
    fn nodes_not_double_assigned() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let a1 = AllocationBuilder::new().nodes(3).build();
        let a2 = AllocationBuilder::new().nodes(3).build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let result = solver.solve(&[a1, a2], &nodes, &topology, &default_ctx());
        // Only room for one (3 + 3 > 4)
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.deferred().len(), 1);
    }

    #[test]
    fn owned_nodes_not_assigned() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(2).build();

        let n1 = NodeBuilder::new().id("n1").group(0).build();
        let n2 = NodeBuilder::new()
            .id("n2")
            .group(0)
            .owner(NodeOwnership {
                tenant: "other".into(),
                vcluster: "vc".into(),
                allocation: uuid::Uuid::new_v4(),
                claimed_by: None,
                is_borrowed: false,
            })
            .build();
        let n3 = NodeBuilder::new().id("n3").group(0).build();
        let nodes = vec![n1, n2, n3];
        let topology = create_test_topology(1, 3);

        let result = solver.solve(&[alloc], &nodes, &topology, &default_ctx());
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
            // n2 is owned, so should not be in the assignment
            assert!(!nodes.contains(&"n2".to_string()));
            assert_eq!(nodes.len(), 2);
        } else {
            panic!("Expected Place decision");
        }
    }

    #[test]
    fn gpu_type_constraint_filters_nodes() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let mut alloc = AllocationBuilder::new().nodes(2).build();
        alloc.resources.constraints.gpu_type = Some("MI300X".into());

        let n1 = NodeBuilder::new()
            .id("n1")
            .group(0)
            .gpu_type("GH200")
            .build();
        let n2 = NodeBuilder::new()
            .id("n2")
            .group(0)
            .gpu_type("MI300X")
            .build();
        let n3 = NodeBuilder::new()
            .id("n3")
            .group(0)
            .gpu_type("MI300X")
            .build();
        let nodes = vec![n1, n2, n3];
        let topology = create_test_topology(1, 3);

        let result = solver.solve(&[alloc], &nodes, &topology, &default_ctx());
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
            assert!(!nodes.contains(&"n1".to_string()));
        }
    }

    #[test]
    fn non_operational_nodes_skipped() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(2).build();

        let n1 = NodeBuilder::new().id("n1").group(0).build(); // Ready
        let n2 = NodeBuilder::new()
            .id("n2")
            .group(0)
            .state(NodeState::Down {
                reason: "failed".into(),
            })
            .build();
        let n3 = NodeBuilder::new().id("n3").group(0).build(); // Ready
        let nodes = vec![n1, n2, n3];
        let topology = create_test_topology(1, 3);

        let result = solver.solve(&[alloc], &nodes, &topology, &default_ctx());
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
            assert!(!nodes.contains(&"n2".to_string()));
        }
    }
}

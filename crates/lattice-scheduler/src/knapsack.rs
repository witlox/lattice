//! Greedy topology-aware backfill knapsack solver.
//!
//! Delegates to hpc-scheduler-core for the scheduling logic, with a thin
//! wrapper that converts Lattice types to core types.

use lattice_common::scheduler_core_impls::{to_core_topology, to_core_weights};
use lattice_common::types::*;

use crate::cost::{CostContext, CostEvaluator};
use crate::placement::SchedulingResult;
use crate::resource_timeline::ResourceTimeline;

/// The greedy knapsack solver.
///
/// Wraps `hpc_scheduler_core::knapsack::KnapsackSolver` to accept Lattice types.
pub struct KnapsackSolver {
    inner: hpc_scheduler_core::knapsack::KnapsackSolver,
    evaluator: CostEvaluator,
}

impl KnapsackSolver {
    pub fn new(weights: CostWeights) -> Self {
        let core_weights = to_core_weights(&weights);
        Self {
            inner: hpc_scheduler_core::knapsack::KnapsackSolver::new(core_weights),
            evaluator: CostEvaluator::new(weights),
        }
    }

    /// Run a scheduling cycle: score pending allocations, assign nodes greedily,
    /// then attempt reservation-based backfill for deferred candidates.
    pub fn solve(
        &self,
        pending: &[Allocation],
        available_nodes: &[Node],
        topology: &TopologyModel,
        ctx: &CostContext,
        timeline: &ResourceTimeline,
    ) -> SchedulingResult {
        let core_topology = to_core_topology(topology);
        self.inner
            .solve(pending, available_nodes, &core_topology, ctx, timeline)
    }

    /// Get the underlying cost evaluator (for testing/inspection).
    pub fn evaluator(&self) -> &CostEvaluator {
        &self.evaluator
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::PlacementDecision;
    use crate::resource_timeline::ReleaseEvent;
    use chrono::Utc;
    use lattice_test_harness::fixtures::{
        create_node_batch, create_test_topology, AllocationBuilder, NodeBuilder,
    };

    fn default_ctx() -> CostContext {
        CostContext::default()
    }

    fn empty_timeline() -> ResourceTimeline {
        ResourceTimeline { events: vec![] }
    }

    #[test]
    fn empty_input_returns_empty_result() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let topology = create_test_topology(1, 4);
        let result = solver.solve(&[], &[], &topology, &default_ctx(), &empty_timeline());
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
            &empty_timeline(),
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

        let result = solver.solve(
            &[a1, a2],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 2);
    }

    #[test]
    fn allocation_deferred_when_not_enough_nodes() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(10).build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let result = solver.solve(
            &[alloc],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
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
            &empty_timeline(),
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

        let result = solver.solve(
            &[a1, a2],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
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

        let result = solver.solve(
            &[alloc],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
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

        let result = solver.solve(
            &[alloc],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
            assert!(!nodes.contains(&"n1".to_string()));
        }
    }

    #[test]
    fn non_operational_nodes_skipped() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(2).build();

        let n1 = NodeBuilder::new().id("n1").group(0).build();
        let n2 = NodeBuilder::new()
            .id("n2")
            .group(0)
            .state(NodeState::Down {
                reason: "failed".into(),
            })
            .build();
        let n3 = NodeBuilder::new().id("n3").group(0).build();
        let nodes = vec![n1, n2, n3];
        let topology = create_test_topology(1, 3);

        let result = solver.solve(
            &[alloc],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
            assert!(!nodes.contains(&"n2".to_string()));
        }
    }

    // ── Reservation-based backfill tests ──

    #[test]
    fn no_running_bounded_graceful_noop() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let large = AllocationBuilder::new().nodes(6).build();
        let small = AllocationBuilder::new().nodes(2).build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let result = solver.solve(
            &[large, small],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.deferred().len(), 1);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }

    #[test]
    fn timeline_events_do_not_break_greedy() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let now = Utc::now();

        let large = AllocationBuilder::new()
            .nodes(4)
            .preemption_class(8)
            .lifecycle_bounded(4)
            .build();
        let small = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(1)
            .lifecycle_bounded(1)
            .build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let release_time = now + chrono::Duration::hours(2);
        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: release_time,
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec![nodes[0].id.clone(), nodes[1].id.clone()],
            }],
        };

        let result = solver.solve(
            &[large.clone(), small.clone()],
            &nodes[2..],
            &topology,
            &default_ctx(),
            &timeline,
        );

        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.placed()[0].allocation_id(), small.id);
        assert_eq!(result.deferred().len(), 1);
        assert_eq!(result.deferred()[0].allocation_id(), large.id);
    }

    #[test]
    fn unbounded_jobs_never_backfill() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let now = Utc::now();

        let large = AllocationBuilder::new()
            .nodes(4)
            .preemption_class(8)
            .lifecycle_bounded(4)
            .build();
        let service = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(1)
            .lifecycle_unbounded()
            .build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: now + chrono::Duration::hours(2),
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec![nodes[0].id.clone(), nodes[1].id.clone()],
            }],
        };

        let result = solver.solve(
            &[large, service.clone()],
            &nodes[2..],
            &topology,
            &default_ctx(),
            &timeline,
        );

        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.placed()[0].allocation_id(), service.id);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }

    #[test]
    fn reservation_holder_not_backfilled() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let now = Utc::now();

        let large = AllocationBuilder::new()
            .nodes(6)
            .preemption_class(8)
            .lifecycle_bounded(4)
            .build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: now + chrono::Duration::hours(2),
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["extra1".into(), "extra2".into()],
            }],
        };

        let result = solver.solve(
            std::slice::from_ref(&large),
            &nodes,
            &topology,
            &default_ctx(),
            &timeline,
        );

        assert_eq!(result.deferred().len(), 1);
        assert_eq!(result.deferred()[0].allocation_id(), large.id);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }

    #[test]
    fn greedy_with_timeline_places_same_as_without() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let a1 = AllocationBuilder::new().nodes(2).build();
        let a2 = AllocationBuilder::new().nodes(2).build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);
        let now = Utc::now();

        let result_no_timeline = solver.solve(
            &[a1.clone(), a2.clone()],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );

        let timeline_with_events = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: now + chrono::Duration::hours(1),
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["future1".into(), "future2".into()],
            }],
        };

        let result_with_timeline = solver.solve(
            &[a1, a2],
            &nodes,
            &topology,
            &default_ctx(),
            &timeline_with_events,
        );

        assert_eq!(
            result_no_timeline.placed().len(),
            result_with_timeline.placed().len()
        );
        assert_eq!(
            result_no_timeline.deferred().len(),
            result_with_timeline.deferred().len()
        );
    }

    #[test]
    fn all_deferred_with_large_gap_no_backfill() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let now = Utc::now();

        let large = AllocationBuilder::new()
            .nodes(8)
            .preemption_class(8)
            .lifecycle_bounded(4)
            .build();
        let medium = AllocationBuilder::new()
            .nodes(6)
            .preemption_class(3)
            .lifecycle_bounded(1)
            .build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: now + chrono::Duration::hours(2),
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["r1".into(), "r2".into(), "r3".into(), "r4".into()],
            }],
        };

        let result = solver.solve(
            &[large, medium],
            &nodes,
            &topology,
            &default_ctx(),
            &timeline,
        );

        assert_eq!(result.deferred().len(), 2);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }
}

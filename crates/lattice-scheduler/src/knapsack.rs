//! Greedy topology-aware backfill knapsack solver.
//!
//! Implements the `GreedyTopologyAwareBackfill` algorithm from
//! docs/architecture/scheduling-algorithm.md, extended with
//! reservation-based backfill via a resource timeline.

use std::collections::HashSet;

use chrono::Utc;
use lattice_common::types::*;

use crate::conformance::{filter_by_constraints, select_conformant_nodes};
use crate::cost::{CostContext, CostEvaluator};
use crate::placement::{PlacementDecision, SchedulingResult};
use crate::resource_timeline::ResourceTimeline;
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
        let mut deferred_candidates: Vec<&Allocation> = Vec::new();

        // 2. Pass 1 — Primary placement (greedy assignment)
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
                deferred_candidates.push(alloc);
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
                    deferred_candidates.push(alloc);
                }
            }
        }

        // 3. Reservation: highest-priority deferred candidate gets a reservation
        let now = Utc::now();
        let free_count = available_nodes
            .iter()
            .filter(|n| !used_nodes.contains(&n.id))
            .filter(|n| n.state.is_operational())
            .filter(|n| n.owner.is_none())
            .count() as u32;

        let reservation = if let Some(holder) = deferred_candidates.first() {
            let needed = match holder.resources.nodes {
                NodeCount::Exact(n) => n,
                NodeCount::Range { min, .. } => min,
            };
            timeline
                .earliest_start(needed, free_count, |_| true)
                .map(|time| (holder.id, time))
        } else {
            None
        };

        // 4. Pass 2 — Backfill: remaining deferred candidates (excluding reservation holder)
        if let Some((reservation_holder_id, reservation_time)) = reservation {
            let backfill_candidates: Vec<&Allocation> = deferred_candidates
                .iter()
                .filter(|a| a.id != reservation_holder_id)
                .copied()
                .collect();

            for alloc in &backfill_candidates {
                // Only bounded allocations with walltime can backfill
                let walltime = match &alloc.lifecycle.lifecycle_type {
                    LifecycleType::Bounded { walltime } => *walltime,
                    _ => continue,
                };

                if !ResourceTimeline::is_backfill_safe(now, walltime, reservation_time) {
                    continue;
                }

                let requested = match alloc.resources.nodes {
                    NodeCount::Exact(n) => n,
                    NodeCount::Range { min, .. } => min,
                };

                let candidates: Vec<&Node> = available_nodes
                    .iter()
                    .filter(|n| !used_nodes.contains(&n.id))
                    .filter(|n| n.state.is_operational())
                    .filter(|n| n.owner.is_none())
                    .collect();

                let constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

                if (constrained.len() as u32) < requested {
                    continue;
                }

                let selected = select_conformant_nodes(requested, &constrained).or_else(|| {
                    select_nodes_topology_aware(
                        requested,
                        alloc.resources.constraints.topology.as_ref(),
                        &constrained,
                        topology,
                    )
                });

                if let Some(nodes) = selected {
                    for n in &nodes {
                        used_nodes.insert(n.clone());
                    }
                    decisions.push(PlacementDecision::Backfill {
                        allocation_id: alloc.id,
                        nodes,
                        reservation_holder: reservation_holder_id,
                        must_complete_by: reservation_time,
                    });
                }
            }
        }

        // 5. Emit Defer decisions for everything not placed or backfilled
        let placed_ids: HashSet<AllocId> = decisions.iter().map(|d| d.allocation_id()).collect();
        for alloc in &deferred_candidates {
            if !placed_ids.contains(&alloc.id) {
                decisions.push(PlacementDecision::Defer {
                    allocation_id: alloc.id,
                    reason: "insufficient nodes or no suitable node set found".into(),
                });
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
    use crate::resource_timeline::ReleaseEvent;
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

        let result = solver.solve(
            &[alloc],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );
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

    // ─── Reservation-based backfill tests ────────────────────────

    #[test]
    fn no_running_bounded_graceful_noop() {
        // No timeline events → no reservation → pure greedy (same as before)
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
        // small fits in pass 1, large deferred, no backfills
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.deferred().len(), 1);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }

    #[test]
    fn timeline_events_do_not_break_greedy() {
        // Timeline has events but small jobs still get placed in pass 1 (greedy).
        // Large is deferred (reservation holder). No backfills within a single cycle
        // because pass 1 already places everything that fits.
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

        // Large deferred (2 free < 4 needed), small placed via greedy pass 1
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

        // Service fits on 2 free nodes → placed in pass 1
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
    fn no_reservation_when_all_unbounded_running() {
        // Timeline has no events (all running are unbounded) → no reservation possible
        let solver = KnapsackSolver::new(CostWeights::default());
        let large = AllocationBuilder::new().nodes(6).build();
        let small = AllocationBuilder::new()
            .nodes(2)
            .lifecycle_bounded(1)
            .build();
        let nodes = create_node_batch(4, 0);
        let topology = create_test_topology(1, 4);

        let result = solver.solve(
            &[large, small],
            &nodes,
            &topology,
            &default_ctx(),
            &empty_timeline(),
        );

        // small placed (greedy), large deferred, no backfills
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.deferred().len(), 1);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }

    #[test]
    fn reservation_holder_not_backfilled() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let now = Utc::now();

        // Large needs 6 (deferred), it's the only deferred candidate → reservation holder
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

        // Large is deferred (reservation holder), never backfilled
        assert_eq!(result.deferred().len(), 1);
        assert_eq!(result.deferred()[0].allocation_id(), large.id);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }

    #[test]
    fn greedy_with_timeline_places_same_as_without() {
        // Verify that adding a timeline doesn't change greedy placement behavior
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
        // All jobs need more nodes than available → all deferred.
        // Reservation is set for highest priority, but remaining deferred jobs
        // also can't fit on the available nodes.
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

        // Both deferred — large is reservation holder, medium still can't fit (6 > 4 free)
        assert_eq!(result.deferred().len(), 2);
        assert!(!result.decisions.iter().any(|d| d.is_backfill()));
    }
}

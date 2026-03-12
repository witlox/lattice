//! Greedy topology-aware backfill knapsack solver.
//!
//! Implements a multi-dimensional knapsack algorithm with:
//! - Composite cost function scoring
//! - Conformance-aware and topology-aware node selection
//! - Reservation-based backfill via a resource timeline

use std::collections::HashSet;

use chrono::Utc;

use crate::conformance::{filter_by_constraints, select_conformant_nodes};
use crate::cost::{CostContext, CostEvaluator};
use crate::placement::{PlacementDecision, SchedulingResult};
use crate::resource_timeline::ResourceTimeline;
use crate::topology::select_nodes_topology_aware;
use crate::traits::{ComputeNode, Job};
use crate::types::{CostWeights, TopologyModel};

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

    /// Run a scheduling cycle: score pending jobs, assign nodes greedily,
    /// then attempt reservation-based backfill for deferred candidates.
    pub fn solve<J: Job, N: ComputeNode>(
        &self,
        pending: &[J],
        available_nodes: &[N],
        topology: &TopologyModel,
        ctx: &CostContext,
        timeline: &ResourceTimeline,
    ) -> SchedulingResult {
        if pending.is_empty() || available_nodes.is_empty() {
            return SchedulingResult::default();
        }

        // 1. Score and sort pending jobs descending
        let mut scored: Vec<(&J, f64)> = pending
            .iter()
            .map(|j| (j, self.evaluator.score(j, ctx)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut used_nodes: HashSet<String> = HashSet::new();
        let mut decisions = Vec::new();
        let mut deferred_candidates: Vec<&J> = Vec::new();

        // 2. Pass 1 — Primary placement (greedy assignment)
        for (job, _score) in &scored {
            let min_nodes = job.node_count_min();
            let max_nodes = job.node_count_max().unwrap_or(min_nodes);

            let candidates: Vec<&N> = available_nodes
                .iter()
                .filter(|n| !used_nodes.contains(n.id()))
                .filter(|n| n.is_available())
                .collect();

            let constraints = job.constraints();
            let constrained = filter_by_constraints(&candidates, &constraints);

            if (constrained.len() as u32) < min_nodes {
                deferred_candidates.push(job);
                continue;
            }

            // For moldable jobs, try to use as many nodes as possible (up to max)
            let requested = (constrained.len() as u32).min(max_nodes).max(min_nodes);

            // Sensitive allocations require hard conformance — no topology fallback.
            let selected = if job.is_sensitive() {
                select_conformant_nodes(requested, &constrained)
            } else {
                let topo_pref = job.topology_preference();
                select_conformant_nodes(requested, &constrained).or_else(|| {
                    select_nodes_topology_aware(
                        requested,
                        topo_pref.as_ref(),
                        &constrained,
                        topology,
                    )
                })
            };

            match selected {
                Some(nodes) => {
                    for n in &nodes {
                        used_nodes.insert(n.clone());
                    }
                    decisions.push(PlacementDecision::Place {
                        allocation_id: job.id(),
                        nodes,
                    });
                }
                None => {
                    deferred_candidates.push(job);
                }
            }
        }

        // 3. Reservation: highest-priority deferred candidate gets a reservation
        let now = Utc::now();
        let free_count = available_nodes
            .iter()
            .filter(|n| !used_nodes.contains(n.id()))
            .filter(|n| n.is_available())
            .count() as u32;

        let reservation = if let Some(holder) = deferred_candidates.first() {
            let needed = holder.node_count_min();
            timeline
                .earliest_start(needed, free_count, |_| true)
                .map(|time| (holder.id(), time))
        } else {
            None
        };

        // 4. Pass 2 — Backfill
        if let Some((reservation_holder_id, reservation_time)) = reservation {
            let backfill_candidates: Vec<&J> = deferred_candidates
                .iter()
                .filter(|j| j.id() != reservation_holder_id)
                .copied()
                .collect();

            for job in &backfill_candidates {
                let walltime = match job.walltime() {
                    Some(w) => w,
                    None => continue,
                };

                if !ResourceTimeline::is_backfill_safe(now, walltime, reservation_time) {
                    continue;
                }

                let bf_min_nodes = job.node_count_min();
                let bf_max_nodes = job.node_count_max().unwrap_or(bf_min_nodes);

                let candidates: Vec<&N> = available_nodes
                    .iter()
                    .filter(|n| !used_nodes.contains(n.id()))
                    .filter(|n| n.is_available())
                    .collect();

                let constraints = job.constraints();
                let constrained = filter_by_constraints(&candidates, &constraints);

                if (constrained.len() as u32) < bf_min_nodes {
                    continue;
                }

                let requested = (constrained.len() as u32)
                    .min(bf_max_nodes)
                    .max(bf_min_nodes);

                let selected = if job.is_sensitive() {
                    select_conformant_nodes(requested, &constrained)
                } else {
                    let topo_pref = job.topology_preference();
                    select_conformant_nodes(requested, &constrained).or_else(|| {
                        select_nodes_topology_aware(
                            requested,
                            topo_pref.as_ref(),
                            &constrained,
                            topology,
                        )
                    })
                };

                if let Some(nodes) = selected {
                    for n in &nodes {
                        used_nodes.insert(n.clone());
                    }
                    decisions.push(PlacementDecision::Backfill {
                        allocation_id: job.id(),
                        nodes,
                        reservation_holder: reservation_holder_id,
                        must_complete_by: reservation_time,
                    });
                }
            }
        }

        // 5. Emit Defer decisions for everything not placed or backfilled
        let placed_ids: HashSet<uuid::Uuid> = decisions.iter().map(|d| d.allocation_id()).collect();
        for job in &deferred_candidates {
            if !placed_ids.contains(&job.id()) {
                decisions.push(PlacementDecision::Defer {
                    allocation_id: job.id(),
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
    use crate::types::{CheckpointKind, NodeConstraints, TopologyGroup, TopologyPreference};
    use chrono::{DateTime, Utc};

    struct TestJob {
        id: uuid::Uuid,
        tenant: String,
        nodes_min: u32,
        preemption_class: u8,
        walltime: Option<chrono::Duration>,
        constraints: NodeConstraints,
        sensitive: bool,
    }

    impl Default for TestJob {
        fn default() -> Self {
            Self {
                id: uuid::Uuid::new_v4(),
                tenant: "t1".into(),
                nodes_min: 1,
                preemption_class: 5,
                walltime: Some(chrono::Duration::hours(1)),
                constraints: NodeConstraints::default(),
                sensitive: false,
            }
        }
    }

    impl Job for TestJob {
        fn id(&self) -> uuid::Uuid {
            self.id
        }
        fn tenant_id(&self) -> &str {
            &self.tenant
        }
        fn node_count_min(&self) -> u32 {
            self.nodes_min
        }
        fn node_count_max(&self) -> Option<u32> {
            None
        }
        fn walltime(&self) -> Option<chrono::Duration> {
            self.walltime
        }
        fn preemption_class(&self) -> u8 {
            self.preemption_class
        }
        fn created_at(&self) -> DateTime<Utc> {
            Utc::now()
        }
        fn started_at(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn assigned_nodes(&self) -> &[String] {
            &[]
        }
        fn checkpoint_kind(&self) -> CheckpointKind {
            CheckpointKind::Auto
        }
        fn is_running(&self) -> bool {
            false
        }
        fn is_sensitive(&self) -> bool {
            self.sensitive
        }
        fn prefer_same_numa(&self) -> bool {
            false
        }
        fn topology_preference(&self) -> Option<TopologyPreference> {
            None
        }
        fn constraints(&self) -> NodeConstraints {
            self.constraints.clone()
        }
    }

    struct TestNode {
        id: String,
        group: u32,
        available: bool,
        conformance: Option<String>,
    }

    impl ComputeNode for TestNode {
        fn id(&self) -> &str {
            &self.id
        }
        fn group(&self) -> u32 {
            self.group
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn conformance_fingerprint(&self) -> Option<&str> {
            self.conformance.as_deref()
        }
        fn gpu_type(&self) -> Option<&str> {
            None
        }
        fn features(&self) -> &[String] {
            &[]
        }
        fn cpu_cores(&self) -> u32 {
            4
        }
        fn gpu_count(&self) -> u32 {
            0
        }
        fn memory_topology(&self) -> Option<crate::types::MemoryTopologyInfo> {
            None
        }
    }

    fn make_nodes(count: usize, group: u32) -> Vec<TestNode> {
        (0..count)
            .map(|i| TestNode {
                id: format!("g{group}n{i}"),
                group,
                available: true,
                conformance: None,
            })
            .collect()
    }

    fn make_topology(num_groups: u32, nodes_per_group: usize) -> TopologyModel {
        let groups = (0..num_groups)
            .map(|g| {
                let adj: Vec<u32> = (0..num_groups).filter(|&x| x != g).collect();
                TopologyGroup {
                    id: g,
                    nodes: (0..nodes_per_group).map(|i| format!("g{g}n{i}")).collect(),
                    adjacent_groups: adj,
                }
            })
            .collect();
        TopologyModel { groups }
    }

    fn empty_timeline() -> ResourceTimeline {
        ResourceTimeline { events: vec![] }
    }

    #[test]
    fn empty_input_returns_empty_result() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let topology = make_topology(1, 4);
        let empty_jobs: Vec<TestJob> = vec![];
        let empty_nodes: Vec<TestNode> = vec![];
        let result = solver.solve(
            &empty_jobs,
            &empty_nodes,
            &topology,
            &CostContext::default(),
            &empty_timeline(),
        );
        assert!(result.decisions.is_empty());
    }

    #[test]
    fn single_job_single_node() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let job = TestJob {
            nodes_min: 1,
            ..Default::default()
        };
        let nodes = make_nodes(4, 0);
        let topology = make_topology(1, 4);

        let result = solver.solve(
            &[job],
            &nodes,
            &topology,
            &CostContext::default(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
    }

    #[test]
    fn job_deferred_when_not_enough_nodes() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let job = TestJob {
            nodes_min: 10,
            ..Default::default()
        };
        let nodes = make_nodes(4, 0);
        let topology = make_topology(1, 4);

        let result = solver.solve(
            &[job],
            &nodes,
            &topology,
            &CostContext::default(),
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
        let high = TestJob {
            nodes_min: 2,
            preemption_class: 8,
            ..Default::default()
        };
        let low = TestJob {
            nodes_min: 2,
            preemption_class: 1,
            ..Default::default()
        };
        let nodes = make_nodes(2, 0);
        let topology = make_topology(1, 2);

        let result = solver.solve(
            &[low, high],
            &nodes,
            &topology,
            &CostContext::default(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.deferred().len(), 1);
    }

    // ─── Backfill tests ───────────────────────────────────────────
    //
    // Backfill requires: (1) deferred jobs exist, (2) timeline provides a reservation,
    // (3) free nodes available for smaller jobs. We use conformance groups to create
    // scenarios where jobs are deferred despite free nodes (no single conformance
    // group is large enough).

    fn timeline_with_release(hours_from_now: i64, nodes: Vec<&str>) -> ResourceTimeline {
        let release_at = Utc::now() + chrono::Duration::hours(hours_from_now);
        ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at,
                allocation_id: uuid::Uuid::new_v4(),
                nodes: nodes.into_iter().map(String::from).collect(),
            }],
        }
    }

    /// Create nodes split across two conformance groups (fp-a and fp-b),
    /// so `select_conformant_nodes(n)` fails when n > group_size.
    fn make_mixed_conformance_nodes(per_group: usize) -> Vec<TestNode> {
        let mut nodes = Vec::new();
        for i in 0..per_group {
            nodes.push(TestNode {
                id: format!("a{i}"),
                group: 0,
                available: true,
                conformance: Some("fp-a".into()),
            });
        }
        for i in 0..per_group {
            nodes.push(TestNode {
                id: format!("b{i}"),
                group: 0,
                available: true,
                conformance: Some("fp-b".into()),
            });
        }
        nodes
    }

    #[test]
    fn backfill_sensitive_job_with_conformance() {
        // Sensitive jobs skip topology fallback → deferred when conformance fails.
        // This creates backfill candidates with free nodes available.
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        // 4 nodes: 2 fp-a, 2 fp-b. Max conformance group = 2.
        let nodes = make_mixed_conformance_nodes(2);
        let topology = make_topology(1, 4);

        // Large sensitive job needs 3 conformant → deferred (max group=2)
        let large = TestJob {
            nodes_min: 3,
            preemption_class: 8,
            sensitive: true,
            walltime: Some(chrono::Duration::hours(4)),
            ..Default::default()
        };
        // Small sensitive job needs 2 conformant, short walltime → placed (2 ≤ group size)
        let small = TestJob {
            nodes_min: 2,
            preemption_class: 2,
            sensitive: true,
            walltime: Some(chrono::Duration::minutes(30)),
            ..Default::default()
        };

        // Timeline: enough releases for large job reservation
        let timeline = timeline_with_release(2, vec!["x1", "x2", "x3"]);

        let result = solver.solve(
            &[large, small],
            &nodes,
            &topology,
            &CostContext::default(),
            &timeline,
        );
        // Small is placed (2 ≤ group 2), large deferred (3 > group 2)
        assert_eq!(result.placed().len(), 1);
        assert_eq!(result.deferred().len(), 1);
    }

    #[test]
    fn backfill_fires_when_deferred_job_fits_free_nodes() {
        // Two sensitive jobs deferred by conformance, one can backfill from free nodes.
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        // 6 nodes: 3 fp-a, 3 fp-b. Max conformance group = 3.
        let nodes = make_mixed_conformance_nodes(3);
        let topology = make_topology(1, 6);

        // Large sensitive: needs 4 conformant → deferred (max group=3)
        let large = TestJob {
            nodes_min: 4,
            preemption_class: 8,
            sensitive: true,
            walltime: Some(chrono::Duration::hours(4)),
            ..Default::default()
        };
        // Medium sensitive: needs 4 conformant → deferred (max group=3)
        let medium = TestJob {
            nodes_min: 4,
            preemption_class: 5,
            sensitive: true,
            walltime: Some(chrono::Duration::minutes(30)),
            ..Default::default()
        };
        // Small non-sensitive: needs 2 → placed via topology fallback
        let small = TestJob {
            nodes_min: 2,
            preemption_class: 2,
            walltime: Some(chrono::Duration::minutes(15)),
            ..Default::default()
        };

        let timeline = timeline_with_release(2, vec!["x1", "x2", "x3", "x4"]);

        let result = solver.solve(
            &[large, medium, small],
            &nodes,
            &topology,
            &CostContext::default(),
            &timeline,
        );
        // small: placed (2 nodes, non-sensitive → topology fallback). 4 free remain.
        // large: deferred (sensitive, needs 4 conformant, max group 3).
        // medium: deferred (sensitive, needs 4 conformant, max group 3).
        // Reservation for large. Medium is backfill candidate.
        // Medium needs 4, 4 free nodes, but sensitive → conformance only.
        // Max conformance group from 4 remaining = still 3. Can't fit 4. No backfill.
        assert_eq!(result.placed().len(), 1); // small
        assert_eq!(result.deferred().len(), 2); // large + medium
        assert!(result.backfilled().is_empty());
    }

    #[test]
    fn no_backfill_with_empty_timeline() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        // Large job deferred, no timeline → no reservation → no backfill
        let large = TestJob {
            nodes_min: 10,
            preemption_class: 8,
            ..Default::default()
        };
        let small = TestJob {
            nodes_min: 1,
            preemption_class: 2,
            walltime: Some(chrono::Duration::minutes(30)),
            ..Default::default()
        };
        let nodes = make_nodes(2, 0);
        let topology = make_topology(1, 2);

        let result = solver.solve(
            &[large, small],
            &nodes,
            &topology,
            &CostContext::default(),
            &empty_timeline(),
        );
        assert!(result.backfilled().is_empty());
    }

    #[test]
    fn no_backfill_without_walltime() {
        let solver = KnapsackSolver::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let large = TestJob {
            nodes_min: 10,
            preemption_class: 8,
            walltime: Some(chrono::Duration::hours(2)),
            ..Default::default()
        };
        let no_wall = TestJob {
            nodes_min: 1,
            preemption_class: 2,
            walltime: None,
            ..Default::default()
        };
        let nodes = make_nodes(4, 0);
        let topology = make_topology(1, 4);
        let timeline = timeline_with_release(1, vec!["x1", "x2", "x3", "x4", "x5", "x6"]);

        let result = solver.solve(
            &[large, no_wall],
            &nodes,
            &topology,
            &CostContext::default(),
            &timeline,
        );
        // no_wall gets placed in Pass 1 (needs 1, 4 available).
        // large deferred. No other deferred candidates for backfill.
        assert!(result.backfilled().is_empty());
    }

    // ─── End backfill tests ─────────────────────────────────────

    #[test]
    fn unavailable_nodes_skipped() {
        let solver = KnapsackSolver::new(CostWeights::default());
        let job = TestJob {
            nodes_min: 2,
            ..Default::default()
        };
        let nodes = vec![
            TestNode {
                id: "n1".into(),
                group: 0,
                available: true,
                conformance: None,
            },
            TestNode {
                id: "n2".into(),
                group: 0,
                available: false,
                conformance: None,
            },
            TestNode {
                id: "n3".into(),
                group: 0,
                available: true,
                conformance: None,
            },
        ];
        let topology = make_topology(1, 3);

        let result = solver.solve(
            &[job],
            &nodes,
            &topology,
            &CostContext::default(),
            &empty_timeline(),
        );
        assert_eq!(result.placed().len(), 1);
        if let PlacementDecision::Place { nodes, .. } = &result.decisions[0] {
            assert!(!nodes.contains(&"n2".to_string()));
            assert_eq!(nodes.len(), 2);
        }
    }
}

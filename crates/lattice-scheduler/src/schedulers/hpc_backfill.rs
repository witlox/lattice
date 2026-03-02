//! HPC Backfill scheduler.
//!
//! Priority + backfill + topology-aware scheduling.
//! Uses the knapsack solver with HPC-tuned weights.

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::{Placement, VClusterScheduler};
use lattice_common::types::*;

use crate::conformance::filter_by_constraints;
use crate::cost::{CostContext, CostEvaluator};
use crate::topology::select_nodes_topology_aware;

/// HPC backfill scheduler: priority-based with topology-aware backfill.
pub struct HpcBackfillScheduler {
    evaluator: CostEvaluator,
}

impl HpcBackfillScheduler {
    pub fn new(weights: CostWeights) -> Self {
        Self {
            evaluator: CostEvaluator::new(weights),
        }
    }
}

#[async_trait]
impl VClusterScheduler for HpcBackfillScheduler {
    async fn schedule(
        &self,
        pending: &[Allocation],
        nodes: &[Node],
    ) -> Result<Vec<Placement>, LatticeError> {
        let ctx = CostContext::default();
        let topology = TopologyModel {
            groups: build_topology_from_nodes(nodes),
        };

        // Score and sort
        let mut scored: Vec<(&Allocation, f64)> = pending
            .iter()
            .map(|a| (a, self.evaluator.score(a, &ctx)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut used: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut placements = Vec::new();

        for (alloc, _) in &scored {
            let requested = match alloc.resources.nodes {
                NodeCount::Exact(n) => n,
                NodeCount::Range { min, .. } => min,
            };

            let candidates: Vec<&Node> = nodes
                .iter()
                .filter(|n| !used.contains(&n.id) && n.state.is_operational() && n.owner.is_none())
                .collect();

            let constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

            if let Some(selected) = select_nodes_topology_aware(
                requested,
                alloc.resources.constraints.topology.as_ref(),
                &constrained,
                &topology,
            ) {
                for n in &selected {
                    used.insert(n.clone());
                }
                placements.push(Placement {
                    allocation_id: alloc.id,
                    nodes: selected,
                });
            }
            // If not placed: skip (backfill attempt with smaller jobs continues)
        }

        Ok(placements)
    }

    fn scheduler_type(&self) -> SchedulerType {
        SchedulerType::HpcBackfill
    }
}

/// Build a simple topology model from node group assignments.
fn build_topology_from_nodes(nodes: &[Node]) -> Vec<TopologyGroup> {
    let mut groups: std::collections::HashMap<GroupId, Vec<NodeId>> =
        std::collections::HashMap::new();
    for node in nodes {
        groups.entry(node.group).or_default().push(node.id.clone());
    }

    let group_ids: Vec<GroupId> = groups.keys().copied().collect();
    groups
        .into_iter()
        .map(|(id, node_ids)| TopologyGroup {
            id,
            nodes: node_ids,
            adjacent_groups: group_ids.iter().filter(|&&g| g != id).copied().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{create_node_batch, AllocationBuilder};

    #[tokio::test]
    async fn schedule_single_job() {
        let scheduler = HpcBackfillScheduler::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(2).build();
        let nodes = create_node_batch(4, 0);

        let placements = scheduler
            .schedule(std::slice::from_ref(&alloc), &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].allocation_id, alloc.id);
        assert_eq!(placements[0].nodes.len(), 2);
    }

    #[tokio::test]
    async fn higher_priority_placed_first() {
        let scheduler = HpcBackfillScheduler::new(CostWeights {
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

        let placements = scheduler
            .schedule(&[low, high.clone()], &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].allocation_id, high.id);
    }

    #[tokio::test]
    async fn backfill_smaller_jobs() {
        let scheduler = HpcBackfillScheduler::new(CostWeights::default());

        let big = AllocationBuilder::new().nodes(5).build();
        let small = AllocationBuilder::new().nodes(2).build();
        let nodes = create_node_batch(4, 0);

        let placements = scheduler
            .schedule(&[big, small.clone()], &nodes)
            .await
            .unwrap();
        // Big can't fit (needs 5, only 4 available), small can
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].allocation_id, small.id);
    }
}

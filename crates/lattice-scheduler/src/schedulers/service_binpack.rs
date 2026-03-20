//! Service bin-packing scheduler.
//!
//! Optimizes for density: pack services onto the fewest nodes and groups.
//! Autoscale-aware: respects Reactive lifecycle scaling policies.
//!
//! Bin-packing strategy (full-node scheduling):
//! 1. Sort candidates by group (lowest first) to concentrate allocations
//!    into fewer physical racks/dragonfly groups.
//! 2. Within a group, prefer nodes already selected in this cycle so
//!    multi-service bursts land on adjacent nodes.
//! 3. Track a per-group usage counter so successive allocations continue
//!    filling the densest group before spilling to the next.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::{Placement, VClusterScheduler};
use lattice_common::types::*;

use crate::conformance::filter_by_constraints;
use crate::cost::{CostContext, CostEvaluator};

/// Service bin-pack scheduler: optimizes node utilization.
pub struct ServiceBinPackScheduler {
    evaluator: CostEvaluator,
}

impl ServiceBinPackScheduler {
    pub fn new(weights: CostWeights) -> Self {
        Self {
            evaluator: CostEvaluator::new(weights),
        }
    }
}

#[async_trait]
impl VClusterScheduler for ServiceBinPackScheduler {
    async fn schedule(
        &self,
        pending: &[Allocation],
        nodes: &[Node],
    ) -> Result<Vec<Placement>, LatticeError> {
        let ctx = CostContext::default();

        // Score and sort allocations by priority (highest first)
        let mut scored: Vec<(&Allocation, f64)> = pending
            .iter()
            .map(|a| (a, self.evaluator.score(a, &ctx)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut used: HashSet<NodeId> = HashSet::new();
        // Track how many nodes are used per group for density-first packing
        let mut group_usage: HashMap<GroupId, usize> = HashMap::new();
        let mut placements = Vec::new();

        for (alloc, _) in &scored {
            let requested = match &alloc.lifecycle.lifecycle_type {
                LifecycleType::Reactive { min_nodes, .. } => *min_nodes,
                _ => match alloc.resources.nodes {
                    NodeCount::Exact(n) => n,
                    NodeCount::Range { min, .. } => min,
                },
            };

            let candidates: Vec<&Node> = nodes
                .iter()
                .filter(|n| !used.contains(&n.id) && n.state.is_operational() && n.owner.is_none())
                .collect();

            let mut constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

            // Bin-pack: sort by group density (most-used group first), then by group ID
            // to concentrate workloads into the fewest groups.
            constrained.sort_by(|a, b| {
                let a_usage = group_usage.get(&a.group).copied().unwrap_or(0);
                let b_usage = group_usage.get(&b.group).copied().unwrap_or(0);
                // Higher usage = more packed group = preferred (sort descending)
                b_usage
                    .cmp(&a_usage)
                    // Tie-break: lower group ID first (stable packing)
                    .then(a.group.cmp(&b.group))
            });

            if constrained.len() >= requested as usize {
                let selected: Vec<NodeId> = constrained
                    .iter()
                    .take(requested as usize)
                    .map(|n| n.id.clone())
                    .collect();

                for node in &constrained[..requested as usize] {
                    used.insert(node.id.clone());
                    *group_usage.entry(node.group).or_insert(0) += 1;
                }
                placements.push(Placement {
                    allocation_id: alloc.id,
                    nodes: selected,
                });
            }
        }

        Ok(placements)
    }

    fn scheduler_type(&self) -> SchedulerType {
        SchedulerType::ServiceBinPack
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{create_node_batch, AllocationBuilder, NodeBuilder};

    #[tokio::test]
    async fn schedule_service_allocation() {
        let scheduler = ServiceBinPackScheduler::new(CostWeights::default());
        let alloc = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();
        let nodes = create_node_batch(4, 0);

        let placements = scheduler
            .schedule(std::slice::from_ref(&alloc), &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].nodes.len(), 1);
    }

    #[tokio::test]
    async fn multiple_services_packed() {
        let scheduler = ServiceBinPackScheduler::new(CostWeights::default());
        let a1 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();
        let a2 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();
        let nodes = create_node_batch(2, 0);

        let placements = scheduler.schedule(&[a1, a2], &nodes).await.unwrap();
        assert_eq!(placements.len(), 2);
    }

    #[tokio::test]
    async fn packs_into_same_group() {
        let scheduler = ServiceBinPackScheduler::new(CostWeights::default());

        // 2 nodes in group 0, 2 in group 1
        let nodes = vec![
            NodeBuilder::new().id("g0-n0").group(0).build(),
            NodeBuilder::new().id("g0-n1").group(0).build(),
            NodeBuilder::new().id("g1-n0").group(1).build(),
            NodeBuilder::new().id("g1-n1").group(1).build(),
        ];

        // Submit 2 single-node services — both should land in the same group
        let a1 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();
        let a2 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();

        let placements = scheduler.schedule(&[a1, a2], &nodes).await.unwrap();
        assert_eq!(placements.len(), 2);

        // Both should be in the same group (group 0 preferred since it gets
        // usage from the first placement, making it the densest)
        let node0 = &placements[0].nodes[0];
        let node1 = &placements[1].nodes[0];
        let group0 = nodes.iter().find(|n| n.id == *node0).unwrap().group;
        let group1 = nodes.iter().find(|n| n.id == *node1).unwrap().group;
        assert_eq!(
            group0, group1,
            "Both services should pack into the same group"
        );
    }

    #[tokio::test]
    async fn spills_to_next_group_when_full() {
        let scheduler = ServiceBinPackScheduler::new(CostWeights::default());

        // 2 nodes in group 0, 2 in group 1
        let nodes = vec![
            NodeBuilder::new().id("g0-n0").group(0).build(),
            NodeBuilder::new().id("g0-n1").group(0).build(),
            NodeBuilder::new().id("g1-n0").group(1).build(),
            NodeBuilder::new().id("g1-n1").group(1).build(),
        ];

        // 3 services — should fill group 0 (2 nodes), then spill to group 1
        let a1 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();
        let a2 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();
        let a3 = AllocationBuilder::new()
            .lifecycle_unbounded()
            .nodes(1)
            .build();

        let placements = scheduler.schedule(&[a1, a2, a3], &nodes).await.unwrap();
        assert_eq!(placements.len(), 3);

        // First 2 should be in one group, third in the other
        let groups: Vec<GroupId> = placements
            .iter()
            .map(|p| nodes.iter().find(|n| n.id == p.nodes[0]).unwrap().group)
            .collect();

        // The first two should share a group
        assert_eq!(groups[0], groups[1]);
        // The third should be different
        assert_ne!(groups[1], groups[2]);
    }
}

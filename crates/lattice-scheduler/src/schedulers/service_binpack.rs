//! Service bin-packing scheduler.
//!
//! Optimizes for density: pack services onto the fewest nodes.
//! Autoscale-aware: respects Reactive lifecycle scaling policies.

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

        // Score and sort
        let mut scored: Vec<(&Allocation, f64)> = pending
            .iter()
            .map(|a| (a, self.evaluator.score(a, &ctx)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut used: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
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

            let constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

            if constrained.len() >= requested as usize {
                // Bin-pack: prefer nodes that are already partially used (but since
                // we track used/unused, just pick the first `requested` nodes)
                let selected: Vec<NodeId> = constrained
                    .iter()
                    .take(requested as usize)
                    .map(|n| n.id.clone())
                    .collect();

                for n in &selected {
                    used.insert(n.clone());
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
    use lattice_test_harness::fixtures::{create_node_batch, AllocationBuilder};

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
}

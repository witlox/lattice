//! Interactive FIFO scheduler.
//!
//! Simple FIFO ordering for short-lived interactive sessions.
//! Uses the cost function with wait-time heavily weighted to ensure
//! fairness, but fundamentally respects submission order.

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::{Placement, VClusterScheduler};
use lattice_common::types::*;

use crate::conformance::filter_by_constraints;

/// Interactive FIFO scheduler: submission-order priority.
pub struct InteractiveFifoScheduler {
    _weights: CostWeights,
}

impl InteractiveFifoScheduler {
    pub fn new(weights: CostWeights) -> Self {
        Self { _weights: weights }
    }
}

#[async_trait]
impl VClusterScheduler for InteractiveFifoScheduler {
    async fn schedule(
        &self,
        pending: &[Allocation],
        nodes: &[Node],
    ) -> Result<Vec<Placement>, LatticeError> {
        let mut used: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut placements = Vec::new();

        // FIFO: order by submission time (created_at)
        let mut sorted: Vec<&Allocation> = pending.iter().collect();
        sorted.sort_by_key(|a| a.created_at);

        for alloc in sorted {
            let requested = match alloc.resources.nodes {
                NodeCount::Exact(n) => n,
                NodeCount::Range { min, .. } => min,
            };

            let candidates: Vec<&Node> = nodes
                .iter()
                .filter(|n| !used.contains(&n.id) && n.state.is_operational() && n.owner.is_none())
                .collect();

            let constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

            if constrained.len() >= requested as usize {
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
            // FIFO: if head of queue can't be placed, stop (no backfill for interactive)
            else {
                break;
            }
        }

        Ok(placements)
    }

    fn scheduler_type(&self) -> SchedulerType {
        SchedulerType::InteractiveFifo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{create_node_batch, AllocationBuilder};

    #[tokio::test]
    async fn fifo_order_respected() {
        let scheduler = InteractiveFifoScheduler::new(CostWeights::default());

        let first = AllocationBuilder::new().nodes(1).build();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let second = AllocationBuilder::new().nodes(1).build();

        let nodes = create_node_batch(2, 0);

        let placements = scheduler
            .schedule(&[second.clone(), first.clone()], &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].allocation_id, first.id);
        assert_eq!(placements[1].allocation_id, second.id);
    }

    #[tokio::test]
    async fn fifo_blocks_on_head() {
        let scheduler = InteractiveFifoScheduler::new(CostWeights::default());

        let first = AllocationBuilder::new().nodes(10).build(); // can't fit
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let second = AllocationBuilder::new().nodes(1).build(); // could fit

        let nodes = create_node_batch(4, 0);

        let placements = scheduler.schedule(&[second, first], &nodes).await.unwrap();
        // FIFO: head can't fit, so stop — no backfill
        assert!(placements.is_empty());
    }

    #[tokio::test]
    async fn single_interactive_session() {
        let scheduler = InteractiveFifoScheduler::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let nodes = create_node_batch(4, 0);

        let placements = scheduler
            .schedule(std::slice::from_ref(&alloc), &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 1);
    }
}

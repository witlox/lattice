//! Medical reservation scheduler.
//!
//! User-claim reservation model: nodes are claimed by specific users,
//! no sharing, strict conformance, and audit logging.
//! Medical allocations are never preempted (class 10).

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::{Placement, VClusterScheduler};
use lattice_common::types::*;

use crate::conformance::{filter_by_constraints, select_conformant_nodes};

/// Medical reservation scheduler: user-claim, no sharing, strict conformance.
pub struct MedicalReservationScheduler;

impl MedicalReservationScheduler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MedicalReservationScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VClusterScheduler for MedicalReservationScheduler {
    async fn schedule(
        &self,
        pending: &[Allocation],
        nodes: &[Node],
    ) -> Result<Vec<Placement>, LatticeError> {
        let mut used: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut placements = Vec::new();

        // Medical scheduling: priority-only ordering (no cost function complexity)
        // All medical allocations are class 10, so ordering is by submission time
        let mut sorted: Vec<&Allocation> = pending.iter().collect();
        sorted.sort_by_key(|a| a.created_at);

        for alloc in sorted {
            let requested = match alloc.resources.nodes {
                NodeCount::Exact(n) => n,
                NodeCount::Range { min, .. } => min,
            };

            // Medical: only operational, unowned, conformant nodes
            let candidates: Vec<&Node> = nodes
                .iter()
                .filter(|n| {
                    !used.contains(&n.id)
                        && n.state.is_operational()
                        && n.owner.is_none()
                        && n.conformance_fingerprint.is_some() // require known conformance
                })
                .collect();

            let constrained = filter_by_constraints(&candidates, &alloc.resources.constraints);

            // Medical: strict conformance — all nodes must be from same conformance group
            if let Some(selected) = select_conformant_nodes(requested, &constrained) {
                for n in &selected {
                    used.insert(n.clone());
                }
                placements.push(Placement {
                    allocation_id: alloc.id,
                    nodes: selected,
                });
            }
            // If conformant set not available: allocation stays queued
        }

        Ok(placements)
    }

    fn scheduler_type(&self) -> SchedulerType {
        SchedulerType::MedicalReservation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{AllocationBuilder, NodeBuilder};

    #[tokio::test]
    async fn medical_requires_conformant_nodes() {
        let scheduler = MedicalReservationScheduler::new();
        let alloc = AllocationBuilder::new()
            .medical()
            .preemption_class(10)
            .nodes(2)
            .build();

        // Nodes without conformance fingerprint should be rejected
        let n1 = NodeBuilder::new().id("n1").build(); // no fingerprint
        let n2 = NodeBuilder::new().id("n2").build();
        let nodes = vec![n1, n2];

        let placements = scheduler.schedule(&[alloc], &nodes).await.unwrap();
        assert!(placements.is_empty());
    }

    #[tokio::test]
    async fn medical_places_on_conformant_nodes() {
        let scheduler = MedicalReservationScheduler::new();
        let alloc = AllocationBuilder::new()
            .medical()
            .preemption_class(10)
            .nodes(2)
            .build();

        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-a").build();
        let nodes = vec![n1, n2];

        let placements = scheduler
            .schedule(std::slice::from_ref(&alloc), &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].allocation_id, alloc.id);
    }

    #[tokio::test]
    async fn medical_rejects_mixed_conformance() {
        let scheduler = MedicalReservationScheduler::new();
        let alloc = AllocationBuilder::new()
            .medical()
            .preemption_class(10)
            .nodes(2)
            .build();

        // Two different fingerprints — can't form a conformant group of 2
        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-b").build();
        let nodes = vec![n1, n2];

        let placements = scheduler.schedule(&[alloc], &nodes).await.unwrap();
        assert!(placements.is_empty());
    }

    #[tokio::test]
    async fn medical_fifo_ordering() {
        let scheduler = MedicalReservationScheduler::new();
        let first = AllocationBuilder::new()
            .medical()
            .preemption_class(10)
            .nodes(2)
            .build();

        // Delay to ensure different timestamps
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let second = AllocationBuilder::new()
            .medical()
            .preemption_class(10)
            .nodes(2)
            .build();

        let nodes: Vec<Node> = (0..4)
            .map(|i| {
                NodeBuilder::new()
                    .id(&format!("n{i}"))
                    .conformance("fp-a")
                    .build()
            })
            .collect();

        let placements = scheduler
            .schedule(&[second.clone(), first.clone()], &nodes)
            .await
            .unwrap();
        assert_eq!(placements.len(), 2);
        // First submitted should be placed first
        assert_eq!(placements[0].allocation_id, first.id);
        assert_eq!(placements[1].allocation_id, second.id);
    }
}

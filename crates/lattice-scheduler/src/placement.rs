//! Placement result types.
//!
//! A `PlacementDecision` is the output of the scheduling algorithm:
//! which allocations are placed on which nodes, and which need preemption.

use chrono::{DateTime, Utc};
use lattice_common::types::*;

/// Result of a single allocation placement decision.
#[derive(Debug, Clone)]
pub enum PlacementDecision {
    /// Allocation can be placed on the specified nodes.
    Place {
        allocation_id: AllocId,
        nodes: Vec<NodeId>,
    },
    /// Allocation needs preemption of existing allocations to be placed.
    Preempt {
        allocation_id: AllocId,
        nodes: Vec<NodeId>,
        victims: Vec<AllocId>,
    },
    /// Allocation placed via backfill — must complete before the reservation holder starts.
    Backfill {
        allocation_id: AllocId,
        nodes: Vec<NodeId>,
        reservation_holder: AllocId,
        must_complete_by: DateTime<Utc>,
    },
    /// Allocation cannot be placed (insufficient resources, constraints, etc.)
    Defer {
        allocation_id: AllocId,
        reason: String,
    },
}

impl PlacementDecision {
    pub fn allocation_id(&self) -> AllocId {
        match self {
            PlacementDecision::Place { allocation_id, .. }
            | PlacementDecision::Preempt { allocation_id, .. }
            | PlacementDecision::Backfill { allocation_id, .. }
            | PlacementDecision::Defer { allocation_id, .. } => *allocation_id,
        }
    }

    pub fn is_placed(&self) -> bool {
        matches!(
            self,
            PlacementDecision::Place { .. } | PlacementDecision::Backfill { .. }
        )
    }

    pub fn is_preempt(&self) -> bool {
        matches!(self, PlacementDecision::Preempt { .. })
    }

    pub fn is_backfill(&self) -> bool {
        matches!(self, PlacementDecision::Backfill { .. })
    }

    pub fn is_deferred(&self) -> bool {
        matches!(self, PlacementDecision::Defer { .. })
    }
}

/// The complete output of a scheduling cycle.
#[derive(Debug, Clone, Default)]
pub struct SchedulingResult {
    pub decisions: Vec<PlacementDecision>,
}

impl SchedulingResult {
    pub fn placed(&self) -> Vec<&PlacementDecision> {
        self.decisions.iter().filter(|d| d.is_placed()).collect()
    }

    pub fn preemptions(&self) -> Vec<&PlacementDecision> {
        self.decisions.iter().filter(|d| d.is_preempt()).collect()
    }

    pub fn deferred(&self) -> Vec<&PlacementDecision> {
        self.decisions.iter().filter(|d| d.is_deferred()).collect()
    }

    pub fn backfilled(&self) -> Vec<&PlacementDecision> {
        self.decisions.iter().filter(|d| d.is_backfill()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn placement_decision_variants() {
        let id = Uuid::new_v4();
        let place = PlacementDecision::Place {
            allocation_id: id,
            nodes: vec!["n1".into()],
        };
        assert!(place.is_placed());
        assert!(!place.is_preempt());
        assert!(!place.is_deferred());
        assert_eq!(place.allocation_id(), id);
    }

    #[test]
    fn backfill_decision_variant() {
        let id = Uuid::new_v4();
        let holder = Uuid::new_v4();
        let deadline = chrono::Utc::now() + chrono::Duration::hours(2);

        let backfill = PlacementDecision::Backfill {
            allocation_id: id,
            nodes: vec!["n1".into()],
            reservation_holder: holder,
            must_complete_by: deadline,
        };
        assert!(backfill.is_placed());
        assert!(backfill.is_backfill());
        assert!(!backfill.is_preempt());
        assert!(!backfill.is_deferred());
        assert_eq!(backfill.allocation_id(), id);
    }

    #[test]
    fn scheduling_result_filtering() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        let id4 = Uuid::new_v4();

        let result = SchedulingResult {
            decisions: vec![
                PlacementDecision::Place {
                    allocation_id: id1,
                    nodes: vec!["n1".into()],
                },
                PlacementDecision::Preempt {
                    allocation_id: id2,
                    nodes: vec!["n2".into()],
                    victims: vec![id3],
                },
                PlacementDecision::Defer {
                    allocation_id: id3,
                    reason: "no nodes".into(),
                },
                PlacementDecision::Backfill {
                    allocation_id: id4,
                    nodes: vec!["n3".into()],
                    reservation_holder: id1,
                    must_complete_by: chrono::Utc::now(),
                },
            ],
        };

        // placed() includes both Place and Backfill
        assert_eq!(result.placed().len(), 2);
        assert_eq!(result.preemptions().len(), 1);
        assert_eq!(result.deferred().len(), 1);
    }
}

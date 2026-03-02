//! Inter-vCluster elastic node borrowing (lending).
//!
//! Implements the elastic borrowing model described in
//! `docs/architecture/scheduling-algorithm.md` and referenced by
//! `NodeOwnership::is_borrowed` in `lattice-common`.
//!
//! A vCluster with idle nodes can **lend** them to another vCluster
//! that needs extra capacity. The lending vCluster can reclaim nodes
//! when it needs them (subject to `return_grace_secs`).

use lattice_common::types::{NodeId, VClusterId};

/// Configuration controlling how a vCluster participates in borrowing.
#[derive(Debug, Clone)]
pub struct BorrowingConfig {
    /// Whether this vCluster is willing to lend idle nodes to others.
    pub allow_lending: bool,
    /// Whether this vCluster is allowed to borrow from others.
    pub allow_borrowing: bool,
    /// Maximum fraction of the vCluster's dedicated node pool that can
    /// be lent out at any time (0.0–1.0, default 0.2).
    pub max_borrow_pct: f64,
    /// Grace period (seconds) given to a borrower before nodes are
    /// forcibly reclaimed when the owner vCluster needs them back.
    pub return_grace_secs: u64,
}

impl Default for BorrowingConfig {
    fn default() -> Self {
        Self {
            allow_lending: true,
            allow_borrowing: true,
            max_borrow_pct: 0.2,
            return_grace_secs: 60,
        }
    }
}

/// A request from one vCluster to borrow nodes from another.
#[derive(Debug, Clone)]
pub struct BorrowRequest {
    /// The vCluster that currently owns the idle nodes (the lender).
    pub source_vcluster: VClusterId,
    /// The vCluster that needs the extra nodes (the borrower).
    pub target_vcluster: VClusterId,
    /// How many nodes the borrower is requesting.
    pub node_count: u32,
    /// Scheduling priority of the borrow request (higher = more urgent).
    pub priority: u8,
}

/// Outcome of evaluating a borrow request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BorrowResult {
    /// The request is approved; the returned node IDs are lent to the borrower.
    Approved { nodes: Vec<NodeId> },
    /// The request is denied.
    Denied { reason: String },
}

impl BorrowResult {
    /// Return `true` if the request was approved.
    pub fn is_approved(&self) -> bool {
        matches!(self, BorrowResult::Approved { .. })
    }
}

/// Broker that manages inter-vCluster node lending.
pub struct BorrowingBroker {
    /// Global borrowing configuration (applies to the whole broker).
    config: BorrowingConfig,
}

impl BorrowingBroker {
    /// Create a new broker with the supplied configuration.
    pub fn new(config: BorrowingConfig) -> Self {
        Self { config }
    }

    /// Evaluate a borrow request against available idle nodes.
    ///
    /// # Arguments
    /// - `request` – the borrow request to evaluate
    /// - `idle_nodes` – nodes currently idle in `request.source_vcluster`
    /// - `total_dedicated` – total dedicated node count of the source vCluster
    ///   (used to compute the maximum lendable fraction)
    pub fn evaluate_request(
        &self,
        request: &BorrowRequest,
        idle_nodes: &[NodeId],
        total_dedicated: u32,
    ) -> BorrowResult {
        // Check: lender must allow lending.
        if !self.config.allow_lending {
            return BorrowResult::Denied {
                reason: "source vCluster does not allow lending".into(),
            };
        }

        // Check: borrower must allow borrowing.
        if !self.config.allow_borrowing {
            return BorrowResult::Denied {
                reason: "target vCluster does not allow borrowing".into(),
            };
        }

        // Compute how many nodes can be lent out (capacity cap).
        let max_lendable = if total_dedicated == 0 {
            0
        } else {
            ((total_dedicated as f64 * self.config.max_borrow_pct).floor()) as u32
        };

        if max_lendable == 0 {
            return BorrowResult::Denied {
                reason: "source vCluster has no lending capacity (max_borrow_pct too low or dedicated pool empty)".into(),
            };
        }

        // The number of nodes we can actually provide is bounded by:
        //   1. How many are idle
        //   2. The max_borrow_pct cap
        let available = (idle_nodes.len() as u32).min(max_lendable);

        if available == 0 {
            return BorrowResult::Denied {
                reason: "no idle nodes available in source vCluster".into(),
            };
        }

        if request.node_count > available {
            return BorrowResult::Denied {
                reason: format!(
                    "requested {} nodes but only {} available (idle={}, cap={})",
                    request.node_count,
                    available,
                    idle_nodes.len(),
                    max_lendable,
                ),
            };
        }

        // Grant the first `node_count` idle nodes.
        let lent: Vec<NodeId> = idle_nodes
            .iter()
            .take(request.node_count as usize)
            .cloned()
            .collect();

        BorrowResult::Approved { nodes: lent }
    }

    /// Return the broker's configuration.
    pub fn config(&self) -> &BorrowingConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_nodes(count: usize) -> Vec<NodeId> {
        (0..count).map(|i| format!("node-{i}")).collect()
    }

    fn default_broker() -> BorrowingBroker {
        BorrowingBroker::new(BorrowingConfig::default())
    }

    fn simple_request(node_count: u32) -> BorrowRequest {
        BorrowRequest {
            source_vcluster: "vc-lender".into(),
            target_vcluster: "vc-borrower".into(),
            node_count,
            priority: 5,
        }
    }

    // ── Allow/deny based on config ───────────────────────────

    #[test]
    fn borrow_allowed_when_both_sides_enabled() {
        let broker = default_broker();
        let idle = make_nodes(10);
        let result = broker.evaluate_request(&simple_request(2), &idle, 10);
        assert!(result.is_approved());
    }

    #[test]
    fn borrow_denied_when_lending_disabled() {
        let config = BorrowingConfig {
            allow_lending: false,
            ..Default::default()
        };
        let broker = BorrowingBroker::new(config);
        let idle = make_nodes(10);
        let result = broker.evaluate_request(&simple_request(2), &idle, 10);
        assert!(!result.is_approved());
        if let BorrowResult::Denied { reason } = result {
            assert!(reason.contains("does not allow lending"));
        } else {
            panic!("expected Denied");
        }
    }

    #[test]
    fn borrow_denied_when_borrowing_disabled() {
        let config = BorrowingConfig {
            allow_borrowing: false,
            ..Default::default()
        };
        let broker = BorrowingBroker::new(config);
        let idle = make_nodes(10);
        let result = broker.evaluate_request(&simple_request(2), &idle, 10);
        assert!(!result.is_approved());
        if let BorrowResult::Denied { reason } = result {
            assert!(reason.contains("does not allow borrowing"));
        } else {
            panic!("expected Denied");
        }
    }

    // ── Capacity limit tests ─────────────────────────────────

    #[test]
    fn capacity_limit_respected_caps_lendable_nodes() {
        // 20% of 10 dedicated = 2 lendable. Requesting 3 must be denied.
        let config = BorrowingConfig {
            max_borrow_pct: 0.2,
            ..Default::default()
        };
        let broker = BorrowingBroker::new(config);
        let idle = make_nodes(10); // plenty of idle nodes
        let result = broker.evaluate_request(&simple_request(3), &idle, 10);
        assert!(!result.is_approved());
        if let BorrowResult::Denied { reason } = result {
            assert!(reason.contains("available"));
        } else {
            panic!("expected Denied");
        }
    }

    #[test]
    fn capacity_limit_respected_within_cap() {
        // 20% of 10 dedicated = 2 lendable. Requesting 2 should be approved.
        let config = BorrowingConfig {
            max_borrow_pct: 0.2,
            ..Default::default()
        };
        let broker = BorrowingBroker::new(config);
        let idle = make_nodes(10);
        let result = broker.evaluate_request(&simple_request(2), &idle, 10);
        assert!(result.is_approved());
        if let BorrowResult::Approved { nodes } = result {
            assert_eq!(nodes.len(), 2);
        } else {
            panic!("expected Approved");
        }
    }

    #[test]
    fn zero_dedicated_nodes_returns_denied() {
        let broker = default_broker();
        let idle = make_nodes(5);
        // total_dedicated = 0 → max_lendable = 0
        let result = broker.evaluate_request(&simple_request(1), &idle, 0);
        assert!(!result.is_approved());
    }

    #[test]
    fn no_idle_nodes_returns_denied() {
        let broker = default_broker();
        let idle: Vec<NodeId> = vec![];
        let result = broker.evaluate_request(&simple_request(1), &idle, 10);
        assert!(!result.is_approved());
        if let BorrowResult::Denied { reason } = result {
            assert!(reason.contains("no idle nodes"));
        } else {
            panic!("expected Denied");
        }
    }

    #[test]
    fn requesting_more_than_idle_returns_denied() {
        let broker = default_broker();
        let idle = make_nodes(1);
        // Requesting 2 but only 1 idle (and cap allows more).
        let result = broker.evaluate_request(&simple_request(2), &idle, 100);
        assert!(!result.is_approved());
    }

    // ── Approved result content tests ────────────────────────

    #[test]
    fn approved_result_contains_requested_node_ids() {
        let broker = default_broker();
        let idle = make_nodes(5);
        let result = broker.evaluate_request(&simple_request(3), &idle, 20);
        if let BorrowResult::Approved { nodes } = result {
            assert_eq!(nodes.len(), 3);
            // Nodes should come from the idle list.
            for n in &nodes {
                assert!(idle.contains(n));
            }
        } else {
            panic!("expected Approved");
        }
    }

    #[test]
    fn approved_result_node_count_matches_request() {
        let broker = default_broker();
        let idle = make_nodes(10);
        let result = broker.evaluate_request(&simple_request(4), &idle, 50);
        if let BorrowResult::Approved { nodes } = result {
            assert_eq!(nodes.len(), 4);
        } else {
            panic!("expected Approved");
        }
    }

    // ── max_borrow_pct edge cases ────────────────────────────

    #[test]
    fn max_borrow_pct_zero_denies_all() {
        let config = BorrowingConfig {
            max_borrow_pct: 0.0,
            ..Default::default()
        };
        let broker = BorrowingBroker::new(config);
        let idle = make_nodes(10);
        let result = broker.evaluate_request(&simple_request(1), &idle, 10);
        assert!(!result.is_approved());
    }

    #[test]
    fn max_borrow_pct_one_allows_all_idle() {
        let config = BorrowingConfig {
            max_borrow_pct: 1.0,
            ..Default::default()
        };
        let broker = BorrowingBroker::new(config);
        let idle = make_nodes(5);
        let result = broker.evaluate_request(&simple_request(5), &idle, 5);
        assert!(result.is_approved());
    }
}

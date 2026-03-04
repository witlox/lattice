//! Federation broker — coordinates scheduling across federated Lattice sites.
//!
//! The broker receives scheduling suggestions from remote sites and decides
//! whether to accept or reject them based on local capacity and policy.
//! Federation is always opt-in — the system works fully without it.
//!
//! Key principles:
//! - Loose coupling: federation broker suggests, local scheduler decides
//! - Data gravity drives placement (prefer sites where data lives)
//! - Sensitive data sovereignty enforced (data stays at designated site)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use lattice_common::types::{AllocId, NodeId, TenantId};

/// A scheduling suggestion from a remote federation site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationOffer {
    /// ID of the remote site making the offer.
    pub source_site: String,
    /// Allocation to be placed.
    pub allocation_id: AllocId,
    /// Tenant that owns the allocation.
    pub tenant_id: TenantId,
    /// Number of nodes requested.
    pub node_count: u32,
    /// Whether this is a sensitive/regulated workload.
    pub sensitive: bool,
    /// Data location hints (site IDs where input data resides).
    pub data_locations: Vec<String>,
    /// When the offer was made.
    pub offered_at: DateTime<Utc>,
    /// Offer expiry (seconds from offered_at).
    pub ttl_secs: u64,
}

/// Result of evaluating a federation offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfferDecision {
    /// Accept the workload for local execution.
    Accept {
        /// Nodes allocated for the remote workload.
        nodes: Vec<NodeId>,
    },
    /// Reject the offer.
    Reject { reason: String },
}

/// Configuration for the federation broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationConfig {
    /// This site's identifier.
    pub site_id: String,
    /// Maximum percentage of local capacity available for federation (0.0-1.0).
    pub max_federation_pct: f64,
    /// Whether to accept sensitive workloads from remote sites.
    pub accept_sensitive: bool,
    /// Trusted remote sites (site IDs).
    pub trusted_sites: Vec<String>,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            site_id: "local".to_string(),
            max_federation_pct: 0.1,
            accept_sensitive: false,
            trusted_sites: Vec::new(),
        }
    }
}

/// The federation broker evaluates remote scheduling offers.
#[derive(Debug)]
pub struct FederationBroker {
    config: FederationConfig,
}

impl FederationBroker {
    pub fn new(config: FederationConfig) -> Self {
        Self { config }
    }

    /// Evaluate a federation offer against local policy.
    pub fn evaluate_offer(
        &self,
        offer: &FederationOffer,
        idle_node_count: u32,
        total_node_count: u32,
    ) -> OfferDecision {
        // Check if the source site is trusted
        if !self.config.trusted_sites.contains(&offer.source_site) {
            return OfferDecision::Reject {
                reason: format!("site '{}' is not trusted", offer.source_site),
            };
        }

        // Sensitive workloads require explicit opt-in
        if offer.sensitive && !self.config.accept_sensitive {
            return OfferDecision::Reject {
                reason: "sensitive workloads not accepted from federation".to_string(),
            };
        }

        // Check capacity limits
        let max_federation_nodes =
            (total_node_count as f64 * self.config.max_federation_pct) as u32;
        if offer.node_count > max_federation_nodes {
            return OfferDecision::Reject {
                reason: format!(
                    "request for {} nodes exceeds federation limit of {}",
                    offer.node_count, max_federation_nodes
                ),
            };
        }

        // Check if we have enough idle nodes
        if offer.node_count > idle_node_count {
            return OfferDecision::Reject {
                reason: format!(
                    "insufficient idle nodes: need {}, have {}",
                    offer.node_count, idle_node_count
                ),
            };
        }

        // Check if data is local (data gravity)
        let data_is_local =
            offer.data_locations.is_empty() || offer.data_locations.contains(&self.config.site_id);

        if !data_is_local {
            // Still accept, but note the data transfer cost
            tracing::info!(
                source = %offer.source_site,
                allocation = %offer.allocation_id,
                "accepting federation offer with remote data (data gravity penalty)"
            );
        }

        // Generate placeholder node IDs
        let nodes: Vec<NodeId> = (0..offer.node_count)
            .map(|i| format!("{}-fed-node-{i}", self.config.site_id))
            .collect();

        OfferDecision::Accept { nodes }
    }

    /// Check if an offer has expired.
    pub fn is_expired(offer: &FederationOffer) -> bool {
        let now = Utc::now();
        let expires_at = offer.offered_at + chrono::Duration::seconds(offer.ttl_secs as i64);
        now > expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> FederationConfig {
        FederationConfig {
            site_id: "site-a".to_string(),
            max_federation_pct: 0.2,
            accept_sensitive: false,
            trusted_sites: vec!["site-b".to_string(), "site-c".to_string()],
        }
    }

    fn test_offer(source: &str, nodes: u32) -> FederationOffer {
        FederationOffer {
            source_site: source.to_string(),
            allocation_id: uuid::Uuid::new_v4(),
            tenant_id: "physics".to_string(),
            node_count: nodes,
            sensitive: false,
            data_locations: vec![],
            offered_at: Utc::now(),
            ttl_secs: 300,
        }
    }

    #[test]
    fn accept_trusted_offer_within_capacity() {
        let broker = FederationBroker::new(test_config());
        let offer = test_offer("site-b", 2);

        let decision = broker.evaluate_offer(&offer, 10, 100);
        match decision {
            OfferDecision::Accept { nodes } => assert_eq!(nodes.len(), 2),
            OfferDecision::Reject { reason } => panic!("expected accept, got reject: {reason}"),
        }
    }

    #[test]
    fn reject_untrusted_site() {
        let broker = FederationBroker::new(test_config());
        let offer = test_offer("evil-site", 1);

        let decision = broker.evaluate_offer(&offer, 10, 100);
        assert!(matches!(decision, OfferDecision::Reject { .. }));
    }

    #[test]
    fn reject_sensitive_by_default() {
        let broker = FederationBroker::new(test_config());
        let mut offer = test_offer("site-b", 1);
        offer.sensitive = true;

        let decision = broker.evaluate_offer(&offer, 10, 100);
        assert!(matches!(decision, OfferDecision::Reject { .. }));
    }

    #[test]
    fn accept_sensitive_when_configured() {
        let mut config = test_config();
        config.accept_sensitive = true;
        let broker = FederationBroker::new(config);
        let mut offer = test_offer("site-b", 1);
        offer.sensitive = true;

        let decision = broker.evaluate_offer(&offer, 10, 100);
        assert!(matches!(decision, OfferDecision::Accept { .. }));
    }

    #[test]
    fn reject_exceeds_federation_limit() {
        let broker = FederationBroker::new(test_config());
        // max_federation_pct = 0.2, total_nodes = 100, so max = 20 nodes
        let offer = test_offer("site-b", 25);

        let decision = broker.evaluate_offer(&offer, 30, 100);
        assert!(matches!(decision, OfferDecision::Reject { .. }));
    }

    #[test]
    fn reject_insufficient_idle_nodes() {
        let broker = FederationBroker::new(test_config());
        let offer = test_offer("site-b", 5);

        let decision = broker.evaluate_offer(&offer, 2, 100);
        assert!(matches!(decision, OfferDecision::Reject { .. }));
    }

    #[test]
    fn accept_with_data_gravity() {
        let broker = FederationBroker::new(test_config());
        let mut offer = test_offer("site-b", 2);
        offer.data_locations = vec!["site-a".to_string()]; // data is local

        let decision = broker.evaluate_offer(&offer, 10, 100);
        assert!(matches!(decision, OfferDecision::Accept { .. }));
    }

    #[test]
    fn accept_with_remote_data() {
        let broker = FederationBroker::new(test_config());
        let mut offer = test_offer("site-b", 2);
        offer.data_locations = vec!["site-b".to_string()]; // data is remote

        // Still accepted (just with data gravity penalty noted)
        let decision = broker.evaluate_offer(&offer, 10, 100);
        assert!(matches!(decision, OfferDecision::Accept { .. }));
    }

    #[test]
    fn offer_not_expired() {
        let offer = test_offer("site-b", 1);
        assert!(!FederationBroker::is_expired(&offer));
    }

    #[test]
    fn offer_expired() {
        let mut offer = test_offer("site-b", 1);
        offer.offered_at = Utc::now() - chrono::Duration::seconds(600);
        offer.ttl_secs = 300;
        assert!(FederationBroker::is_expired(&offer));
    }

    #[test]
    fn default_config() {
        let config = FederationConfig::default();
        assert_eq!(config.site_id, "local");
        assert!((config.max_federation_pct - 0.1).abs() < 0.001);
        assert!(!config.accept_sensitive);
        assert!(config.trusted_sites.is_empty());
    }

    #[test]
    fn node_ids_include_site_prefix() {
        let broker = FederationBroker::new(test_config());
        let offer = test_offer("site-b", 3);

        if let OfferDecision::Accept { nodes } = broker.evaluate_offer(&offer, 10, 100) {
            for node in &nodes {
                assert!(node.starts_with("site-a-fed-node-"));
            }
        } else {
            panic!("expected accept");
        }
    }
}

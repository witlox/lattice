//! VNI (Virtual Network Identifier) management for network domains.
//!
//! Allocations sharing a network domain get L3 reachability via
//! Slingshot VNIs. This module tracks which VNIs are applied on
//! this node and manages apply/remove operations.
//!
//! VNI release supports a configurable grace period (default 5 minutes)
//! to allow dependent services or follow-up allocations to reuse the
//! same network domain without a gap in connectivity.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use lattice_common::types::AllocId;

/// Default VNI grace period: 5 minutes.
const DEFAULT_VNI_GRACE_SECONDS: i64 = 300;

/// A network domain configuration applied on a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VniConfig {
    pub vni: u32,
    pub allocation_id: AllocId,
    pub traffic_class: TrafficClass,
}

/// Traffic class for prioritized network domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficClass {
    /// Best-effort data traffic.
    BestEffort,
    /// Low-latency RDMA traffic.
    LowLatency,
    /// Management/heartbeat traffic (highest priority).
    Management,
}

/// A VNI that is in its grace period before being fully released.
#[derive(Debug, Clone)]
struct GracedVni {
    config: VniConfig,
    /// When the grace period started (i.e., when release_with_grace was called).
    grace_start: DateTime<Utc>,
}

/// Manages VNI assignments on a single node.
///
/// Supports immediate release (`remove`) and deferred release with a grace
/// period (`release_with_grace`). During the grace period, the VNI remains
/// reserved and can be reclaimed by the same allocation or a new allocation
/// explicitly requesting that VNI. After the grace period elapses, calling
/// `collect_expired_graces` returns and removes the fully released VNIs.
pub struct VniManager {
    assignments: HashMap<AllocId, VniConfig>,
    /// VNIs in their grace period, keyed by allocation ID.
    graced: HashMap<AllocId, GracedVni>,
    /// Duration of the VNI grace period.
    grace_period: Duration,
}

impl VniManager {
    /// Create a new VNI manager with the default 5-minute grace period.
    pub fn new() -> Self {
        Self {
            assignments: HashMap::new(),
            graced: HashMap::new(),
            grace_period: Duration::seconds(DEFAULT_VNI_GRACE_SECONDS),
        }
    }

    /// Create a new VNI manager with a custom grace period.
    pub fn with_grace_period(grace_period: Duration) -> Self {
        Self {
            assignments: HashMap::new(),
            graced: HashMap::new(),
            grace_period,
        }
    }

    /// Apply a VNI for an allocation.
    pub fn apply(&mut self, config: VniConfig) -> Result<(), String> {
        // Check for VNI collision (different allocation using same VNI)
        for (id, existing) in &self.assignments {
            if existing.vni == config.vni && *id != config.allocation_id {
                return Err(format!(
                    "VNI {} already assigned to allocation {id}",
                    config.vni
                ));
            }
        }
        // Also check graced VNIs for collision
        for (id, graced) in &self.graced {
            if graced.config.vni == config.vni && *id != config.allocation_id {
                return Err(format!(
                    "VNI {} still in grace period for allocation {id}",
                    config.vni
                ));
            }
        }

        // If this allocation had a graced VNI, remove it from grace (reclaimed)
        self.graced.remove(&config.allocation_id);

        self.assignments.insert(config.allocation_id, config);
        Ok(())
    }

    /// Remove VNI assignment for an allocation immediately (no grace period).
    pub fn remove(&mut self, allocation_id: &AllocId) -> Option<VniConfig> {
        self.assignments.remove(allocation_id)
    }

    /// Release a VNI with a grace period. The VNI is moved from active
    /// assignments to the grace pool. It will not be available for other
    /// allocations until the grace period elapses and `collect_expired_graces`
    /// is called.
    ///
    /// Returns `None` if the allocation has no active VNI assignment.
    pub fn release_with_grace(
        &mut self,
        allocation_id: &AllocId,
        now: DateTime<Utc>,
    ) -> Option<VniConfig> {
        if let Some(config) = self.assignments.remove(allocation_id) {
            let graced_config = config.clone();
            self.graced.insert(
                *allocation_id,
                GracedVni {
                    config,
                    grace_start: now,
                },
            );
            Some(graced_config)
        } else {
            None
        }
    }

    /// Collect VNIs whose grace period has expired. Returns the configs
    /// that are now fully released, removing them from the grace pool.
    pub fn collect_expired_graces(&mut self, now: DateTime<Utc>) -> Vec<VniConfig> {
        let grace = self.grace_period;
        let mut expired = Vec::new();
        let mut expired_ids = Vec::new();

        for (alloc_id, graced) in &self.graced {
            if now >= graced.grace_start + grace {
                expired.push(graced.config.clone());
                expired_ids.push(*alloc_id);
            }
        }

        for id in &expired_ids {
            self.graced.remove(id);
        }

        expired
    }

    /// Number of VNIs currently in the grace period.
    pub fn graced_count(&self) -> usize {
        self.graced.len()
    }

    /// Check if a VNI is in grace period for a given allocation.
    pub fn is_in_grace(&self, allocation_id: &AllocId) -> bool {
        self.graced.contains_key(allocation_id)
    }

    /// Get VNI config for an allocation.
    pub fn get(&self, allocation_id: &AllocId) -> Option<&VniConfig> {
        self.assignments.get(allocation_id)
    }

    /// Count active VNI assignments (not including graced).
    pub fn active_count(&self) -> usize {
        self.assignments.len()
    }

    /// List all active VNIs (not including graced).
    pub fn list_vnis(&self) -> Vec<u32> {
        self.assignments.values().map(|c| c.vni).collect()
    }
}

impl Default for VniManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Original VNI management tests ─────────────────────────

    #[test]
    fn apply_and_retrieve_vni() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();

        let config = VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::LowLatency,
        };
        mgr.apply(config).unwrap();

        let retrieved = mgr.get(&id).unwrap();
        assert_eq!(retrieved.vni, 1000);
        assert_eq!(retrieved.traffic_class, TrafficClass::LowLatency);
    }

    #[test]
    fn remove_vni() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        let removed = mgr.remove(&id);
        assert!(removed.is_some());
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn vni_collision_rejected() {
        let mut mgr = VniManager::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id1,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        let result = mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id2,
            traffic_class: TrafficClass::BestEffort,
        });
        assert!(result.is_err());
    }

    #[test]
    fn same_allocation_can_update_vni() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        // Same allocation, same VNI — should succeed (update)
        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::LowLatency,
        })
        .unwrap();

        assert_eq!(
            mgr.get(&id).unwrap().traffic_class,
            TrafficClass::LowLatency
        );
    }

    #[test]
    fn multiple_allocations_different_vnis() {
        let mut mgr = VniManager::new();

        for i in 0..5 {
            let id = uuid::Uuid::new_v4();
            mgr.apply(VniConfig {
                vni: 1000 + i,
                allocation_id: id,
                traffic_class: TrafficClass::BestEffort,
            })
            .unwrap();
        }

        assert_eq!(mgr.active_count(), 5);
        assert_eq!(mgr.list_vnis().len(), 5);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut mgr = VniManager::new();
        assert!(mgr.remove(&uuid::Uuid::new_v4()).is_none());
    }

    // ─── VNI grace period tests ────────────────────────────────

    #[test]
    fn release_with_grace_moves_to_graced() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        let released = mgr.release_with_grace(&id, now);
        assert!(released.is_some());
        assert_eq!(released.unwrap().vni, 1000);

        // No longer in active assignments
        assert_eq!(mgr.active_count(), 0);
        assert!(mgr.get(&id).is_none());

        // But is in grace pool
        assert!(mgr.is_in_grace(&id));
        assert_eq!(mgr.graced_count(), 1);
    }

    #[test]
    fn release_with_grace_nonexistent_returns_none() {
        let mut mgr = VniManager::new();
        let result = mgr.release_with_grace(&uuid::Uuid::new_v4(), Utc::now());
        assert!(result.is_none());
        assert_eq!(mgr.graced_count(), 0);
    }

    #[test]
    fn graced_vni_blocks_collision() {
        let mut mgr = VniManager::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id1,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        mgr.release_with_grace(&id1, now);

        // Another allocation trying to use the same VNI should be rejected
        let result = mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id2,
            traffic_class: TrafficClass::BestEffort,
        });
        assert!(result.is_err());
    }

    #[test]
    fn graced_vni_not_expired_during_grace_period() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        mgr.release_with_grace(&id, now);

        // Check before grace period elapses (default 5 min, check at 2 min)
        let expired = mgr.collect_expired_graces(now + Duration::minutes(2));
        assert!(expired.is_empty());
        assert_eq!(mgr.graced_count(), 1);
    }

    #[test]
    fn graced_vni_expired_after_grace_period() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        mgr.release_with_grace(&id, now);

        // Check after grace period (default 5 min)
        let expired = mgr.collect_expired_graces(now + Duration::minutes(5));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].vni, 1000);
        assert_eq!(mgr.graced_count(), 0);
    }

    #[test]
    fn custom_grace_period() {
        let mut mgr = VniManager::with_grace_period(Duration::seconds(60));
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        mgr.release_with_grace(&id, now);

        // Not expired at 30s
        let expired = mgr.collect_expired_graces(now + Duration::seconds(30));
        assert!(expired.is_empty());

        // Expired at 60s
        let expired = mgr.collect_expired_graces(now + Duration::seconds(60));
        assert_eq!(expired.len(), 1);
    }

    #[test]
    fn multiple_graced_vnis_expire_independently() {
        let mut mgr = VniManager::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id1,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();
        mgr.apply(VniConfig {
            vni: 2000,
            allocation_id: id2,
            traffic_class: TrafficClass::LowLatency,
        })
        .unwrap();

        // Release at different times
        mgr.release_with_grace(&id1, now);
        mgr.release_with_grace(&id2, now + Duration::minutes(2));

        // At 5 min: only id1's grace expired
        let expired = mgr.collect_expired_graces(now + Duration::minutes(5));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].vni, 1000);
        assert_eq!(mgr.graced_count(), 1);

        // At 7 min: id2's grace also expired
        let expired = mgr.collect_expired_graces(now + Duration::minutes(7));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].vni, 2000);
        assert_eq!(mgr.graced_count(), 0);
    }

    #[test]
    fn reclaim_graced_vni_by_same_allocation() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();
        let now = Utc::now();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        mgr.release_with_grace(&id, now);
        assert!(mgr.is_in_grace(&id));

        // Same allocation re-applies the same VNI
        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::LowLatency,
        })
        .unwrap();

        // Back in active, no longer graced
        assert!(!mgr.is_in_grace(&id));
        assert_eq!(mgr.active_count(), 1);
        assert_eq!(mgr.graced_count(), 0);
        assert_eq!(
            mgr.get(&id).unwrap().traffic_class,
            TrafficClass::LowLatency
        );
    }

    #[test]
    fn collect_expired_graces_empty_when_no_graced() {
        let mut mgr = VniManager::new();
        let expired = mgr.collect_expired_graces(Utc::now());
        assert!(expired.is_empty());
    }

    #[test]
    fn immediate_remove_does_not_create_grace() {
        let mut mgr = VniManager::new();
        let id = uuid::Uuid::new_v4();

        mgr.apply(VniConfig {
            vni: 1000,
            allocation_id: id,
            traffic_class: TrafficClass::BestEffort,
        })
        .unwrap();

        // Immediate remove: no grace period
        mgr.remove(&id);
        assert!(!mgr.is_in_grace(&id));
        assert_eq!(mgr.graced_count(), 0);
    }

    #[test]
    fn default_grace_period_is_five_minutes() {
        let mgr = VniManager::new();
        // Verify by testing behavior: grace should last 5 min
        assert_eq!(mgr.grace_period, Duration::seconds(300));
    }
}

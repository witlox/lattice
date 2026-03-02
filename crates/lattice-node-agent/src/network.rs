//! VNI (Virtual Network Identifier) management for network domains.
//!
//! Allocations sharing a network domain get L3 reachability via
//! Slingshot VNIs. This module tracks which VNIs are applied on
//! this node and manages apply/remove operations.

use std::collections::HashMap;

use lattice_common::types::AllocId;

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

/// Manages VNI assignments on a single node.
pub struct VniManager {
    assignments: HashMap<AllocId, VniConfig>,
}

impl VniManager {
    pub fn new() -> Self {
        Self {
            assignments: HashMap::new(),
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
        self.assignments.insert(config.allocation_id, config);
        Ok(())
    }

    /// Remove VNI assignment for an allocation.
    pub fn remove(&mut self, allocation_id: &AllocId) -> Option<VniConfig> {
        self.assignments.remove(allocation_id)
    }

    /// Get VNI config for an allocation.
    pub fn get(&self, allocation_id: &AllocId) -> Option<&VniConfig> {
        self.assignments.get(allocation_id)
    }

    /// Count active VNI assignments.
    pub fn active_count(&self) -> usize {
        self.assignments.len()
    }

    /// List all active VNIs.
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
}

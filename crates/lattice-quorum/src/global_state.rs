//! Global state managed by the Raft state machine.
//!
//! This is the authoritative state of the cluster, replicated across all quorum members.
//! Strong consistency for: node ownership, sensitive audit log.
//! Eventually consistent data (telemetry, metrics) is NOT stored here per ADR-004.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use lattice_common::error::LatticeError;
use lattice_common::traits::{AuditEntry, AuditFilter};
use lattice_common::types::{
    AllocId, Allocation, AllocationState, IsolationLevel, Node, NodeId, NodeOwnership, NodeState,
    Tenant, TenantId, TenantQuota, TopologyModel, VCluster, VClusterId,
};
use serde::{Deserialize, Serialize};

use crate::commands::{Command, CommandResponse};
use crate::TypeConfig;

/// The complete cluster state replicated via Raft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalState {
    pub nodes: HashMap<NodeId, Node>,
    pub allocations: HashMap<AllocId, Allocation>,
    pub tenants: HashMap<TenantId, Tenant>,
    pub vclusters: HashMap<VClusterId, VCluster>,
    pub topology: TopologyModel,
    pub audit_log: Vec<AuditEntry>,
    /// System-wide limit on nodes that can be claimed for sensitive use.
    /// None means unlimited (no sensitive pool cap).
    pub sensitive_pool_size: Option<u32>,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self {
            nodes: HashMap::new(),
            allocations: HashMap::new(),
            tenants: HashMap::new(),
            vclusters: HashMap::new(),
            topology: TopologyModel { groups: vec![] },
            audit_log: Vec::new(),
            sensitive_pool_size: None,
        }
    }
}

impl GlobalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a command to the state machine, returning a response.
    pub fn apply(&mut self, cmd: Command) -> CommandResponse {
        match cmd {
            Command::SubmitAllocation(alloc) => self.submit_allocation(alloc),
            Command::UpdateAllocationState {
                id,
                state,
                message,
                exit_code,
            } => self.update_allocation_state(id, state, message, exit_code),
            Command::AssignNodes { id, nodes } => self.assign_nodes(id, nodes),
            Command::RegisterNode(node) => self.register_node(node),
            Command::UpdateNodeState { id, state, reason } => {
                self.update_node_state(id, state, reason)
            }
            Command::ClaimNode { id, ownership } => self.claim_node(id, ownership),
            Command::ReleaseNode { id } => self.release_node(id),
            Command::RecordHeartbeat { id, timestamp } => self.record_heartbeat(id, timestamp),
            Command::CreateTenant(tenant) => self.create_tenant(tenant),
            Command::UpdateTenant {
                id,
                quota,
                isolation_level,
            } => self.update_tenant(id, quota, isolation_level),
            Command::CreateVCluster(vc) => self.create_vcluster(vc),
            Command::UpdateVCluster {
                id,
                cost_weights,
                allow_borrowing,
                allow_lending,
            } => self.update_vcluster(id, cost_weights, allow_borrowing, allow_lending),
            Command::UpdateTopology(topo) => {
                self.topology = topo;
                CommandResponse::Ok
            }
            Command::SetSensitivePoolSize(size) => {
                self.sensitive_pool_size = size;
                CommandResponse::Ok
            }
            Command::RecordAudit(entry) => {
                self.audit_log.push(entry);
                CommandResponse::Ok
            }
        }
    }

    // ── Allocation operations ───────────────────────────────

    fn submit_allocation(&mut self, alloc: Allocation) -> CommandResponse {
        // Hard quota check
        if let Err(e) = self.check_hard_quota(&alloc) {
            return CommandResponse::Error(e.to_string());
        }

        let id = alloc.id;
        self.allocations.insert(id, alloc);
        CommandResponse::AllocationId(id)
    }

    fn update_allocation_state(
        &mut self,
        id: AllocId,
        new_state: AllocationState,
        message: Option<String>,
        exit_code: Option<i32>,
    ) -> CommandResponse {
        let Some(alloc) = self.allocations.get_mut(&id) else {
            return CommandResponse::Error(format!("Allocation not found: {id}"));
        };

        if !alloc.state.can_transition_to(&new_state) {
            return CommandResponse::Error(format!(
                "Invalid transition from {:?} to {:?}",
                alloc.state, new_state
            ));
        }

        alloc.state = new_state.clone();
        if let Some(msg) = message {
            alloc.message = Some(msg);
        }
        if let Some(code) = exit_code {
            alloc.exit_code = Some(code);
        }

        // Update timestamps
        match new_state {
            AllocationState::Running => {
                alloc.started_at = Some(Utc::now());
            }
            AllocationState::Completed | AllocationState::Failed | AllocationState::Cancelled => {
                alloc.completed_at = Some(Utc::now());
            }
            _ => {}
        }

        CommandResponse::Ok
    }

    fn assign_nodes(&mut self, id: AllocId, nodes: Vec<NodeId>) -> CommandResponse {
        let Some(alloc) = self.allocations.get(&id) else {
            return CommandResponse::Error(format!("Allocation not found: {id}"));
        };
        let tenant_id = alloc.tenant.clone();
        let new_node_count = nodes.len() as u32;

        // Re-check tenant max_nodes quota before assigning
        if let Some(tenant) = self.tenants.get(&tenant_id) {
            let other_nodes_in_use: u32 = self
                .allocations
                .iter()
                .filter(|(aid, a)| **aid != id && a.tenant == tenant_id && !a.state.is_terminal())
                .map(|(_, a)| a.assigned_nodes.len() as u32)
                .sum();

            if other_nodes_in_use + new_node_count > tenant.quota.max_nodes {
                return CommandResponse::Error(format!(
                    "Quota exceeded for tenant {}: max_nodes ({}) would be exceeded: \
                     {} in use by other allocations + {} to assign",
                    tenant_id, tenant.quota.max_nodes, other_nodes_in_use, new_node_count
                ));
            }
        }

        let alloc = self.allocations.get_mut(&id).unwrap();
        alloc.assigned_nodes = nodes;
        CommandResponse::Ok
    }

    // ── Node operations ─────────────────────────────────────

    fn register_node(&mut self, node: Node) -> CommandResponse {
        let id = node.id.clone();
        self.nodes.insert(id.clone(), node);
        CommandResponse::NodeId(id)
    }

    fn update_node_state(
        &mut self,
        id: NodeId,
        new_state: NodeState,
        reason: Option<String>,
    ) -> CommandResponse {
        let Some(node) = self.nodes.get_mut(&id) else {
            return CommandResponse::Error(format!("Node not found: {id}"));
        };

        if !node.state.can_transition_to(&new_state) {
            return CommandResponse::Error(format!(
                "Invalid node transition from {:?} to {:?}",
                node.state, new_state
            ));
        }

        node.state = new_state;
        let _ = reason; // Could store in a reason field if needed
        CommandResponse::Ok
    }

    fn claim_node(&mut self, id: NodeId, ownership: NodeOwnership) -> CommandResponse {
        // Check node exists and ownership conflict (immutable borrows first)
        {
            let Some(node) = self.nodes.get(&id) else {
                return CommandResponse::Error(format!("Node not found: {id}"));
            };
            if let Some(ref existing) = node.owner {
                if existing.claimed_by.is_some() && ownership.claimed_by.is_some() {
                    return CommandResponse::Error(format!(
                        "Node {id} already claimed by {}",
                        existing.claimed_by.as_deref().unwrap_or("unknown")
                    ));
                }
            }
        }

        // Check sensitive_pool_size limit if this is a sensitive claim
        if ownership.claimed_by.is_some() {
            if let Some(pool_limit) = self.sensitive_pool_size {
                let currently_claimed = self
                    .nodes
                    .values()
                    .filter(|n| {
                        n.owner
                            .as_ref()
                            .map(|o| o.claimed_by.is_some())
                            .unwrap_or(false)
                    })
                    .count() as u32;

                if currently_claimed >= pool_limit {
                    return CommandResponse::Error(format!(
                        "sensitive_pool_size limit ({}) reached: {} nodes already claimed",
                        pool_limit, currently_claimed
                    ));
                }
            }
        }

        let node = self.nodes.get_mut(&id).unwrap();
        node.owner = Some(ownership);
        CommandResponse::Ok
    }

    fn release_node(&mut self, id: NodeId) -> CommandResponse {
        let Some(node) = self.nodes.get_mut(&id) else {
            return CommandResponse::Error(format!("Node not found: {id}"));
        };
        node.owner = None;
        CommandResponse::Ok
    }

    fn record_heartbeat(&mut self, id: NodeId, timestamp: DateTime<Utc>) -> CommandResponse {
        let Some(node) = self.nodes.get_mut(&id) else {
            return CommandResponse::Error(format!("Node not found: {id}"));
        };
        node.last_heartbeat = Some(timestamp);
        CommandResponse::Ok
    }

    // ── Tenant operations ───────────────────────────────────

    fn create_tenant(&mut self, tenant: Tenant) -> CommandResponse {
        let id = tenant.id.clone();
        self.tenants.insert(id.clone(), tenant);
        CommandResponse::TenantId(id)
    }

    fn update_tenant(
        &mut self,
        id: TenantId,
        quota: Option<TenantQuota>,
        isolation_level: Option<IsolationLevel>,
    ) -> CommandResponse {
        let Some(tenant) = self.tenants.get_mut(&id) else {
            return CommandResponse::Error(format!("Tenant not found: {id}"));
        };
        if let Some(q) = quota {
            tenant.quota = q;
        }
        if let Some(il) = isolation_level {
            tenant.isolation_level = il;
        }
        CommandResponse::Ok
    }

    // ── VCluster operations ─────────────────────────────────

    fn create_vcluster(&mut self, vc: VCluster) -> CommandResponse {
        let id = vc.id.clone();
        self.vclusters.insert(id.clone(), vc);
        CommandResponse::VClusterId(id)
    }

    fn update_vcluster(
        &mut self,
        id: VClusterId,
        cost_weights: Option<lattice_common::types::CostWeights>,
        allow_borrowing: Option<bool>,
        allow_lending: Option<bool>,
    ) -> CommandResponse {
        let Some(vc) = self.vclusters.get_mut(&id) else {
            return CommandResponse::Error(format!("VCluster not found: {id}"));
        };
        if let Some(cw) = cost_weights {
            vc.cost_weights = cw;
        }
        if let Some(b) = allow_borrowing {
            vc.allow_borrowing = b;
        }
        if let Some(l) = allow_lending {
            vc.allow_lending = l;
        }
        CommandResponse::Ok
    }

    // ── Quota enforcement ───────────────────────────────────

    /// Check hard quota limits before accepting an allocation.
    fn check_hard_quota(&self, alloc: &Allocation) -> Result<(), LatticeError> {
        let Some(tenant) = self.tenants.get(&alloc.tenant) else {
            // No tenant registered yet — allow (tenant may be created later)
            return Ok(());
        };

        // Check max_nodes quota using worst-case (max) for ranges
        let requested_nodes = match &alloc.resources.nodes {
            lattice_common::types::NodeCount::Exact(n) => *n,
            lattice_common::types::NodeCount::Range { max, .. } => *max,
        };
        let currently_used: u32 = self
            .allocations
            .values()
            .filter(|a| a.tenant == alloc.tenant && !a.state.is_terminal())
            .map(|a| a.assigned_nodes.len() as u32)
            .sum();

        if currently_used + requested_nodes > tenant.quota.max_nodes {
            return Err(LatticeError::QuotaExceeded {
                tenant: alloc.tenant.clone(),
                detail: format!(
                    "max_nodes quota ({}) would be exceeded: {} in use + {} requested",
                    tenant.quota.max_nodes, currently_used, requested_nodes
                ),
            });
        }

        // Check max_concurrent_allocations
        if let Some(max_concurrent) = tenant.quota.max_concurrent_allocations {
            let active_count = self
                .allocations
                .values()
                .filter(|a| a.tenant == alloc.tenant && !a.state.is_terminal())
                .count() as u32;

            if active_count >= max_concurrent {
                return Err(LatticeError::QuotaExceeded {
                    tenant: alloc.tenant.clone(),
                    detail: format!(
                        "max_concurrent_allocations quota ({}) reached: {} active",
                        max_concurrent, active_count
                    ),
                });
            }
        }

        Ok(())
    }
}

impl raft_hpc_core::StateMachineState<TypeConfig> for GlobalState {
    fn apply(&mut self, cmd: Command) -> CommandResponse {
        GlobalState::apply(self, cmd)
    }

    fn blank_response() -> CommandResponse {
        CommandResponse::Ok
    }
}

impl raft_hpc_core::BackupMetadataSource for GlobalState {
    type Metadata = LatticeBackupMeta;

    fn backup_metadata(&self) -> Self::Metadata {
        LatticeBackupMeta {
            node_count: self.nodes.len(),
            allocation_count: self.allocations.len(),
            tenant_count: self.tenants.len(),
            audit_entry_count: self.audit_log.len(),
        }
    }
}

/// Application-specific backup metadata for Lattice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeBackupMeta {
    pub node_count: usize,
    pub allocation_count: usize,
    pub tenant_count: usize,
    pub audit_entry_count: usize,
}

impl GlobalState {
    // ── Query helpers ───────────────────────────────────────

    pub fn query_audit(&self, filter: &AuditFilter) -> Vec<AuditEntry> {
        self.audit_log
            .iter()
            .filter(|e| {
                if let Some(ref user) = filter.user {
                    if &e.user != user {
                        return false;
                    }
                }
                if let Some(ref since) = filter.since {
                    if e.timestamp < *since {
                        return false;
                    }
                }
                if let Some(ref until) = filter.until {
                    if e.timestamp > *until {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::traits::AuditAction;
    use lattice_common::types::{AllocationState, NodeOwnership, NodeState, TenantQuota};
    use lattice_test_harness::fixtures::{
        AllocationBuilder, NodeBuilder, TenantBuilder, VClusterBuilder,
    };
    use uuid::Uuid;

    fn test_allocation(tenant: &str) -> Allocation {
        AllocationBuilder::new().tenant(tenant).build()
    }

    fn test_node(id: &str) -> Node {
        NodeBuilder::new().id(id).build()
    }

    #[test]
    fn submit_allocation_stores_it() {
        let mut state = GlobalState::new();
        let alloc = test_allocation("test-tenant");
        let id = alloc.id;
        let resp = state.apply(Command::SubmitAllocation(alloc));
        assert!(matches!(resp, CommandResponse::AllocationId(aid) if aid == id));
        assert!(state.allocations.contains_key(&id));
    }

    #[test]
    fn update_allocation_state_valid_transition() {
        let mut state = GlobalState::new();
        let alloc = test_allocation("test-tenant");
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));

        let resp = state.apply(Command::UpdateAllocationState {
            id,
            state: AllocationState::Running,
            message: None,
            exit_code: None,
        });
        assert!(matches!(resp, CommandResponse::Ok));
        assert_eq!(state.allocations[&id].state, AllocationState::Running);
        assert!(state.allocations[&id].started_at.is_some());
    }

    #[test]
    fn update_allocation_state_invalid_transition() {
        let mut state = GlobalState::new();
        let alloc = test_allocation("test-tenant");
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));

        let resp = state.apply(Command::UpdateAllocationState {
            id,
            state: AllocationState::Completed,
            message: None,
            exit_code: None,
        });
        assert!(matches!(resp, CommandResponse::Error(_)));
    }

    #[test]
    fn register_and_claim_node() {
        let mut state = GlobalState::new();
        let node = test_node("x1000c0s0b0n0");
        state.apply(Command::RegisterNode(node));

        let ownership = NodeOwnership {
            tenant: "med-tenant".into(),
            vcluster: "med-vc".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("dr-smith".into()),
            is_borrowed: false,
        };
        let resp = state.apply(Command::ClaimNode {
            id: "x1000c0s0b0n0".into(),
            ownership,
        });
        assert!(matches!(resp, CommandResponse::Ok));

        let node = &state.nodes["x1000c0s0b0n0"];
        assert!(node.owner.is_some());
        assert_eq!(
            node.owner.as_ref().unwrap().claimed_by.as_deref(),
            Some("dr-smith")
        );
    }

    #[test]
    fn claim_conflict_detected() {
        let mut state = GlobalState::new();
        let node = test_node("x1000c0s0b0n0");
        state.apply(Command::RegisterNode(node));

        let ownership1 = NodeOwnership {
            tenant: "tenant-1".into(),
            vcluster: "vc-1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-1".into()),
            is_borrowed: false,
        };
        state.apply(Command::ClaimNode {
            id: "x1000c0s0b0n0".into(),
            ownership: ownership1,
        });

        let ownership2 = NodeOwnership {
            tenant: "tenant-2".into(),
            vcluster: "vc-2".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-2".into()),
            is_borrowed: false,
        };
        let resp = state.apply(Command::ClaimNode {
            id: "x1000c0s0b0n0".into(),
            ownership: ownership2,
        });
        assert!(matches!(resp, CommandResponse::Error(_)));
    }

    #[test]
    fn release_node_clears_ownership() {
        let mut state = GlobalState::new();
        let node = test_node("x1000c0s0b0n0");
        state.apply(Command::RegisterNode(node));

        let ownership = NodeOwnership {
            tenant: "tenant-1".into(),
            vcluster: "vc-1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-1".into()),
            is_borrowed: false,
        };
        state.apply(Command::ClaimNode {
            id: "x1000c0s0b0n0".into(),
            ownership,
        });

        state.apply(Command::ReleaseNode {
            id: "x1000c0s0b0n0".into(),
        });
        assert!(state.nodes["x1000c0s0b0n0"].owner.is_none());
    }

    #[test]
    fn sensitive_pool_size_limits_claims() {
        let mut state = GlobalState::new();
        state.apply(Command::SetSensitivePoolSize(Some(1)));
        state.apply(Command::RegisterNode(test_node("n1")));
        state.apply(Command::RegisterNode(test_node("n2")));

        let ownership1 = NodeOwnership {
            tenant: "t1".into(),
            vcluster: "vc1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-1".into()),
            is_borrowed: false,
        };
        let resp = state.apply(Command::ClaimNode {
            id: "n1".into(),
            ownership: ownership1,
        });
        assert!(matches!(resp, CommandResponse::Ok));

        // Second sensitive claim should be rejected (pool_size=1)
        let ownership2 = NodeOwnership {
            tenant: "t2".into(),
            vcluster: "vc2".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-2".into()),
            is_borrowed: false,
        };
        let resp = state.apply(Command::ClaimNode {
            id: "n2".into(),
            ownership: ownership2,
        });
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("sensitive_pool_size")));
    }

    #[test]
    fn sensitive_pool_unlimited_by_default() {
        let mut state = GlobalState::new();
        // No pool size set → unlimited
        state.apply(Command::RegisterNode(test_node("n1")));
        state.apply(Command::RegisterNode(test_node("n2")));

        for (node_id, user) in [("n1", "user-1"), ("n2", "user-2")] {
            let ownership = NodeOwnership {
                tenant: "t1".into(),
                vcluster: "vc1".into(),
                allocation: Uuid::new_v4(),
                claimed_by: Some(user.into()),
                is_borrowed: false,
            };
            let resp = state.apply(Command::ClaimNode {
                id: node_id.into(),
                ownership,
            });
            assert!(matches!(resp, CommandResponse::Ok));
        }
    }

    #[test]
    fn non_sensitive_claim_bypasses_pool_limit() {
        let mut state = GlobalState::new();
        state.apply(Command::SetSensitivePoolSize(Some(0)));
        state.apply(Command::RegisterNode(test_node("n1")));

        // Non-sensitive claim (claimed_by is None) should still work
        let ownership = NodeOwnership {
            tenant: "t1".into(),
            vcluster: "vc1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: None,
            is_borrowed: false,
        };
        let resp = state.apply(Command::ClaimNode {
            id: "n1".into(),
            ownership,
        });
        assert!(matches!(resp, CommandResponse::Ok));
    }

    #[test]
    fn hard_quota_blocks_over_limit() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("limited").max_nodes(2).build();
        state.apply(Command::CreateTenant(tenant));

        // Submit allocation requesting 1 node, assign 2 nodes
        let mut alloc1 = test_allocation("limited");
        alloc1.assigned_nodes = vec!["n1".into(), "n2".into()];
        alloc1.state = AllocationState::Running;
        state.allocations.insert(alloc1.id, alloc1);

        // Try to submit another — quota exceeded
        let alloc2 = test_allocation("limited");
        let resp = state.apply(Command::SubmitAllocation(alloc2));
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("max_nodes")));
    }

    #[test]
    fn concurrent_allocation_limit() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("limited")
            .max_nodes(100)
            .max_concurrent(2)
            .build();
        state.apply(Command::CreateTenant(tenant));

        // Submit 2 allocations
        state.apply(Command::SubmitAllocation(test_allocation("limited")));
        state.apply(Command::SubmitAllocation(test_allocation("limited")));

        // Third should fail
        let resp = state.apply(Command::SubmitAllocation(test_allocation("limited")));
        assert!(
            matches!(resp, CommandResponse::Error(e) if e.contains("max_concurrent_allocations"))
        );
    }

    #[test]
    fn audit_log_append_and_query() {
        let mut state = GlobalState::new();
        let entry = AuditEntry {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            user: "dr-smith".into(),
            action: AuditAction::NodeClaim,
            details: serde_json::json!({"node": "x1000c0s0b0n0"}),
        };
        state.apply(Command::RecordAudit(entry.clone()));

        let results = state.query_audit(&AuditFilter {
            user: Some("dr-smith".into()),
            ..Default::default()
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, AuditAction::NodeClaim);
    }

    #[test]
    fn node_state_transition_validated() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(test_node("n1")));

        // Ready → Draining is valid
        let resp = state.apply(Command::UpdateNodeState {
            id: "n1".into(),
            state: NodeState::Draining,
            reason: Some("maintenance".into()),
        });
        assert!(matches!(resp, CommandResponse::Ok));

        // Draining → Ready is invalid
        let resp = state.apply(Command::UpdateNodeState {
            id: "n1".into(),
            state: NodeState::Ready,
            reason: None,
        });
        assert!(matches!(resp, CommandResponse::Error(_)));
    }

    #[test]
    fn heartbeat_updates_timestamp() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(test_node("n1")));

        let ts = Utc::now();
        state.apply(Command::RecordHeartbeat {
            id: "n1".into(),
            timestamp: ts,
        });

        assert_eq!(state.nodes["n1"].last_heartbeat, Some(ts));
    }

    #[test]
    fn create_and_update_tenant() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("test-tenant").build();
        let tenant_id = tenant.id.clone();
        state.apply(Command::CreateTenant(tenant));

        state.apply(Command::UpdateTenant {
            id: tenant_id.clone(),
            quota: Some(TenantQuota {
                max_nodes: 20,
                fair_share_target: 0.2,
                gpu_hours_budget: None,
                max_concurrent_allocations: None,
                burst_allowance: None,
            }),
            isolation_level: None,
        });

        assert_eq!(state.tenants[&tenant_id].quota.max_nodes, 20);
    }

    #[test]
    fn create_and_update_vcluster() {
        let mut state = GlobalState::new();
        let vc = VClusterBuilder::new("vc-1").build();
        let vc_id = vc.id.clone();
        state.apply(Command::CreateVCluster(vc));

        state.apply(Command::UpdateVCluster {
            id: vc_id.clone(),
            cost_weights: None,
            allow_borrowing: Some(false),
            allow_lending: None,
        });

        assert!(!state.vclusters[&vc_id].allow_borrowing);
    }

    #[test]
    fn completed_allocation_sets_timestamp() {
        let mut state = GlobalState::new();
        let alloc = test_allocation("test-tenant");
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));

        state.apply(Command::UpdateAllocationState {
            id,
            state: AllocationState::Running,
            message: None,
            exit_code: None,
        });

        state.apply(Command::UpdateAllocationState {
            id,
            state: AllocationState::Completed,
            message: Some("done".into()),
            exit_code: Some(0),
        });

        let a = &state.allocations[&id];
        assert!(a.completed_at.is_some());
        assert_eq!(a.exit_code, Some(0));
        assert_eq!(a.message.as_deref(), Some("done"));
    }

    #[test]
    fn assign_nodes_to_allocation() {
        let mut state = GlobalState::new();
        let alloc = test_allocation("test-tenant");
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));

        state.apply(Command::AssignNodes {
            id,
            nodes: vec!["n1".into(), "n2".into()],
        });

        assert_eq!(state.allocations[&id].assigned_nodes, vec!["n1", "n2"]);
    }

    #[test]
    fn assign_nodes_checks_tenant_quota() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("t1").max_nodes(3).build();
        state.apply(Command::CreateTenant(tenant));

        // First allocation: assign 2 nodes
        let alloc1 = AllocationBuilder::new().tenant("t1").build();
        let id1 = alloc1.id;
        state.apply(Command::SubmitAllocation(alloc1));
        state.apply(Command::AssignNodes {
            id: id1,
            nodes: vec!["n1".into(), "n2".into()],
        });

        // Second allocation: assigning 2 more would exceed quota of 3
        let alloc2 = AllocationBuilder::new().tenant("t1").build();
        let id2 = alloc2.id;
        state.apply(Command::SubmitAllocation(alloc2));
        let resp = state.apply(Command::AssignNodes {
            id: id2,
            nodes: vec!["n3".into(), "n4".into()],
        });
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("max_nodes")));
        // Original assignment unchanged
        assert!(state.allocations[&id2].assigned_nodes.is_empty());
    }

    #[test]
    fn submit_with_range_checks_max_not_min() {
        use lattice_common::types::NodeCount;
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("t1").max_nodes(5).build();
        state.apply(Command::CreateTenant(tenant));

        // Submit with range min=1, max=10 — should be rejected because max(10) > quota(5)
        let mut alloc = AllocationBuilder::new().tenant("t1").build();
        alloc.resources.nodes = NodeCount::Range { min: 1, max: 10 };
        let resp = state.apply(Command::SubmitAllocation(alloc));
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("max_nodes")));
    }

    #[test]
    fn assign_nodes_within_quota_succeeds() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("t1").max_nodes(5).build();
        state.apply(Command::CreateTenant(tenant));

        let alloc = AllocationBuilder::new().tenant("t1").build();
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));
        let resp = state.apply(Command::AssignNodes {
            id,
            nodes: vec!["n1".into(), "n2".into(), "n3".into()],
        });
        assert!(matches!(resp, CommandResponse::Ok));
        assert_eq!(state.allocations[&id].assigned_nodes.len(), 3);
    }
}

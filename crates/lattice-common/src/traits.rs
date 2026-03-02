use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::LatticeError;
use crate::types::{
    AllocId, Allocation, AllocationState, GroupId, Node, NodeId, NodeOwnership, NodeState,
    SchedulerType, TenantId, UserId, VClusterId,
};

// ─── External Service Traits ────────────────────────────────

/// Integration with VAST/Lustre storage systems.
#[async_trait]
pub trait StorageService: Send + Sync {
    /// Returns a readiness score [0.0, 1.0] indicating how much data is on hot tier.
    async fn data_readiness(&self, source: &str) -> Result<f64, LatticeError>;

    /// Pre-stage data from source to target path.
    async fn stage_data(&self, source: &str, target: &str) -> Result<(), LatticeError>;

    /// Set QoS floor bandwidth for a storage path.
    async fn set_qos(&self, path: &str, floor_gbps: f64) -> Result<(), LatticeError>;

    /// Securely wipe data at path (for medical workload teardown).
    async fn wipe_data(&self, path: &str) -> Result<(), LatticeError>;
}

/// Integration with OpenCHAMI infrastructure management.
#[async_trait]
pub trait InfrastructureService: Send + Sync {
    /// Boot or reimage a node with the specified OS image.
    async fn boot_node(&self, node_id: &NodeId, image: &str) -> Result<(), LatticeError>;

    /// Wipe a node (for medical workload teardown).
    async fn wipe_node(&self, node_id: &NodeId) -> Result<(), LatticeError>;

    /// Query current health status from BMC/agent.
    async fn query_node_health(&self, node_id: &NodeId) -> Result<NodeHealthReport, LatticeError>;
}

/// Integration with Waldur external accounting.
#[async_trait]
pub trait AccountingService: Send + Sync {
    /// Report allocation start for billing.
    async fn report_start(&self, allocation: &Allocation) -> Result<(), LatticeError>;

    /// Report allocation completion for billing.
    async fn report_completion(&self, allocation: &Allocation) -> Result<(), LatticeError>;

    /// Query remaining budget for a tenant (None = unlimited).
    async fn remaining_budget(&self, tenant: &TenantId) -> Result<Option<f64>, LatticeError>;
}

// ─── Internal Component Traits ──────────────────────────────

/// Registry of compute nodes and their states.
#[async_trait]
pub trait NodeRegistry: Send + Sync {
    /// Get a single node by ID.
    async fn get_node(&self, id: &NodeId) -> Result<Node, LatticeError>;

    /// List nodes matching a filter.
    async fn list_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, LatticeError>;

    /// Update a node's state (e.g., Ready → Draining).
    async fn update_node_state(&self, id: &NodeId, state: NodeState) -> Result<(), LatticeError>;

    /// Claim a node for an allocation (sets ownership).
    async fn claim_node(&self, id: &NodeId, ownership: NodeOwnership) -> Result<(), LatticeError>;

    /// Release a node (clears ownership).
    async fn release_node(&self, id: &NodeId) -> Result<(), LatticeError>;
}

/// Persistent store for allocations.
#[async_trait]
pub trait AllocationStore: Send + Sync {
    /// Insert a new allocation.
    async fn insert(&self, allocation: Allocation) -> Result<(), LatticeError>;

    /// Get an allocation by ID.
    async fn get(&self, id: &AllocId) -> Result<Allocation, LatticeError>;

    /// Update an allocation's state.
    async fn update_state(&self, id: &AllocId, state: AllocationState) -> Result<(), LatticeError>;

    /// List allocations matching a filter.
    async fn list(&self, filter: &AllocationFilter) -> Result<Vec<Allocation>, LatticeError>;

    /// Count running allocations for a tenant.
    async fn count_running(&self, tenant: &TenantId) -> Result<u32, LatticeError>;
}

/// Audit log for compliance (medical workloads, security events).
#[async_trait]
pub trait AuditLog: Send + Sync {
    /// Record an audit entry.
    async fn record(&self, entry: AuditEntry) -> Result<(), LatticeError>;

    /// Query audit entries matching a filter.
    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>, LatticeError>;
}

/// Per-vCluster scheduling strategy.
#[async_trait]
pub trait VClusterScheduler: Send + Sync {
    /// Propose placements for pending allocations given available nodes.
    async fn schedule(
        &self,
        pending: &[Allocation],
        nodes: &[Node],
    ) -> Result<Vec<Placement>, LatticeError>;

    /// The type of scheduler (for identification/logging).
    fn scheduler_type(&self) -> SchedulerType;
}

/// Checkpoint coordination broker.
#[async_trait]
pub trait CheckpointBroker: Send + Sync {
    /// Evaluate whether an allocation should checkpoint now.
    async fn should_checkpoint(&self, allocation: &Allocation) -> Result<bool, LatticeError>;

    /// Initiate a checkpoint for the given allocation.
    async fn initiate_checkpoint(&self, id: &AllocId) -> Result<(), LatticeError>;
}

// ─── Supporting Types ───────────────────────────────────────

/// Health report from infrastructure management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHealthReport {
    pub healthy: bool,
    pub issues: Vec<String>,
}

/// Filter criteria for node queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeFilter {
    pub state: Option<NodeState>,
    pub group: Option<GroupId>,
    pub tenant: Option<TenantId>,
}

/// Filter criteria for allocation queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AllocationFilter {
    pub user: Option<UserId>,
    pub tenant: Option<TenantId>,
    pub state: Option<AllocationState>,
    pub vcluster: Option<VClusterId>,
}

/// A single audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub user: UserId,
    pub action: AuditAction,
    pub details: serde_json::Value,
}

/// Categories of auditable actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    NodeClaim,
    NodeRelease,
    AllocationStart,
    AllocationComplete,
    DataAccess,
    AttachSession,
    LogAccess,
    MetricsQuery,
    CheckpointEvent,
}

/// Filter criteria for audit log queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFilter {
    pub user: Option<UserId>,
    pub allocation: Option<AllocId>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

/// A scheduler's proposed placement of an allocation onto nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Placement {
    pub allocation_id: AllocId,
    pub nodes: Vec<NodeId>,
}

/// Invalid state transition error detail.
#[derive(Debug, Clone)]
pub struct InvalidTransition {
    pub from: String,
    pub to: String,
}

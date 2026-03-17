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

    /// Securely wipe data at path (for sensitive workload teardown).
    async fn wipe_data(&self, path: &str) -> Result<(), LatticeError>;
}

/// Integration with OpenCHAMI infrastructure management.
#[async_trait]
pub trait InfrastructureService: Send + Sync {
    /// Boot or reimage a node with the specified OS image.
    async fn boot_node(&self, node_id: &NodeId, image: &str) -> Result<(), LatticeError>;

    /// Wipe a node (for sensitive workload teardown).
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

/// Summary of an archived audit log chunk stored in external storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditArchiveInfo {
    /// S3 object key where the archived entries are stored.
    pub object_key: String,
    /// Number of entries in this archive chunk.
    pub entry_count: usize,
    /// Timestamp of the first entry in the chunk.
    pub first_timestamp: DateTime<Utc>,
    /// Timestamp of the last entry in the chunk.
    pub last_timestamp: DateTime<Utc>,
}

/// Audit log for compliance (sensitive workloads, security events).
#[async_trait]
pub trait AuditLog: Send + Sync {
    /// Record an audit entry.
    async fn record(&self, entry: AuditEntry) -> Result<(), LatticeError>;

    /// Query audit entries matching a filter.
    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>, LatticeError>;

    /// Return metadata about archived audit log chunks.
    /// Returns an empty vec if no archival has occurred.
    async fn archive_info(&self) -> Result<Vec<AuditArchiveInfo>, LatticeError>;

    /// Total number of audit entries including both in-memory and archived.
    async fn total_entry_count(&self) -> Result<usize, LatticeError>;
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

/// A single audit log entry with cryptographic integrity chain (ADV-03).
///
/// Wraps `hpc_audit::AuditEvent` as the standardized payload, adding
/// hash chaining and ed25519 signing for tamper evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Standardized audit event (hpc-audit format: who, what, when, where, outcome).
    pub event: hpc_audit::AuditEvent,
    /// SHA-256 hash of the previous audit entry (hex string).
    /// Empty string for the first entry in the chain.
    #[serde(default)]
    pub previous_hash: String,
    /// Ed25519 signature over (event + previous_hash).
    /// Empty until signed by the quorum on commit.
    #[serde(default)]
    pub signature: String,
}

impl AuditEntry {
    /// Create a new unsigned audit entry (previous_hash and signature filled by quorum).
    pub fn new(event: hpc_audit::AuditEvent) -> Self {
        Self {
            event,
            previous_hash: String::new(),
            signature: String::new(),
        }
    }

    /// Convenience: access the action string.
    pub fn action(&self) -> &str {
        &self.event.action
    }

    /// Convenience: access the principal identity.
    pub fn principal_identity(&self) -> &str {
        &self.event.principal.identity
    }
}

/// Lattice-specific audit action constants (supplement hpc_audit::actions).
///
/// Shared actions (cgroup, namespace, mount, workload lifecycle) use
/// hpc_audit::actions constants directly. These lattice-prefixed constants
/// cover scheduling, sensitive workloads, and lattice-specific operations.
pub mod audit_actions {
    // Re-export shared actions for convenience
    pub use hpc_audit::actions::*;

    // Node ownership (sensitive audit path)
    pub const NODE_CLAIM: &str = "lattice.node.claim";
    pub const NODE_RELEASE: &str = "lattice.node.release";

    // Allocation lifecycle (extends hpc-audit workload actions)
    pub const ALLOCATION_COMPLETE: &str = "lattice.allocation.complete";
    pub const ALLOCATION_FAILED: &str = "lattice.allocation.failed";
    pub const ALLOCATION_CANCELLED: &str = "lattice.allocation.cancelled";
    pub const ALLOCATION_REQUEUED: &str = "lattice.allocation.requeued";
    pub const ALLOCATION_SUSPENDED: &str = "lattice.allocation.suspended";

    // Sensitive workload operations
    pub const DATA_ACCESS: &str = "lattice.data.access";
    pub const ATTACH_SESSION: &str = "lattice.attach.session";
    pub const LOG_ACCESS: &str = "lattice.log.access";
    pub const METRICS_QUERY: &str = "lattice.metrics.query";

    // Secure wipe
    pub const WIPE_STARTED: &str = "lattice.wipe.started";
    pub const WIPE_COMPLETED: &str = "lattice.wipe.completed";
    pub const WIPE_FAILED: &str = "lattice.wipe.failed";

    // Scheduling
    pub const PROPOSAL_COMMITTED: &str = "lattice.scheduling.proposal_committed";
    pub const PROPOSAL_REJECTED: &str = "lattice.scheduling.proposal_rejected";
    pub const PREEMPTION_INITIATED: &str = "lattice.scheduling.preemption_initiated";

    // Quota
    pub const QUOTA_EXCEEDED: &str = "lattice.quota.exceeded";
    pub const QUOTA_UPDATED: &str = "lattice.quota.updated";

    // DAG
    pub const DAG_SUBMITTED: &str = "lattice.dag.submitted";
    pub const DAG_COMPLETED: &str = "lattice.dag.completed";

    // VNI / Network domain
    pub const VNI_ASSIGNED: &str = "lattice.network.vni_assigned";
    pub const VNI_RELEASED: &str = "lattice.network.vni_released";
}

/// Helper to construct an `hpc_audit::AuditEvent` for lattice components.
pub fn lattice_audit_event(
    action: &str,
    principal_identity: &str,
    scope: hpc_audit::AuditScope,
    outcome: hpc_audit::AuditOutcome,
    detail: &str,
    metadata: serde_json::Value,
    source: hpc_audit::AuditSource,
) -> hpc_audit::AuditEvent {
    hpc_audit::AuditEvent {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        principal: hpc_audit::AuditPrincipal {
            identity: principal_identity.to_string(),
            principal_type: hpc_audit::PrincipalType::Human,
            role: String::new(),
        },
        action: action.to_string(),
        scope,
        outcome,
        detail: detail.to_string(),
        metadata,
        source,
    }
}

/// Filter criteria for audit log queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFilter {
    /// Filter by principal identity (OIDC subject or service account).
    pub principal: Option<String>,
    /// Filter by allocation ID (matches scope.allocation_id).
    pub allocation: Option<AllocId>,
    /// Filter by action string (exact match).
    pub action: Option<String>,
    /// Filter entries after this timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Filter entries before this timestamp.
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

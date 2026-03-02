use thiserror::Error;

#[derive(Error, Debug)]
pub enum LatticeError {
    #[error("Allocation not found: {0}")]
    AllocationNotFound(String),

    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Tenant not found: {0}")]
    TenantNotFound(String),

    #[error("VCluster not found: {0}")]
    VClusterNotFound(String),

    #[error("Resource constraint violation: {0}")]
    ResourceConstraint(String),

    #[error("Topology constraint violation: {0}")]
    TopologyConstraint(String),

    #[error("Quota exceeded for tenant {tenant}: {detail}")]
    QuotaExceeded { tenant: String, detail: String },

    #[error("Medical isolation violation: {0}")]
    MedicalIsolation(String),

    #[error("Node ownership conflict: node {node} already owned by {owner}")]
    OwnershipConflict { node: String, owner: String },

    #[error("Dependency not satisfied: {0}")]
    DependencyNotSatisfied(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Authorization denied: {0}")]
    AuthorizationDenied(String),

    #[error("Quorum error: {0}")]
    QuorumError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Internal error: {0}")]
    Internal(String),

    // ─── Observability errors ───────────────────────────────
    #[error("Cannot attach to allocation {allocation}: state is {state}, expected Running")]
    AttachNotRunning { allocation: String, state: String },

    #[error("Attach denied: user {user} is not authorized to attach to allocation {allocation}")]
    AttachDenied { user: String, allocation: String },

    #[error("Logs not available for allocation {allocation}")]
    LogsNotAvailable { allocation: String },

    #[error("Metrics query failed: {0}")]
    MetricsQueryFailed(String),

    #[error("Diagnostics not available for allocation {allocation}: allocation must be in Running state")]
    DiagnosticsNotAvailable { allocation: String },

    #[error("Comparison failed: {0}")]
    CompareFailed(String),
}

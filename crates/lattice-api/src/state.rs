//! Shared API server state.
//!
//! All gRPC and REST handlers share this state via `Arc<ApiState>`.

use std::sync::Arc;

use lattice_common::clients::SovraClient;
use lattice_common::traits::{
    AccountingService, AllocationStore, AuditLog, CheckpointBroker, NodeRegistry, StorageService,
};
use lattice_common::tsdb_client::TsdbClient;
use lattice_node_agent::pty::PtyBackend;

use crate::events::EventBus;
use crate::middleware::cert_san::{default_dev_validator, SharedSanValidator};
use crate::middleware::oidc::{OidcConfig, OidcValidator};
use crate::middleware::rate_limit::RateLimiter;
use crate::mpi::NodeAgentPool;

/// Shared state for the API server, holding trait-object references
/// to the backing stores and services.
pub struct ApiState {
    pub allocations: Arc<dyn AllocationStore>,
    pub nodes: Arc<dyn NodeRegistry>,
    pub audit: Arc<dyn AuditLog>,
    pub checkpoint: Arc<dyn CheckpointBroker>,
    /// Optional quorum client for Raft-committed mutations.
    /// When present, tenant/vCluster operations go through Raft.
    pub quorum: Option<Arc<lattice_quorum::QuorumClient>>,
    /// Data directory for persistent Raft storage (needed for backup/restore).
    pub data_dir: Option<std::path::PathBuf>,
    /// Event bus for streaming RPCs (watch, stream_logs, stream_metrics).
    pub events: Arc<EventBus>,
    /// Optional TSDB client for metrics push/query (e.g., VictoriaMetrics).
    pub tsdb: Option<Arc<dyn TsdbClient>>,
    /// Optional VAST/Lustre storage integration for data staging, QoS, wipe.
    pub storage: Option<Arc<dyn StorageService>>,
    /// Optional Waldur accounting integration for billing/resource tracking.
    pub accounting: Option<Arc<dyn AccountingService>>,
    /// Optional OIDC token validator for authentication.
    pub oidc: Option<Arc<dyn OidcValidator>>,
    /// Optional per-user rate limiter.
    pub rate_limiter: Option<Arc<RateLimiter>>,
    /// Optional Sovra federation client for cross-site credential exchange.
    pub sovra: Option<Arc<dyn SovraClient>>,
    /// Optional PTY backend for interactive attach sessions.
    pub pty: Option<Arc<dyn PtyBackend>>,
    /// Optional node agent pool for MPI launch fan-out.
    pub agent_pool: Option<Arc<dyn NodeAgentPool>>,
    /// Optional OIDC configuration for auth discovery endpoint.
    pub oidc_config: Option<OidcConfig>,
    /// Cert-SAN validator for INV-D14. Dev/test deployments use
    /// `AllowAllSanValidator`; production with mTLS uses
    /// `BoundToPeerCertSanValidator`. Always present (never Option)
    /// so handlers have a validator to call unconditionally.
    pub san_validator: SharedSanValidator,
}

impl ApiState {
    /// Convenience: construct a fresh dev-mode SAN validator. Used by
    /// existing test harnesses that build `ApiState` struct-literal-style;
    /// production code injects `bound_validator(false)`.
    pub fn default_dev_san_validator() -> SharedSanValidator {
        default_dev_validator()
    }
}

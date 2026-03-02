//! Shared API server state.
//!
//! All gRPC and REST handlers share this state via `Arc<ApiState>`.

use std::sync::Arc;

use lattice_common::traits::{AllocationStore, AuditLog, CheckpointBroker, NodeRegistry};

use crate::events::EventBus;

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
    /// Event bus for streaming RPCs (watch, stream_logs, stream_metrics).
    pub events: Arc<EventBus>,
}

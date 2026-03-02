//! Shared API server state.
//!
//! All gRPC and REST handlers share this state via `Arc<ApiState>`.

use std::sync::Arc;

use lattice_common::traits::{AllocationStore, AuditLog, CheckpointBroker, NodeRegistry};

/// Shared state for the API server, holding trait-object references
/// to the backing stores and services.
pub struct ApiState {
    pub allocations: Arc<dyn AllocationStore>,
    pub nodes: Arc<dyn NodeRegistry>,
    pub audit: Arc<dyn AuditLog>,
    pub checkpoint: Arc<dyn CheckpointBroker>,
}

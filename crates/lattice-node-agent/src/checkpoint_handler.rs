//! Checkpoint signal handler — receives checkpoint hints from the
//! broker and forwards them to application processes.
//!
//! Three communication modes:
//! 1. Signal: Send SIGUSR1 to the process
//! 2. Shared memory: Write flag to a known shmem location
//! 3. gRPC callback: Call the application's checkpoint endpoint

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use lattice_common::types::AllocId;

/// How the checkpoint signal is delivered to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointMode {
    Signal,
    SharedMemory,
    GrpcCallback { endpoint: String },
}

/// Status of a checkpoint operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointStatus {
    Pending,
    InProgress,
    Completed,
    Failed { reason: String },
}

/// A checkpoint request for a specific allocation.
#[derive(Debug, Clone)]
pub struct CheckpointRequest {
    pub allocation_id: AllocId,
    pub mode: CheckpointMode,
    pub requested_at: DateTime<Utc>,
    pub status: CheckpointStatus,
}

/// Manages checkpoint signaling for allocations on this node.
pub struct CheckpointHandler {
    requests: HashMap<AllocId, Vec<CheckpointRequest>>,
}

impl CheckpointHandler {
    pub fn new() -> Self {
        Self {
            requests: HashMap::new(),
        }
    }

    /// Queue a checkpoint request for an allocation.
    pub fn request_checkpoint(&mut self, allocation_id: AllocId, mode: CheckpointMode) {
        let req = CheckpointRequest {
            allocation_id,
            mode,
            requested_at: Utc::now(),
            status: CheckpointStatus::Pending,
        };
        self.requests.entry(allocation_id).or_default().push(req);
    }

    /// Get pending checkpoint requests for an allocation.
    pub fn pending_for(&self, allocation_id: &AllocId) -> Vec<&CheckpointRequest> {
        self.requests
            .get(allocation_id)
            .map(|reqs| {
                reqs.iter()
                    .filter(|r| r.status == CheckpointStatus::Pending)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Mark a checkpoint as in progress.
    pub fn mark_in_progress(&mut self, allocation_id: &AllocId) -> bool {
        if let Some(reqs) = self.requests.get_mut(allocation_id) {
            if let Some(req) = reqs
                .iter_mut()
                .find(|r| r.status == CheckpointStatus::Pending)
            {
                req.status = CheckpointStatus::InProgress;
                return true;
            }
        }
        false
    }

    /// Mark the current in-progress checkpoint as completed.
    pub fn mark_completed(&mut self, allocation_id: &AllocId) -> bool {
        if let Some(reqs) = self.requests.get_mut(allocation_id) {
            if let Some(req) = reqs
                .iter_mut()
                .find(|r| r.status == CheckpointStatus::InProgress)
            {
                req.status = CheckpointStatus::Completed;
                return true;
            }
        }
        false
    }

    /// Mark the current in-progress checkpoint as failed.
    pub fn mark_failed(&mut self, allocation_id: &AllocId, reason: String) -> bool {
        if let Some(reqs) = self.requests.get_mut(allocation_id) {
            if let Some(req) = reqs
                .iter_mut()
                .find(|r| r.status == CheckpointStatus::InProgress)
            {
                req.status = CheckpointStatus::Failed { reason };
                return true;
            }
        }
        false
    }

    /// Count completed checkpoints for an allocation.
    pub fn completed_count(&self, allocation_id: &AllocId) -> usize {
        self.requests
            .get(allocation_id)
            .map(|reqs| {
                reqs.iter()
                    .filter(|r| r.status == CheckpointStatus::Completed)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Remove all checkpoint history for an allocation (after cleanup).
    pub fn clear(&mut self, allocation_id: &AllocId) {
        self.requests.remove(allocation_id);
    }
}

impl Default for CheckpointHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_retrieve_checkpoint() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();

        handler.request_checkpoint(id, CheckpointMode::Signal);
        let pending = handler.pending_for(&id);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].mode, CheckpointMode::Signal);
    }

    #[test]
    fn checkpoint_lifecycle() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();

        handler.request_checkpoint(id, CheckpointMode::SharedMemory);
        assert_eq!(handler.pending_for(&id).len(), 1);

        assert!(handler.mark_in_progress(&id));
        assert_eq!(handler.pending_for(&id).len(), 0);

        assert!(handler.mark_completed(&id));
        assert_eq!(handler.completed_count(&id), 1);
    }

    #[test]
    fn checkpoint_failure() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();

        handler.request_checkpoint(id, CheckpointMode::Signal);
        handler.mark_in_progress(&id);
        handler.mark_failed(&id, "disk full".to_string());

        assert_eq!(handler.completed_count(&id), 0);
        assert_eq!(handler.pending_for(&id).len(), 0);
    }

    #[test]
    fn multiple_checkpoints() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();

        handler.request_checkpoint(id, CheckpointMode::Signal);
        handler.mark_in_progress(&id);
        handler.mark_completed(&id);

        handler.request_checkpoint(id, CheckpointMode::Signal);
        handler.mark_in_progress(&id);
        handler.mark_completed(&id);

        assert_eq!(handler.completed_count(&id), 2);
    }

    #[test]
    fn grpc_callback_mode() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();

        handler.request_checkpoint(
            id,
            CheckpointMode::GrpcCallback {
                endpoint: "localhost:9090".to_string(),
            },
        );
        let pending = handler.pending_for(&id);
        assert_eq!(
            pending[0].mode,
            CheckpointMode::GrpcCallback {
                endpoint: "localhost:9090".to_string()
            }
        );
    }

    #[test]
    fn clear_removes_history() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();

        handler.request_checkpoint(id, CheckpointMode::Signal);
        handler.mark_in_progress(&id);
        handler.mark_completed(&id);

        handler.clear(&id);
        assert_eq!(handler.completed_count(&id), 0);
        assert_eq!(handler.pending_for(&id).len(), 0);
    }

    #[test]
    fn mark_in_progress_when_no_pending_returns_false() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();
        assert!(!handler.mark_in_progress(&id));
    }

    #[test]
    fn mark_completed_when_none_in_progress_returns_false() {
        let mut handler = CheckpointHandler::new();
        let id = uuid::Uuid::new_v4();
        handler.request_checkpoint(id, CheckpointMode::Signal);
        // Don't mark in_progress first
        assert!(!handler.mark_completed(&id));
    }
}

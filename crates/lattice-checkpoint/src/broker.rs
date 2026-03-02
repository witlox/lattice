//! CheckpointBroker implementation.
//!
//! Evaluates the cost function for running allocations and issues
//! checkpoint hints when the value exceeds the cost.

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::CheckpointBroker;
use lattice_common::types::*;

use crate::cost_model::{evaluate_checkpoint, CheckpointParams};
use crate::policy::{can_checkpoint_now, evaluate_policy, is_checkpoint_due};
use crate::protocol::{checkpoint_destination, resolve_protocol, CheckpointRequest};

/// The checkpoint broker evaluates running allocations and decides
/// whether to initiate checkpoints based on cost/value analysis.
pub struct LatticeCheckpointBroker {
    /// Default checkpoint parameters
    params: CheckpointParams,
    /// Whether to use NFS instead of S3 for checkpoints
    use_nfs: bool,
    /// Checkpoint counter for generating IDs
    next_checkpoint_id: std::sync::atomic::AtomicU64,
}

impl LatticeCheckpointBroker {
    pub fn new(params: CheckpointParams) -> Self {
        Self {
            params,
            use_nfs: false,
            next_checkpoint_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn with_nfs(mut self) -> Self {
        self.use_nfs = true;
        self
    }

    /// Evaluate a batch of running allocations and return checkpoint requests.
    pub fn evaluate_batch(&self, allocations: &[Allocation]) -> Vec<CheckpointRequest> {
        allocations
            .iter()
            .filter(|a| a.state == AllocationState::Running)
            .filter_map(|alloc| self.evaluate_single(alloc))
            .collect()
    }

    /// Evaluate a single allocation for checkpoint.
    fn evaluate_single(&self, alloc: &Allocation) -> Option<CheckpointRequest> {
        // Skip non-checkpointable
        let protocol = resolve_protocol(alloc)?;
        let policy = evaluate_policy(alloc);

        // Check policy: minimum interval respected?
        // (Using started_at as proxy for last checkpoint time — real impl would track this)
        if !can_checkpoint_now(alloc.started_at, &policy, self.params.now) {
            return None;
        }

        // Check if checkpoint is overdue (max interval)
        let overdue = is_checkpoint_due(alloc.started_at, &policy, self.params.now);

        // Evaluate cost model
        let eval = evaluate_checkpoint(alloc, &self.params);

        if eval.should_checkpoint || overdue {
            let ckpt_id = format!(
                "ckpt-{}",
                self.next_checkpoint_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let destination = checkpoint_destination(alloc, &ckpt_id, self.use_nfs);

            Some(CheckpointRequest {
                allocation_id: alloc.id,
                node_id: alloc.assigned_nodes.first().cloned().unwrap_or_default(),
                protocol,
                timeout_seconds: policy.timeout_secs,
                checkpoint_id: ckpt_id,
                destination,
            })
        } else {
            None
        }
    }
}

#[async_trait]
impl CheckpointBroker for LatticeCheckpointBroker {
    async fn should_checkpoint(&self, allocation: &Allocation) -> Result<bool, LatticeError> {
        let protocol = resolve_protocol(allocation);
        if protocol.is_none() {
            return Ok(false);
        }

        let eval = evaluate_checkpoint(allocation, &self.params);
        Ok(eval.should_checkpoint)
    }

    async fn initiate_checkpoint(&self, _id: &AllocId) -> Result<(), LatticeError> {
        // In a real implementation, this would send a checkpoint request
        // to the node agent via gRPC. For now, we just acknowledge.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    fn test_params() -> CheckpointParams {
        CheckpointParams {
            backlog_pressure: 0.0,
            waiting_higher_priority_jobs: 0,
            failure_probability: 0.001,
            ..Default::default()
        }
    }

    fn high_pressure_params() -> CheckpointParams {
        CheckpointParams {
            backlog_pressure: 0.9,
            waiting_higher_priority_jobs: 5,
            failure_probability: 0.1,
            ..Default::default()
        }
    }

    fn running_alloc(nodes: usize) -> Allocation {
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = (0..nodes).map(|i| format!("n{i}")).collect();
        alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(2));
        alloc
    }

    #[test]
    fn batch_skips_pending_allocations() {
        let broker = LatticeCheckpointBroker::new(high_pressure_params());
        let pending = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        let requests = broker.evaluate_batch(&[pending]);
        assert!(requests.is_empty());
    }

    #[test]
    fn batch_evaluates_running_allocations() {
        let broker = LatticeCheckpointBroker::new(high_pressure_params());
        let alloc = running_alloc(4);
        let requests = broker.evaluate_batch(&[alloc]);
        assert!(!requests.is_empty());
    }

    #[test]
    fn batch_skips_non_checkpointable() {
        let broker = LatticeCheckpointBroker::new(high_pressure_params());
        let mut alloc = running_alloc(4);
        alloc.checkpoint = CheckpointStrategy::None;
        let requests = broker.evaluate_batch(&[alloc]);
        assert!(requests.is_empty());
    }

    #[test]
    fn nfs_destination_when_configured() {
        let broker = LatticeCheckpointBroker::new(high_pressure_params()).with_nfs();
        let alloc = running_alloc(4);
        let requests = broker.evaluate_batch(&[alloc]);
        if let Some(req) = requests.first() {
            assert!(matches!(
                req.destination,
                crate::protocol::CheckpointDestination::Nfs { .. }
            ));
        }
    }

    #[tokio::test]
    async fn should_checkpoint_trait_impl() {
        let broker = LatticeCheckpointBroker::new(high_pressure_params());
        let alloc = running_alloc(4);
        let result = broker.should_checkpoint(&alloc).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn should_not_checkpoint_none_strategy() {
        let broker = LatticeCheckpointBroker::new(high_pressure_params());
        let mut alloc = running_alloc(4);
        alloc.checkpoint = CheckpointStrategy::None;
        let result = broker.should_checkpoint(&alloc).await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn initiate_checkpoint_succeeds() {
        let broker = LatticeCheckpointBroker::new(test_params());
        let id = uuid::Uuid::new_v4();
        let result = broker.initiate_checkpoint(&id).await;
        assert!(result.is_ok());
    }
}

//! CheckpointBroker implementation.
//!
//! Evaluates the cost function for running allocations and issues
//! checkpoint hints when the value exceeds the cost.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use lattice_common::error::LatticeError;
use lattice_common::traits::{AllocationStore, CheckpointBroker};
use lattice_common::types::*;

use crate::cost_model::{evaluate_checkpoint, CheckpointParams};
use crate::policy::{can_checkpoint_now, evaluate_policy, is_checkpoint_due};
use crate::protocol::{
    checkpoint_destination, resolve_protocol, CheckpointProtocol, CheckpointRequest,
};

/// Trait for delivering checkpoint signals to node agents.
///
/// Implementations send the actual checkpoint request to the node agent
/// running on the target node (e.g., via gRPC or signal delivery).
#[async_trait]
pub trait NodeAgentPool: Send + Sync {
    /// Send a checkpoint request to a specific node for an allocation.
    async fn send_checkpoint(
        &self,
        node_id: &str,
        alloc_id: &AllocId,
        protocol: &CheckpointProtocol,
    ) -> Result<(), LatticeError>;
}

/// The checkpoint broker evaluates running allocations and decides
/// whether to initiate checkpoints based on cost/value analysis.
pub struct LatticeCheckpointBroker {
    /// Default checkpoint parameters
    params: CheckpointParams,
    /// Whether to use NFS instead of S3 for checkpoints
    use_nfs: bool,
    /// Checkpoint counter for generating IDs
    next_checkpoint_id: std::sync::atomic::AtomicU64,
    /// Optional pool of node agents for delivering checkpoint signals.
    agent_pool: Option<Arc<dyn NodeAgentPool>>,
    /// Optional allocation store for looking up allocations by ID.
    allocation_store: Option<Arc<dyn AllocationStore>>,
}

impl LatticeCheckpointBroker {
    pub fn new(params: CheckpointParams) -> Self {
        Self {
            params,
            use_nfs: false,
            next_checkpoint_id: std::sync::atomic::AtomicU64::new(1),
            agent_pool: None,
            allocation_store: None,
        }
    }

    pub fn with_nfs(mut self) -> Self {
        self.use_nfs = true;
        self
    }

    /// Attach a node agent pool for real checkpoint delivery.
    pub fn with_agent_pool(mut self, pool: Arc<dyn NodeAgentPool>) -> Self {
        self.agent_pool = Some(pool);
        self
    }

    /// Attach an allocation store for looking up allocations by ID.
    pub fn with_allocation_store(mut self, store: Arc<dyn AllocationStore>) -> Self {
        self.allocation_store = Some(store);
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

    async fn initiate_checkpoint(&self, id: &AllocId) -> Result<(), LatticeError> {
        // If no agent pool is configured, just acknowledge (stub behavior).
        let pool = match &self.agent_pool {
            Some(pool) => pool,
            None => return Ok(()),
        };

        // Look up the allocation to get assigned nodes and protocol.
        let store = match &self.allocation_store {
            Some(store) => store,
            None => return Ok(()),
        };

        let alloc = store.get(id).await?;
        let protocol = match resolve_protocol(&alloc) {
            Some(p) => p,
            None => {
                warn!(alloc_id = %id, "No checkpoint protocol for allocation");
                return Ok(());
            }
        };

        // Fan out to all assigned nodes; collect results.
        let mut successes = 0usize;
        let mut failures = 0usize;
        let total = alloc.assigned_nodes.len();

        for node_id in &alloc.assigned_nodes {
            match pool.send_checkpoint(node_id, id, &protocol).await {
                Ok(()) => {
                    successes += 1;
                }
                Err(e) => {
                    warn!(
                        alloc_id = %id,
                        node_id = %node_id,
                        error = %e,
                        "Checkpoint delivery failed on node"
                    );
                    failures += 1;
                }
            }
        }

        if successes > 0 {
            info!(
                alloc_id = %id,
                successes,
                failures,
                total,
                "Checkpoint initiated"
            );
            Ok(())
        } else if total > 0 {
            Err(LatticeError::Internal(format!(
                "checkpoint delivery failed on all {total} nodes"
            )))
        } else {
            // No assigned nodes — nothing to do.
            Ok(())
        }
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
    async fn initiate_checkpoint_succeeds_without_pool() {
        let broker = LatticeCheckpointBroker::new(test_params());
        let id = uuid::Uuid::new_v4();
        let result = broker.initiate_checkpoint(&id).await;
        assert!(result.is_ok());
    }

    // ─── NodeAgentPool integration tests ─────────────────────────

    struct MockNodeAgentPool {
        calls: tokio::sync::Mutex<Vec<(String, AllocId)>>,
        fail_nodes: Vec<String>,
    }

    impl MockNodeAgentPool {
        fn new() -> Self {
            Self {
                calls: tokio::sync::Mutex::new(Vec::new()),
                fail_nodes: vec![],
            }
        }

        fn with_failing_nodes(nodes: Vec<String>) -> Self {
            Self {
                calls: tokio::sync::Mutex::new(Vec::new()),
                fail_nodes: nodes,
            }
        }
    }

    #[async_trait]
    impl NodeAgentPool for MockNodeAgentPool {
        async fn send_checkpoint(
            &self,
            node_id: &str,
            alloc_id: &AllocId,
            _protocol: &CheckpointProtocol,
        ) -> Result<(), LatticeError> {
            self.calls
                .lock()
                .await
                .push((node_id.to_string(), *alloc_id));
            if self.fail_nodes.contains(&node_id.to_string()) {
                Err(LatticeError::Internal(format!(
                    "node {node_id} unreachable"
                )))
            } else {
                Ok(())
            }
        }
    }

    struct MockAllocationStore {
        alloc: Allocation,
    }

    #[async_trait]
    impl lattice_common::traits::AllocationStore for MockAllocationStore {
        async fn insert(&self, _alloc: Allocation) -> Result<(), LatticeError> {
            Ok(())
        }
        async fn get(&self, _id: &AllocId) -> Result<Allocation, LatticeError> {
            Ok(self.alloc.clone())
        }
        async fn list(
            &self,
            _filter: &lattice_common::traits::AllocationFilter,
        ) -> Result<Vec<Allocation>, LatticeError> {
            Ok(vec![self.alloc.clone()])
        }
        async fn update_state(
            &self,
            _id: &AllocId,
            _state: AllocationState,
        ) -> Result<(), LatticeError> {
            Ok(())
        }
        async fn count_running(
            &self,
            _tenant: &lattice_common::types::TenantId,
        ) -> Result<u32, LatticeError> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn initiate_checkpoint_fans_out_to_all_nodes() {
        let alloc = running_alloc(3);
        let alloc_id = alloc.id;
        let pool = Arc::new(MockNodeAgentPool::new());
        let store = Arc::new(MockAllocationStore {
            alloc: alloc.clone(),
        });

        let broker = LatticeCheckpointBroker::new(test_params())
            .with_agent_pool(pool.clone())
            .with_allocation_store(store);

        broker.initiate_checkpoint(&alloc_id).await.unwrap();

        let calls = pool.calls.lock().await;
        assert_eq!(calls.len(), 3);
        for call in calls.iter() {
            assert_eq!(call.1, alloc_id);
        }
    }

    #[tokio::test]
    async fn initiate_checkpoint_partial_failure_succeeds() {
        let alloc = running_alloc(3);
        let alloc_id = alloc.id;
        // Fail on the first node, succeed on the others.
        let fail_node = alloc.assigned_nodes[0].clone();
        let pool = Arc::new(MockNodeAgentPool::with_failing_nodes(vec![fail_node]));
        let store = Arc::new(MockAllocationStore {
            alloc: alloc.clone(),
        });

        let broker = LatticeCheckpointBroker::new(test_params())
            .with_agent_pool(pool.clone())
            .with_allocation_store(store);

        // Should succeed because at least some nodes succeeded.
        let result = broker.initiate_checkpoint(&alloc_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn initiate_checkpoint_all_fail_returns_error() {
        let alloc = running_alloc(2);
        let alloc_id = alloc.id;
        let all_nodes = alloc.assigned_nodes.clone();
        let pool = Arc::new(MockNodeAgentPool::with_failing_nodes(all_nodes));
        let store = Arc::new(MockAllocationStore {
            alloc: alloc.clone(),
        });

        let broker = LatticeCheckpointBroker::new(test_params())
            .with_agent_pool(pool)
            .with_allocation_store(store);

        let result = broker.initiate_checkpoint(&alloc_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn initiate_checkpoint_non_checkpointable_is_noop() {
        let mut alloc = running_alloc(2);
        alloc.checkpoint = CheckpointStrategy::None;
        let alloc_id = alloc.id;
        let pool = Arc::new(MockNodeAgentPool::new());
        let store = Arc::new(MockAllocationStore {
            alloc: alloc.clone(),
        });

        let broker = LatticeCheckpointBroker::new(test_params())
            .with_agent_pool(pool.clone())
            .with_allocation_store(store);

        broker.initiate_checkpoint(&alloc_id).await.unwrap();

        // Should not have sent any checkpoint signals.
        let calls = pool.calls.lock().await;
        assert!(calls.is_empty());
    }
}

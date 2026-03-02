//! Periodic checkpoint evaluation loop.
//!
//! `CheckpointLoop` runs on a configurable interval (default 30s),
//! reads running allocations from an `AllocationStore`, evaluates
//! each against the cost model, and reports which allocations should
//! be checkpointed.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{debug, info, warn};

use lattice_common::traits::AllocationFilter;
use lattice_common::traits::AllocationStore;
use lattice_common::types::{AllocId, AllocationState};

use crate::broker::LatticeCheckpointBroker;

/// Default evaluation interval (30 seconds).
const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// Periodic checkpoint evaluation loop.
///
/// Reads running allocations from a store, evaluates each using the
/// broker's cost model, and returns those that should be checkpointed.
pub struct CheckpointLoop {
    /// Interval between evaluation cycles.
    interval: Duration,
    /// The checkpoint broker that evaluates cost/value.
    broker: Arc<LatticeCheckpointBroker>,
    /// Allocation store to query for running allocations.
    store: Arc<dyn AllocationStore>,
}

impl CheckpointLoop {
    /// Create a new `CheckpointLoop` with the default 30-second interval.
    pub fn new(broker: Arc<LatticeCheckpointBroker>, store: Arc<dyn AllocationStore>) -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            broker,
            store,
        }
    }

    /// Override the evaluation interval.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Return the configured interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Run a single evaluation cycle.
    ///
    /// Fetches all running allocations from the store, evaluates each
    /// using the broker's `evaluate_batch`, and returns the allocation
    /// IDs that should be checkpointed.
    pub async fn run_once(&self) -> Vec<AllocId> {
        let filter = AllocationFilter {
            state: Some(AllocationState::Running),
            ..Default::default()
        };

        let allocations = match self.store.list(&filter).await {
            Ok(allocs) => allocs,
            Err(e) => {
                warn!("Failed to list running allocations: {e}");
                return Vec::new();
            }
        };

        if allocations.is_empty() {
            debug!("No running allocations to evaluate for checkpoint");
            return Vec::new();
        }

        let requests = self.broker.evaluate_batch(&allocations);

        let ids: Vec<AllocId> = requests.iter().map(|r| r.allocation_id).collect();

        if !ids.is_empty() {
            info!(
                count = ids.len(),
                "Checkpoint evaluation identified allocations to checkpoint"
            );
        } else {
            debug!(
                evaluated = allocations.len(),
                "No allocations need checkpointing this cycle"
            );
        }

        ids
    }

    /// Run the checkpoint evaluation loop until cancellation.
    ///
    /// The loop evaluates on each tick of `self.interval`. It exits
    /// when the `cancel` receiver receives `true`.
    pub async fn run(&self, mut cancel: watch::Receiver<bool>) {
        info!(
            interval_secs = self.interval.as_secs(),
            "Starting checkpoint evaluation loop"
        );

        let mut ticker = tokio::time::interval(self.interval);
        // The first tick fires immediately; consume it so the loop
        // starts after one full interval has elapsed.
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let ids = self.run_once().await;
                    if !ids.is_empty() {
                        debug!(count = ids.len(), "Checkpoint cycle completed with candidates");
                    }
                }
                result = cancel.changed() => {
                    match result {
                        Ok(()) if *cancel.borrow() => {
                            info!("Checkpoint loop received cancellation signal");
                            return;
                        }
                        Ok(()) => {
                            // Value changed but not to true; keep running.
                        }
                        Err(_) => {
                            info!("Checkpoint loop cancel channel closed, shutting down");
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use lattice_test_harness::fixtures::AllocationBuilder;
    use lattice_test_harness::mocks::MockAllocationStore;

    use crate::cost_model::CheckpointParams;

    /// Build a running allocation with assigned nodes and a start time.
    fn running_alloc(nodes: usize) -> lattice_common::types::Allocation {
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = (0..nodes).map(|i| format!("n{i}")).collect();
        alloc.started_at = Some(Utc::now() - chrono::Duration::hours(2));
        alloc
    }

    fn high_pressure_params() -> CheckpointParams {
        CheckpointParams {
            backlog_pressure: 0.9,
            waiting_higher_priority_jobs: 5,
            failure_probability: 0.1,
            ..Default::default()
        }
    }

    fn low_pressure_params() -> CheckpointParams {
        CheckpointParams {
            backlog_pressure: 0.0,
            waiting_higher_priority_jobs: 0,
            failure_probability: 0.001,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn high_backlog_triggers_checkpoint() {
        let alloc = running_alloc(4);
        let expected_id = alloc.id;

        let store = Arc::new(MockAllocationStore::new().with_allocations(vec![alloc]));
        let broker = Arc::new(LatticeCheckpointBroker::new(high_pressure_params()));

        let loop_runner = CheckpointLoop::new(broker, store);
        let ids = loop_runner.run_once().await;

        assert!(!ids.is_empty(), "High backlog should trigger a checkpoint");
        assert!(
            ids.contains(&expected_id),
            "The running allocation should be in the checkpoint list"
        );
    }

    #[tokio::test]
    async fn low_backlog_does_not_trigger_checkpoint() {
        let alloc = running_alloc(2);

        let store = Arc::new(MockAllocationStore::new().with_allocations(vec![alloc]));
        let broker = Arc::new(LatticeCheckpointBroker::new(low_pressure_params()));

        let loop_runner = CheckpointLoop::new(broker, store);
        let ids = loop_runner.run_once().await;

        assert!(
            ids.is_empty(),
            "Low backlog should not trigger any checkpoints"
        );
    }

    #[tokio::test]
    async fn empty_store_returns_no_candidates() {
        let store = Arc::new(MockAllocationStore::new());
        let broker = Arc::new(LatticeCheckpointBroker::new(high_pressure_params()));

        let loop_runner = CheckpointLoop::new(broker, store);
        let ids = loop_runner.run_once().await;

        assert!(ids.is_empty(), "Empty store should return no candidates");
    }

    #[tokio::test]
    async fn skips_non_running_allocations() {
        let pending = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        let completed = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();

        let store = Arc::new(MockAllocationStore::new().with_allocations(vec![pending, completed]));
        let broker = Arc::new(LatticeCheckpointBroker::new(high_pressure_params()));

        let loop_runner = CheckpointLoop::new(broker, store);
        let ids = loop_runner.run_once().await;

        assert!(ids.is_empty(), "Non-running allocations should be skipped");
    }

    #[tokio::test]
    async fn skips_non_checkpointable_allocations() {
        let mut alloc = running_alloc(4);
        alloc.checkpoint = lattice_common::types::CheckpointStrategy::None;

        let store = Arc::new(MockAllocationStore::new().with_allocations(vec![alloc]));
        let broker = Arc::new(LatticeCheckpointBroker::new(high_pressure_params()));

        let loop_runner = CheckpointLoop::new(broker, store);
        let ids = loop_runner.run_once().await;

        assert!(
            ids.is_empty(),
            "Allocations with CheckpointStrategy::None should be skipped"
        );
    }

    #[tokio::test]
    async fn evaluates_multiple_allocations() {
        let alloc1 = running_alloc(4);
        let alloc2 = running_alloc(4);
        let id1 = alloc1.id;
        let id2 = alloc2.id;

        let store = Arc::new(MockAllocationStore::new().with_allocations(vec![alloc1, alloc2]));
        let broker = Arc::new(LatticeCheckpointBroker::new(high_pressure_params()));

        let loop_runner = CheckpointLoop::new(broker, store);
        let ids = loop_runner.run_once().await;

        assert_eq!(ids.len(), 2, "Both running allocations should be flagged");
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[tokio::test]
    async fn default_interval_is_30_seconds() {
        let store = Arc::new(MockAllocationStore::new());
        let broker = Arc::new(LatticeCheckpointBroker::new(CheckpointParams::default()));

        let loop_runner = CheckpointLoop::new(broker, store);
        assert_eq!(loop_runner.interval(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn custom_interval_is_respected() {
        let store = Arc::new(MockAllocationStore::new());
        let broker = Arc::new(LatticeCheckpointBroker::new(CheckpointParams::default()));

        let loop_runner = CheckpointLoop::new(broker, store).with_interval(Duration::from_secs(60));
        assert_eq!(loop_runner.interval(), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn loop_exits_on_cancel() {
        let store = Arc::new(MockAllocationStore::new());
        let broker = Arc::new(LatticeCheckpointBroker::new(CheckpointParams::default()));

        let loop_runner =
            CheckpointLoop::new(broker, store).with_interval(Duration::from_millis(50));

        let (cancel_tx, cancel_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            loop_runner.run(cancel_rx).await;
        });

        // Let it run for a bit, then cancel.
        tokio::time::sleep(Duration::from_millis(120)).await;
        cancel_tx.send(true).expect("cancel send should succeed");

        // The loop should finish promptly.
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "Loop should exit within timeout after cancellation"
        );
    }

    #[tokio::test]
    async fn loop_exits_on_channel_close() {
        let store = Arc::new(MockAllocationStore::new());
        let broker = Arc::new(LatticeCheckpointBroker::new(CheckpointParams::default()));

        let loop_runner =
            CheckpointLoop::new(broker, store).with_interval(Duration::from_millis(50));

        let (cancel_tx, cancel_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            loop_runner.run(cancel_rx).await;
        });

        // Drop sender to close the channel.
        drop(cancel_tx);

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "Loop should exit when cancel channel is closed"
        );
    }
}

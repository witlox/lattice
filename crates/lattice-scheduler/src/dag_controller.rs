//! DAG controller — watches completed allocations and triggers ready successors.
//!
//! The controller periodically checks for terminal allocations, evaluates
//! which downstream DAG stages are now unblocked, and transitions them
//! from `Pending` to schedulable state.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use lattice_common::types::{AllocId, Allocation, AllocationState};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::dag::resolve_dependencies;

/// Reads DAG-related state.
#[async_trait::async_trait]
pub trait DagStateReader: Send + Sync {
    /// Return all allocations that are part of any DAG (have dependencies or are depended upon).
    async fn dag_allocations(&self)
        -> Result<Vec<Allocation>, lattice_common::error::LatticeError>;
}

/// Applies DAG decisions.
#[async_trait::async_trait]
pub trait DagCommandSink: Send + Sync {
    /// Mark an allocation as ready to be scheduled (unblock it).
    /// Typically transitions from a DAG-blocked state to schedulable Pending.
    async fn unblock_allocation(
        &self,
        alloc_id: AllocId,
    ) -> Result<(), lattice_common::error::LatticeError>;

    /// Cancel downstream allocations when an upstream dependency fails
    /// and the dependency condition cannot be satisfied.
    async fn cancel_allocation(
        &self,
        alloc_id: AllocId,
        reason: String,
    ) -> Result<(), lattice_common::error::LatticeError>;
}

/// Configuration for the DAG controller.
#[derive(Debug, Clone)]
pub struct DagControllerConfig {
    /// Interval between evaluation cycles.
    pub tick_interval: Duration,
}

impl Default for DagControllerConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(2),
        }
    }
}

/// The DAG controller: watches completions and triggers ready successors.
pub struct DagController<R: DagStateReader, S: DagCommandSink> {
    reader: Arc<R>,
    sink: Arc<S>,
    config: DagControllerConfig,
    /// Track which allocations we've already processed to avoid re-triggering.
    processed: HashSet<AllocId>,
}

impl<R: DagStateReader, S: DagCommandSink> DagController<R, S> {
    pub fn new(reader: Arc<R>, sink: Arc<S>, config: DagControllerConfig) -> Self {
        Self {
            reader,
            sink,
            config,
            processed: HashSet::new(),
        }
    }

    /// Run a single evaluation cycle.
    /// Returns the number of allocations unblocked.
    pub async fn run_once(&mut self) -> Result<usize, lattice_common::error::LatticeError> {
        let allocations = self.reader.dag_allocations().await?;
        if allocations.is_empty() {
            return Ok(0);
        }

        // Collect terminal states for all allocations
        let terminal_states: HashMap<AllocId, AllocationState> = allocations
            .iter()
            .filter(|a| a.state.is_terminal())
            .map(|a| (a.id, a.state.clone()))
            .collect();

        // Find newly unblocked allocations
        let unblocked = resolve_dependencies(&allocations, &terminal_states);

        let mut count = 0;
        for alloc_id in unblocked {
            if self.processed.contains(&alloc_id) {
                continue;
            }

            debug!(alloc_id = %alloc_id, "Unblocking DAG successor");
            match self.sink.unblock_allocation(alloc_id).await {
                Ok(()) => {
                    self.processed.insert(alloc_id);
                    count += 1;
                }
                Err(e) => {
                    error!(alloc_id = %alloc_id, error = %e, "Failed to unblock allocation");
                }
            }
        }

        // Check for allocations that can never be satisfied and should be cancelled
        let cancelled = self.find_unsatisfiable(&allocations, &terminal_states);
        for alloc_id in cancelled {
            if self.processed.contains(&alloc_id) {
                continue;
            }

            warn!(alloc_id = %alloc_id, "Cancelling unsatisfiable DAG successor");
            match self
                .sink
                .cancel_allocation(alloc_id, "upstream dependency cannot be satisfied".into())
                .await
            {
                Ok(()) => {
                    self.processed.insert(alloc_id);
                }
                Err(e) => {
                    error!(alloc_id = %alloc_id, error = %e, "Failed to cancel allocation");
                }
            }
        }

        if count > 0 {
            info!(unblocked = count, "DAG controller cycle completed");
        }

        Ok(count)
    }

    /// Find allocations whose dependencies can never be satisfied.
    ///
    /// For example, if a `Success` dependency's upstream failed, the downstream
    /// can never proceed and should be cancelled.
    fn find_unsatisfiable(
        &self,
        allocations: &[Allocation],
        terminal_states: &HashMap<AllocId, AllocationState>,
    ) -> Vec<AllocId> {
        let id_lookup: HashMap<String, AllocId> = allocations
            .iter()
            .map(|a| (a.id.to_string(), a.id))
            .collect();

        allocations
            .iter()
            .filter(|alloc| {
                alloc.state == AllocationState::Pending
                    && !alloc.depends_on.is_empty()
                    && !self.processed.contains(&alloc.id)
                    && alloc.depends_on.iter().any(|dep| {
                        id_lookup
                            .get(&dep.ref_id)
                            .and_then(|dep_id| terminal_states.get(dep_id))
                            .is_some_and(|state| {
                                !crate::dag::is_condition_satisfied(&dep.condition, state)
                                    && state.is_terminal()
                            })
                    })
            })
            .map(|a| a.id)
            .collect()
    }

    /// Run the controller loop until cancelled.
    pub async fn run(&mut self, mut cancel: watch::Receiver<bool>) {
        info!(
            interval_ms = self.config.tick_interval.as_millis(),
            "DAG controller starting"
        );
        let mut interval = tokio::time::interval(self.config.tick_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.run_once().await {
                        Ok(n) => {
                            if n > 0 {
                                debug!(unblocked = n, "DAG controller unblocked allocations");
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "DAG controller cycle failed");
                        }
                    }
                }
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        info!("DAG controller shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Get the set of processed allocation IDs.
    pub fn processed(&self) -> &HashSet<AllocId> {
        &self.processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::error::LatticeError;
    use lattice_common::types::*;
    use lattice_test_harness::fixtures::AllocationBuilder;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    struct MockDagReader {
        allocations: Mutex<Vec<Allocation>>,
    }

    impl MockDagReader {
        fn new(allocs: Vec<Allocation>) -> Self {
            Self {
                allocations: Mutex::new(allocs),
            }
        }
    }

    #[async_trait::async_trait]
    impl DagStateReader for MockDagReader {
        async fn dag_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
            Ok(self.allocations.lock().await.clone())
        }
    }

    struct MockDagSink {
        unblocked: Mutex<Vec<AllocId>>,
        cancelled: Mutex<Vec<(AllocId, String)>>,
        unblock_count: AtomicUsize,
    }

    impl MockDagSink {
        fn new() -> Self {
            Self {
                unblocked: Mutex::new(Vec::new()),
                cancelled: Mutex::new(Vec::new()),
                unblock_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl DagCommandSink for MockDagSink {
        async fn unblock_allocation(&self, alloc_id: AllocId) -> Result<(), LatticeError> {
            self.unblocked.lock().await.push(alloc_id);
            self.unblock_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn cancel_allocation(
            &self,
            alloc_id: AllocId,
            reason: String,
        ) -> Result<(), LatticeError> {
            self.cancelled.lock().await.push((alloc_id, reason));
            Ok(())
        }
    }

    #[tokio::test]
    async fn no_dag_allocations_returns_zero() {
        let reader = Arc::new(MockDagReader::new(vec![]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink, DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn afterok_unblocks_on_success() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();
        let b_id = b.id;

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 1);

        let unblocked = sink.unblocked.lock().await;
        assert_eq!(unblocked.len(), 1);
        assert_eq!(unblocked[0], b_id);
    }

    #[tokio::test]
    async fn afterok_does_not_unblock_on_failure() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 0);
        assert!(sink.unblocked.lock().await.is_empty());
    }

    #[tokio::test]
    async fn afternotok_unblocks_on_failure() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Failure)
            .build();
        let b_id = b.id;

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(sink.unblocked.lock().await[0], b_id);
    }

    #[tokio::test]
    async fn afterany_unblocks_on_any_terminal() {
        for state in [
            AllocationState::Completed,
            AllocationState::Failed,
            AllocationState::Cancelled,
        ] {
            let a = AllocationBuilder::new().state(state).build();
            let b = AllocationBuilder::new()
                .depends_on(&a.id.to_string(), DependencyCondition::Any)
                .build();
            let b_id = b.id;

            let reader = Arc::new(MockDagReader::new(vec![a, b]));
            let sink = Arc::new(MockDagSink::new());
            let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

            let count = ctrl.run_once().await.unwrap();
            assert_eq!(count, 1);
            assert_eq!(sink.unblocked.lock().await[0], b_id);
        }
    }

    #[tokio::test]
    async fn three_stage_dag_executes_in_order() {
        // Stage 1: completed
        let s1 = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        // Stage 2: depends on s1 (success), still pending
        let s2 = AllocationBuilder::new()
            .depends_on(&s1.id.to_string(), DependencyCondition::Success)
            .build();
        // Stage 3: depends on s2 (success), still pending
        let s3 = AllocationBuilder::new()
            .depends_on(&s2.id.to_string(), DependencyCondition::Success)
            .build();
        let s2_id = s2.id;

        let reader = Arc::new(MockDagReader::new(vec![s1, s2, s3]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        // First cycle: only s2 should unblock (s3 still blocked by s2)
        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(sink.unblocked.lock().await[0], s2_id);
    }

    #[tokio::test]
    async fn processed_allocations_not_re_triggered() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        // First cycle
        ctrl.run_once().await.unwrap();
        assert_eq!(sink.unblocked.lock().await.len(), 1);

        // Second cycle: should not re-trigger
        ctrl.run_once().await.unwrap();
        assert_eq!(sink.unblocked.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn unsatisfiable_afterok_cancelled_on_failure() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();
        let b_id = b.id;

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        ctrl.run_once().await.unwrap();

        let cancelled = sink.cancelled.lock().await;
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].0, b_id);
        assert!(cancelled[0].1.contains("cannot be satisfied"));
    }

    #[tokio::test]
    async fn multiple_dependencies_all_must_be_satisfied() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let c = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .depends_on(&b.id.to_string(), DependencyCondition::Success)
            .build();
        let c_id = c.id;

        let reader = Arc::new(MockDagReader::new(vec![a, b, c]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(sink.unblocked.lock().await[0], c_id);
    }

    #[tokio::test]
    async fn partial_dependencies_blocks_scheduling() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let c = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .depends_on(&b.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a, b, c]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn controller_loop_processes_and_stops() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());

        let config = DagControllerConfig {
            tick_interval: Duration::from_millis(50),
        };
        let mut ctrl = DagController::new(reader, sink.clone(), config);

        let (cancel_tx, cancel_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            ctrl.run(cancel_rx).await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_tx.send(true).unwrap();
        handle.await.unwrap();

        assert!(sink.unblock_count.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn reader_error_propagates() {
        struct FailingReader;

        #[async_trait::async_trait]
        impl DagStateReader for FailingReader {
            async fn dag_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
                Err(LatticeError::Internal("connection lost".into()))
            }
        }

        let reader = Arc::new(FailingReader);
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink, DagControllerConfig::default());

        let result = ctrl.run_once().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sink_error_skips_allocation() {
        struct FailingDagSink;

        #[async_trait::async_trait]
        impl DagCommandSink for FailingDagSink {
            async fn unblock_allocation(&self, _: AllocId) -> Result<(), LatticeError> {
                Err(LatticeError::Internal("raft unavailable".into()))
            }
            async fn cancel_allocation(&self, _: AllocId, _: String) -> Result<(), LatticeError> {
                Ok(())
            }
        }

        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(FailingDagSink);
        let mut ctrl = DagController::new(reader, sink, DagControllerConfig::default());

        // Should not crash
        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 0);
    }
}

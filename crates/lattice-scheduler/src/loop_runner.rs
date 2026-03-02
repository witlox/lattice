//! Scheduler loop — periodic scheduling cycle that reads quorum state,
//! runs the solver, and proposes placements.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lattice_common::traits::{AllocationFilter, AllocationStore, NodeRegistry};
use lattice_common::types::{AllocationState, CostWeights, Node, TopologyModel};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::cycle::{run_cycle, CycleInput};
use crate::placement::PlacementDecision;

/// Reads cluster state for the scheduler.
#[async_trait::async_trait]
pub trait SchedulerStateReader: Send + Sync {
    /// Read all pending allocations.
    async fn pending_allocations(
        &self,
    ) -> Result<Vec<lattice_common::types::Allocation>, lattice_common::error::LatticeError>;
    /// Read all running allocations.
    async fn running_allocations(
        &self,
    ) -> Result<Vec<lattice_common::types::Allocation>, lattice_common::error::LatticeError>;
    /// Read all available nodes.
    async fn available_nodes(
        &self,
    ) -> Result<Vec<lattice_common::types::Node>, lattice_common::error::LatticeError>;
    /// Read tenants.
    async fn tenants(
        &self,
    ) -> Result<Vec<lattice_common::types::Tenant>, lattice_common::error::LatticeError>;
    /// Read topology.
    async fn topology(&self) -> TopologyModel;
}

/// Applies scheduling decisions to the cluster.
#[async_trait::async_trait]
pub trait SchedulerCommandSink: Send + Sync {
    /// Assign nodes to an allocation.
    async fn assign_nodes(
        &self,
        alloc_id: lattice_common::types::AllocId,
        nodes: Vec<String>,
    ) -> Result<(), lattice_common::error::LatticeError>;
    /// Transition allocation to Running.
    async fn set_running(
        &self,
        alloc_id: lattice_common::types::AllocId,
    ) -> Result<(), lattice_common::error::LatticeError>;
}

/// Configuration for the scheduler loop.
#[derive(Debug, Clone)]
pub struct SchedulerLoopConfig {
    /// Interval between scheduling cycles.
    pub tick_interval: Duration,
    /// Cost weights for the scoring function.
    pub weights: CostWeights,
    /// Normalized energy price (0.0-1.0).
    pub energy_price: f64,
}

impl Default for SchedulerLoopConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(5),
            weights: CostWeights::default(),
            energy_price: 0.5,
        }
    }
}

/// The scheduler loop: reads state, runs solver, proposes placements.
pub struct SchedulerLoop<R: SchedulerStateReader, S: SchedulerCommandSink> {
    reader: Arc<R>,
    sink: Arc<S>,
    config: SchedulerLoopConfig,
}

impl<R: SchedulerStateReader, S: SchedulerCommandSink> SchedulerLoop<R, S> {
    pub fn new(reader: Arc<R>, sink: Arc<S>, config: SchedulerLoopConfig) -> Self {
        Self {
            reader,
            sink,
            config,
        }
    }

    /// Run a single scheduling cycle. Returns the number of allocations placed.
    pub async fn run_once(&self) -> Result<usize, lattice_common::error::LatticeError> {
        let pending = self.reader.pending_allocations().await?;
        if pending.is_empty() {
            debug!("No pending allocations, skipping cycle");
            return Ok(0);
        }

        let running = self.reader.running_allocations().await?;
        let nodes = self.reader.available_nodes().await?;
        let tenants = self.reader.tenants().await?;
        let topology = self.reader.topology().await;

        let input = CycleInput {
            pending,
            running,
            nodes,
            tenants,
            topology,
            data_readiness: HashMap::new(),
            energy_price: self.config.energy_price,
        };

        let result = run_cycle(&input, &self.config.weights);

        let mut placed_count = 0;
        for decision in &result.decisions {
            match decision {
                PlacementDecision::Place {
                    allocation_id,
                    nodes,
                } => {
                    debug!(alloc_id = %allocation_id, nodes = nodes.len(), "Placing allocation");
                    if let Err(e) = self.sink.assign_nodes(*allocation_id, nodes.clone()).await {
                        error!(alloc_id = %allocation_id, error = %e, "Failed to assign nodes");
                        continue;
                    }
                    if let Err(e) = self.sink.set_running(*allocation_id).await {
                        error!(alloc_id = %allocation_id, error = %e, "Failed to set Running");
                        continue;
                    }
                    placed_count += 1;
                }
                PlacementDecision::Preempt {
                    allocation_id,
                    nodes,
                    victims,
                } => {
                    warn!(
                        alloc_id = %allocation_id,
                        nodes = nodes.len(),
                        victims = victims.len(),
                        "Preemption needed (not yet implemented)"
                    );
                }
                PlacementDecision::Defer {
                    allocation_id,
                    reason,
                } => {
                    debug!(alloc_id = %allocation_id, reason = %reason, "Deferred allocation");
                }
            }
        }

        if placed_count > 0 {
            info!(placed = placed_count, "Scheduling cycle completed");
        }

        Ok(placed_count)
    }

    /// Run the scheduler loop until the cancel signal fires.
    pub async fn run(&self, mut cancel: watch::Receiver<bool>) {
        info!(
            interval_ms = self.config.tick_interval.as_millis(),
            "Scheduler loop starting"
        );
        let mut interval = tokio::time::interval(self.config.tick_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.run_once().await {
                        Ok(n) => {
                            if n > 0 {
                                debug!(placed = n, "Cycle placed allocations");
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Scheduling cycle failed");
                        }
                    }
                }
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        info!("Scheduler loop shutting down");
                        break;
                    }
                }
            }
        }
    }
}

/// Adapter that reads from AllocationStore + NodeRegistry traits.
pub struct TraitStateReader {
    allocations: Arc<dyn AllocationStore>,
    nodes: Arc<dyn NodeRegistry>,
    tenants: Vec<lattice_common::types::Tenant>,
    topology: TopologyModel,
}

impl TraitStateReader {
    pub fn new(
        allocations: Arc<dyn AllocationStore>,
        nodes: Arc<dyn NodeRegistry>,
        tenants: Vec<lattice_common::types::Tenant>,
        topology: TopologyModel,
    ) -> Self {
        Self {
            allocations,
            nodes,
            tenants,
            topology,
        }
    }
}

#[async_trait::async_trait]
impl SchedulerStateReader for TraitStateReader {
    async fn pending_allocations(
        &self,
    ) -> Result<Vec<lattice_common::types::Allocation>, lattice_common::error::LatticeError> {
        let filter = AllocationFilter {
            state: Some(AllocationState::Pending),
            ..Default::default()
        };
        self.allocations.list(&filter).await
    }

    async fn running_allocations(
        &self,
    ) -> Result<Vec<lattice_common::types::Allocation>, lattice_common::error::LatticeError> {
        let filter = AllocationFilter {
            state: Some(AllocationState::Running),
            ..Default::default()
        };
        self.allocations.list(&filter).await
    }

    async fn available_nodes(&self) -> Result<Vec<Node>, lattice_common::error::LatticeError> {
        let filter = lattice_common::traits::NodeFilter {
            state: Some(lattice_common::types::NodeState::Ready),
            ..Default::default()
        };
        self.nodes.list_nodes(&filter).await
    }

    async fn tenants(
        &self,
    ) -> Result<Vec<lattice_common::types::Tenant>, lattice_common::error::LatticeError> {
        Ok(self.tenants.clone())
    }

    async fn topology(&self) -> TopologyModel {
        self.topology.clone()
    }
}

/// Adapter that writes back to AllocationStore.
pub struct TraitCommandSink {
    allocations: Arc<dyn AllocationStore>,
}

impl TraitCommandSink {
    pub fn new(allocations: Arc<dyn AllocationStore>) -> Self {
        Self { allocations }
    }
}

#[async_trait::async_trait]
impl SchedulerCommandSink for TraitCommandSink {
    async fn assign_nodes(
        &self,
        _alloc_id: lattice_common::types::AllocId,
        _nodes: Vec<String>,
    ) -> Result<(), lattice_common::error::LatticeError> {
        // In the real system, this would go through the quorum client.
        // The trait-based adapter doesn't support AssignNodes directly,
        // so we just update state to Running (which sets assigned_nodes too).
        Ok(())
    }

    async fn set_running(
        &self,
        alloc_id: lattice_common::types::AllocId,
    ) -> Result<(), lattice_common::error::LatticeError> {
        self.allocations
            .update_state(&alloc_id, AllocationState::Running)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::error::LatticeError;
    use lattice_common::types::*;
    use lattice_test_harness::fixtures::{
        create_node_batch, create_test_topology, AllocationBuilder, TenantBuilder,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    struct MockReader {
        pending: Vec<Allocation>,
        running: Vec<Allocation>,
        nodes: Vec<Node>,
        tenants: Vec<Tenant>,
        topology: TopologyModel,
    }

    #[async_trait::async_trait]
    impl SchedulerStateReader for MockReader {
        async fn pending_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
            Ok(self.pending.clone())
        }
        async fn running_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
            Ok(self.running.clone())
        }
        async fn available_nodes(&self) -> Result<Vec<Node>, LatticeError> {
            Ok(self.nodes.clone())
        }
        async fn tenants(&self) -> Result<Vec<Tenant>, LatticeError> {
            Ok(self.tenants.clone())
        }
        async fn topology(&self) -> TopologyModel {
            self.topology.clone()
        }
    }

    struct MockSink {
        assigned: Mutex<Vec<(AllocId, Vec<String>)>>,
        running: Mutex<Vec<AllocId>>,
        assign_count: AtomicUsize,
    }

    impl MockSink {
        fn new() -> Self {
            Self {
                assigned: Mutex::new(Vec::new()),
                running: Mutex::new(Vec::new()),
                assign_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl SchedulerCommandSink for MockSink {
        async fn assign_nodes(
            &self,
            alloc_id: AllocId,
            nodes: Vec<String>,
        ) -> Result<(), LatticeError> {
            self.assigned.lock().await.push((alloc_id, nodes));
            self.assign_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        async fn set_running(&self, alloc_id: AllocId) -> Result<(), LatticeError> {
            self.running.lock().await.push(alloc_id);
            Ok(())
        }
    }

    fn make_reader(pending: Vec<Allocation>, nodes: usize) -> MockReader {
        MockReader {
            pending,
            running: vec![],
            nodes: create_node_batch(nodes, 0),
            tenants: vec![TenantBuilder::new("t1").build()],
            topology: create_test_topology(1, nodes),
        }
    }

    #[tokio::test]
    async fn run_once_no_pending() {
        let reader = Arc::new(make_reader(vec![], 4));
        let sink = Arc::new(MockSink::new());
        let sched = SchedulerLoop::new(reader, sink.clone(), SchedulerLoopConfig::default());

        let placed = sched.run_once().await.unwrap();
        assert_eq!(placed, 0);
        assert!(sink.assigned.lock().await.is_empty());
    }

    #[tokio::test]
    async fn run_once_places_allocation() {
        let alloc = AllocationBuilder::new().tenant("t1").nodes(2).build();
        let alloc_id = alloc.id;

        let reader = Arc::new(make_reader(vec![alloc], 4));
        let sink = Arc::new(MockSink::new());
        let sched = SchedulerLoop::new(reader, sink.clone(), SchedulerLoopConfig::default());

        let placed = sched.run_once().await.unwrap();
        assert_eq!(placed, 1);

        let assigned = sink.assigned.lock().await;
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].0, alloc_id);
        assert_eq!(assigned[0].1.len(), 2);

        let running = sink.running.lock().await;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0], alloc_id);
    }

    #[tokio::test]
    async fn run_once_defers_when_insufficient_nodes() {
        let alloc = AllocationBuilder::new().tenant("t1").nodes(10).build();

        let reader = Arc::new(make_reader(vec![alloc], 4));
        let sink = Arc::new(MockSink::new());
        let sched = SchedulerLoop::new(reader, sink.clone(), SchedulerLoopConfig::default());

        let placed = sched.run_once().await.unwrap();
        assert_eq!(placed, 0);
        assert!(sink.assigned.lock().await.is_empty());
    }

    #[tokio::test]
    async fn run_once_places_multiple_allocations() {
        let a1 = AllocationBuilder::new().tenant("t1").nodes(1).build();
        let a2 = AllocationBuilder::new().tenant("t1").nodes(1).build();

        let reader = Arc::new(make_reader(vec![a1, a2], 4));
        let sink = Arc::new(MockSink::new());
        let sched = SchedulerLoop::new(reader, sink.clone(), SchedulerLoopConfig::default());

        let placed = sched.run_once().await.unwrap();
        assert_eq!(placed, 2);
    }

    #[tokio::test]
    async fn run_loop_processes_and_stops() {
        let alloc = AllocationBuilder::new().tenant("t1").nodes(1).build();
        let reader = Arc::new(make_reader(vec![alloc], 4));
        let sink = Arc::new(MockSink::new());

        let config = SchedulerLoopConfig {
            tick_interval: Duration::from_millis(50),
            ..Default::default()
        };
        let sched = SchedulerLoop::new(reader, sink.clone(), config);

        let (cancel_tx, cancel_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            sched.run(cancel_rx).await;
        });

        // Let it run a few cycles
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_tx.send(true).unwrap();
        handle.await.unwrap();

        // Should have placed at least once
        assert!(sink.assign_count.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn run_once_with_custom_weights() {
        let high = AllocationBuilder::new()
            .tenant("t1")
            .nodes(2)
            .preemption_class(8)
            .build();
        let low = AllocationBuilder::new()
            .tenant("t1")
            .nodes(2)
            .preemption_class(1)
            .build();
        let high_id = high.id;

        let reader = Arc::new(make_reader(vec![low, high], 2));
        let sink = Arc::new(MockSink::new());

        let config = SchedulerLoopConfig {
            weights: CostWeights {
                priority: 1.0,
                wait_time: 0.0,
                fair_share: 0.0,
                topology: 0.0,
                data_readiness: 0.0,
                backlog: 0.0,
                energy: 0.0,
                checkpoint_efficiency: 0.0,
                conformance: 0.0,
            },
            ..Default::default()
        };
        let sched = SchedulerLoop::new(reader, sink.clone(), config);

        let placed = sched.run_once().await.unwrap();
        assert_eq!(placed, 1);

        let assigned = sink.assigned.lock().await;
        assert_eq!(assigned[0].0, high_id);
    }

    #[tokio::test]
    async fn reader_error_propagates() {
        struct FailingReader;

        #[async_trait::async_trait]
        impl SchedulerStateReader for FailingReader {
            async fn pending_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
                Err(LatticeError::Internal("connection lost".into()))
            }
            async fn running_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
                Ok(vec![])
            }
            async fn available_nodes(&self) -> Result<Vec<Node>, LatticeError> {
                Ok(vec![])
            }
            async fn tenants(&self) -> Result<Vec<Tenant>, LatticeError> {
                Ok(vec![])
            }
            async fn topology(&self) -> TopologyModel {
                TopologyModel { groups: vec![] }
            }
        }

        let reader = Arc::new(FailingReader);
        let sink = Arc::new(MockSink::new());
        let sched = SchedulerLoop::new(reader, sink, SchedulerLoopConfig::default());

        let result = sched.run_once().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sink_error_skips_allocation() {
        struct FailingSink;

        #[async_trait::async_trait]
        impl SchedulerCommandSink for FailingSink {
            async fn assign_nodes(
                &self,
                _alloc_id: AllocId,
                _nodes: Vec<String>,
            ) -> Result<(), LatticeError> {
                Err(LatticeError::Internal("raft unavailable".into()))
            }
            async fn set_running(&self, _alloc_id: AllocId) -> Result<(), LatticeError> {
                Ok(())
            }
        }

        let alloc = AllocationBuilder::new().tenant("t1").nodes(1).build();
        let reader = Arc::new(make_reader(vec![alloc], 4));
        let sink = Arc::new(FailingSink);
        let sched = SchedulerLoop::new(reader, sink, SchedulerLoopConfig::default());

        // Should not crash, just skip the failed allocation
        let placed = sched.run_once().await.unwrap();
        assert_eq!(placed, 0);
    }
}

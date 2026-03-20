//! Main node agent — coordinates heartbeats, health checks,
//! allocation lifecycle, conformance, and checkpoint signaling.
//!
//! The agent connects to the quorum (via NodeRegistry trait) and
//! reports its status. During quorum outages it buffers state
//! updates for later replay.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use lattice_common::traits::NodeRegistry;
use lattice_common::types::{AllocId, NodeCapabilities, NodeId, NodeState};

use crate::allocation_runner::AllocationManager;
use crate::checkpoint_handler::CheckpointHandler;
use crate::conformance::{compute_fingerprint, ConformanceComponents};
use crate::health::{HealthChecker, HealthStatus, ObservedHealth};
use crate::heartbeat::{Heartbeat, HeartbeatGenerator};
use crate::heartbeat_loop::{HealthObserver, HeartbeatLoop, HeartbeatSink};
use crate::network::VniManager;
use crate::probe_manager::ProbeManager;
use crate::telemetry::{TelemetryCollector, TelemetryMode};

/// Commands received from the quorum/API.
#[derive(Debug, Clone)]
pub enum AgentCommand {
    StartAllocation {
        id: AllocId,
        entrypoint: String,
        liveness_probe: Option<lattice_common::types::LivenessProbe>,
    },
    StopAllocation {
        id: AllocId,
    },
    Checkpoint {
        id: AllocId,
    },
    Drain,
    Undrain,
    UpdateTelemetryMode(TelemetryMode),
}

/// State updates buffered during quorum outage.
#[derive(Debug, Clone)]
pub struct BufferedUpdate {
    pub heartbeat: Heartbeat,
    pub timestamp: DateTime<Utc>,
}

/// The per-node agent daemon.
///
/// Debug is manually implemented because `Arc<dyn NodeRegistry>` is not `Debug`.
pub struct NodeAgent {
    node_id: NodeId,
    registry: Arc<dyn NodeRegistry>,
    heartbeat_gen: HeartbeatGenerator,
    health_checker: HealthChecker,
    allocations: AllocationManager,
    probes: ProbeManager,
    checkpoints: CheckpointHandler,
    vni_manager: VniManager,
    telemetry: TelemetryCollector,
    conformance_fingerprint: Option<String>,
    buffered_updates: Vec<BufferedUpdate>,
}

impl std::fmt::Debug for NodeAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeAgent")
            .field("node_id", &self.node_id)
            .field("conformance_fingerprint", &self.conformance_fingerprint)
            .field("buffered_updates", &self.buffered_updates.len())
            .finish_non_exhaustive()
    }
}

impl NodeAgent {
    /// Create a new node agent for the given node.
    pub fn new(
        node_id: NodeId,
        capabilities: NodeCapabilities,
        registry: Arc<dyn NodeRegistry>,
    ) -> Self {
        Self {
            node_id: node_id.clone(),
            registry,
            heartbeat_gen: HeartbeatGenerator::new(node_id),
            health_checker: HealthChecker::new(capabilities),
            allocations: AllocationManager::new(),
            probes: ProbeManager::new(),
            checkpoints: CheckpointHandler::new(),
            vni_manager: VniManager::new(),
            telemetry: TelemetryCollector::new(TelemetryMode::Production),
            conformance_fingerprint: None,
            buffered_updates: Vec::new(),
        }
    }

    /// Compute and store the conformance fingerprint.
    pub fn compute_conformance(&mut self, components: &ConformanceComponents) {
        self.conformance_fingerprint = Some(compute_fingerprint(components));
    }

    /// Run a health check and generate a heartbeat.
    pub fn generate_heartbeat(&mut self, observed: &ObservedHealth) -> Heartbeat {
        let health = self.health_checker.check(observed);
        self.heartbeat_gen.generate(
            health.healthy(),
            health.issues(),
            self.allocations.active_count(),
            self.conformance_fingerprint.clone(),
        )
    }

    /// Send a heartbeat to the quorum. If the quorum is unreachable,
    /// buffer the update for later replay.
    pub async fn send_heartbeat(&mut self, observed: &ObservedHealth) -> Result<(), String> {
        let heartbeat = self.generate_heartbeat(observed);

        // Update the node's last_heartbeat timestamp in the registry
        match self.registry.get_node(&self.node_id).await {
            Ok(_node) => {
                // The registry is reachable — flush any buffered updates
                self.flush_buffered().await;
                Ok(())
            }
            Err(_) => {
                // Registry unreachable — buffer the heartbeat
                self.buffered_updates.push(BufferedUpdate {
                    heartbeat,
                    timestamp: Utc::now(),
                });
                Err("quorum unreachable, heartbeat buffered".to_string())
            }
        }
    }

    /// Process a command from the quorum/API.
    pub async fn handle_command(&mut self, cmd: AgentCommand) -> Result<(), String> {
        match cmd {
            AgentCommand::StartAllocation {
                id,
                entrypoint,
                liveness_probe,
            } => {
                self.allocations.start(id, entrypoint)?;
                // Advance through prologue
                self.allocations.advance(&id)?;
                // Register liveness probe if configured
                if let Some(probe) = liveness_probe {
                    self.probes.register(id, probe);
                }
                Ok(())
            }
            AgentCommand::StopAllocation { id } => {
                self.probes.deregister(&id);
                self.allocations.fail(&id, "stopped by command".to_string())
            }
            AgentCommand::Checkpoint { id } => {
                self.checkpoints
                    .request_checkpoint(id, crate::checkpoint_handler::CheckpointMode::Signal);
                Ok(())
            }
            AgentCommand::Drain => self
                .registry
                .update_node_state(&self.node_id, NodeState::Draining)
                .await
                .map_err(|e| e.to_string()),
            AgentCommand::Undrain => self
                .registry
                .update_node_state(&self.node_id, NodeState::Ready)
                .await
                .map_err(|e| e.to_string()),
            AgentCommand::UpdateTelemetryMode(mode) => {
                self.telemetry.set_mode(mode);
                Ok(())
            }
        }
    }

    /// Run a health check and return the status.
    pub fn check_health(&self, observed: &ObservedHealth) -> HealthStatus {
        self.health_checker.check(observed)
    }

    /// Get the current conformance fingerprint.
    pub fn conformance_fingerprint(&self) -> Option<&str> {
        self.conformance_fingerprint.as_deref()
    }

    /// Number of active allocations.
    pub fn active_allocation_count(&self) -> u32 {
        self.allocations.active_count()
    }

    /// Number of buffered updates (pending quorum reconnect).
    pub fn buffered_update_count(&self) -> usize {
        self.buffered_updates.len()
    }

    /// Access the probe manager.
    pub fn probes(&self) -> &ProbeManager {
        &self.probes
    }

    /// Access the probe manager mutably.
    pub fn probes_mut(&mut self) -> &mut ProbeManager {
        &mut self.probes
    }

    /// Access the allocation manager.
    pub fn allocations(&self) -> &AllocationManager {
        &self.allocations
    }

    /// Access the allocation manager mutably.
    pub fn allocations_mut(&mut self) -> &mut AllocationManager {
        &mut self.allocations
    }

    /// Access the checkpoint handler.
    pub fn checkpoints(&self) -> &CheckpointHandler {
        &self.checkpoints
    }

    /// Access the VNI manager.
    pub fn vni_manager(&self) -> &VniManager {
        &self.vni_manager
    }

    /// Access the VNI manager mutably.
    pub fn vni_manager_mut(&mut self) -> &mut VniManager {
        &mut self.vni_manager
    }

    /// Access the telemetry collector.
    pub fn telemetry(&self) -> &TelemetryCollector {
        &self.telemetry
    }

    /// Access the telemetry collector mutably.
    pub fn telemetry_mut(&mut self) -> &mut TelemetryCollector {
        &mut self.telemetry
    }

    /// The node ID this agent manages.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Replay buffered updates when quorum reconnects.
    async fn flush_buffered(&mut self) {
        // In a real implementation, each buffered heartbeat would be
        // replayed to the quorum. For now, we just clear the buffer.
        self.buffered_updates.clear();
    }

    /// Run the agent: spawn the heartbeat loop and listen for commands.
    ///
    /// * `sink` — where heartbeat payloads are delivered (e.g. gRPC client)
    /// * `observer` — source of health observations
    /// * `interval` — time between heartbeats
    /// * `cancel_rx` — watch channel; set to `true` to shut down
    /// * `cmd_rx` — channel of incoming `AgentCommand`s
    ///
    /// The method runs until the cancel signal fires.
    pub async fn run<S, H>(
        &mut self,
        sink: S,
        observer: H,
        interval: std::time::Duration,
        cancel_rx: tokio::sync::watch::Receiver<bool>,
        mut cmd_rx: tokio::sync::mpsc::Receiver<AgentCommand>,
    ) where
        S: HeartbeatSink + 'static,
        H: HealthObserver + 'static,
    {
        let mut heartbeat_loop = HeartbeatLoop::new(
            self.node_id.clone(),
            sink,
            observer,
            interval,
            cancel_rx.clone(),
        );

        // Share allocation count with the heartbeat loop so it stays current.
        let alloc_count = heartbeat_loop.allocation_count_handle();
        let conformance = heartbeat_loop.conformance_handle();

        // If we already have a conformance fingerprint, propagate it.
        if let Some(fp) = &self.conformance_fingerprint {
            *conformance.write().await = Some(fp.clone());
        }

        info!(node_id = %self.node_id, "agent run loop started");

        // Spawn the heartbeat loop in a background task.
        let hb_handle = tokio::spawn(async move {
            heartbeat_loop.run().await;
        });

        // Main command processing loop with liveness probe ticks.
        let mut cancel_rx_cmd = cancel_rx;
        let mut probe_interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    debug!(?cmd, "received agent command");
                    if let Err(e) = self.handle_command(cmd).await {
                        warn!(error = %e, "command handler error");
                    }
                    // Keep the heartbeat loop's allocation count in sync.
                    alloc_count.store(
                        self.allocations.active_count(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                _ = probe_interval.tick() => {
                    // Run liveness probes; any that exceed their threshold
                    // mark the allocation as failed.
                    let failed_ids = self.probes.tick().await;
                    for id in &failed_ids {
                        if let Err(e) = self.allocations.fail(id, "liveness probe failed".into()) {
                            warn!(alloc_id = %id, error = %e, "failed to mark allocation as failed");
                        }
                    }
                    if !failed_ids.is_empty() {
                        alloc_count.store(
                            self.allocations.active_count(),
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
                result = cancel_rx_cmd.changed() => {
                    if result.is_err() || *cancel_rx_cmd.borrow() {
                        info!("agent shutting down");
                        break;
                    }
                }
            }
        }

        // Wait for the heartbeat loop to finish (it watches the same cancel signal).
        let _ = hb_handle.await;
        info!(node_id = %self.node_id, "agent run loop stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::ConformanceComponents;
    use crate::telemetry::TelemetryMode;
    use lattice_common::types::NodeCapabilities;
    use lattice_test_harness::fixtures::create_node_batch;
    use lattice_test_harness::mocks::MockNodeRegistry;

    fn test_capabilities() -> NodeCapabilities {
        NodeCapabilities {
            gpu_type: Some("GH200".to_string()),
            gpu_count: 4,
            cpu_cores: 72,
            memory_gb: 512,
            features: vec![],
            gpu_topology: None,
            memory_topology: None,
        }
    }

    fn healthy_observed() -> ObservedHealth {
        ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 0,
            nic_up: true,
        }
    }

    fn test_agent() -> NodeAgent {
        let nodes = create_node_batch(1, 0);
        let node_id = nodes[0].id.clone();
        let registry = Arc::new(MockNodeRegistry::new().with_nodes(nodes));
        NodeAgent::new(node_id, test_capabilities(), registry)
    }

    #[test]
    fn agent_generates_heartbeat() {
        let mut agent = test_agent();
        let hb = agent.generate_heartbeat(&healthy_observed());
        assert!(hb.healthy);
        assert_eq!(hb.running_allocations, 0);
        assert_eq!(hb.sequence, 1);
    }

    #[test]
    fn agent_computes_conformance() {
        let mut agent = test_agent();
        assert!(agent.conformance_fingerprint().is_none());

        agent.compute_conformance(&ConformanceComponents {
            gpu_driver_version: "535.129.03".to_string(),
            nic_firmware_version: "22.39.1002".to_string(),
            ..Default::default()
        });

        assert!(agent.conformance_fingerprint().is_some());
        assert_eq!(agent.conformance_fingerprint().unwrap().len(), 64);
    }

    #[test]
    fn heartbeat_includes_conformance() {
        let mut agent = test_agent();
        agent.compute_conformance(&ConformanceComponents::default());

        let hb = agent.generate_heartbeat(&healthy_observed());
        assert!(hb.conformance_fingerprint.is_some());
    }

    #[tokio::test]
    async fn agent_starts_allocation() {
        let mut agent = test_agent();
        let id = uuid::Uuid::new_v4();

        agent
            .handle_command(AgentCommand::StartAllocation {
                id,
                entrypoint: "python train.py".to_string(),
                liveness_probe: None,
            })
            .await
            .unwrap();

        assert_eq!(agent.active_allocation_count(), 1);
    }

    #[tokio::test]
    async fn agent_stops_allocation() {
        let mut agent = test_agent();
        let id = uuid::Uuid::new_v4();

        agent
            .handle_command(AgentCommand::StartAllocation {
                id,
                entrypoint: "train.py".to_string(),
                liveness_probe: None,
            })
            .await
            .unwrap();

        agent
            .handle_command(AgentCommand::StopAllocation { id })
            .await
            .unwrap();

        assert_eq!(agent.active_allocation_count(), 0);
    }

    #[tokio::test]
    async fn agent_handles_checkpoint_command() {
        let mut agent = test_agent();
        let id = uuid::Uuid::new_v4();

        agent
            .handle_command(AgentCommand::Checkpoint { id })
            .await
            .unwrap();

        assert_eq!(agent.checkpoints().pending_for(&id).len(), 1);
    }

    #[tokio::test]
    async fn agent_handles_drain() {
        let mut agent = test_agent();
        agent.handle_command(AgentCommand::Drain).await.unwrap();
    }

    #[tokio::test]
    async fn agent_handles_undrain() {
        let mut agent = test_agent();
        agent.handle_command(AgentCommand::Undrain).await.unwrap();
    }

    #[tokio::test]
    async fn agent_updates_telemetry_mode() {
        let mut agent = test_agent();
        assert_eq!(agent.telemetry().mode(), TelemetryMode::Production);

        agent
            .handle_command(AgentCommand::UpdateTelemetryMode(TelemetryMode::Debug))
            .await
            .unwrap();

        assert_eq!(agent.telemetry().mode(), TelemetryMode::Debug);
    }

    #[tokio::test]
    async fn heartbeat_succeeds_when_registry_available() {
        let mut agent = test_agent();
        let result = agent.send_heartbeat(&healthy_observed()).await;
        assert!(result.is_ok());
        assert_eq!(agent.buffered_update_count(), 0);
    }

    #[tokio::test]
    async fn heartbeat_buffered_when_registry_unavailable() {
        // Create agent with empty registry (node not found → simulates unreachable)
        let registry = Arc::new(MockNodeRegistry::new());
        let mut agent = NodeAgent::new(
            "nonexistent-node".to_string(),
            test_capabilities(),
            registry,
        );

        let result = agent.send_heartbeat(&healthy_observed()).await;
        assert!(result.is_err());
        assert_eq!(agent.buffered_update_count(), 1);
    }

    #[test]
    fn heartbeat_reflects_allocation_count() {
        let mut agent = test_agent();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();

        agent.allocations_mut().start(id1, "a".to_string()).unwrap();
        agent.allocations_mut().start(id2, "b".to_string()).unwrap();

        let hb = agent.generate_heartbeat(&healthy_observed());
        assert_eq!(hb.running_allocations, 2);
    }

    #[test]
    fn unhealthy_node_heartbeat() {
        let mut agent = test_agent();
        let observed = ObservedHealth {
            gpu_count: 2, // Missing GPUs
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 0,
            nic_up: true,
        };

        let hb = agent.generate_heartbeat(&observed);
        assert!(!hb.healthy);
        assert!(!hb.issues.is_empty());
    }

    // ── Tests for the `run()` method ──────────────────────────

    use crate::heartbeat::Heartbeat;
    use crate::heartbeat_loop::StaticHealthObserver;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Mock sink that records heartbeats for assertion.
    struct RecordingSink {
        heartbeats: Arc<Mutex<Vec<Heartbeat>>>,
    }

    impl RecordingSink {
        fn new() -> (Self, Arc<Mutex<Vec<Heartbeat>>>) {
            let store = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    heartbeats: store.clone(),
                },
                store,
            )
        }
    }

    #[async_trait::async_trait]
    impl HeartbeatSink for RecordingSink {
        async fn send(&self, heartbeat: Heartbeat) -> Result<(), String> {
            self.heartbeats.lock().unwrap().push(heartbeat);
            Ok(())
        }
    }

    #[tokio::test]
    async fn agent_run_sends_heartbeats_and_processes_commands() {
        let mut agent = test_agent();
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(16);

        // Send a start command after a short delay, then cancel.
        let alloc_id = uuid::Uuid::new_v4();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cmd_tx
                .send(AgentCommand::StartAllocation {
                    id: alloc_id,
                    entrypoint: "train.py".to_string(),
                    liveness_probe: None,
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = cancel_tx.send(true);
        });

        agent
            .run(sink, observer, Duration::from_millis(15), cancel_rx, cmd_rx)
            .await;

        // Heartbeats should have been sent.
        let hbs = store.lock().unwrap();
        assert!(!hbs.is_empty(), "expected heartbeats to be sent");

        // The start command should have been processed.
        assert_eq!(agent.active_allocation_count(), 1);
    }

    #[tokio::test]
    async fn agent_run_stops_on_cancel() {
        let mut agent = test_agent();
        let (sink, _store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(16);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = cancel_tx.send(true);
        });

        agent
            .run(sink, observer, Duration::from_millis(10), cancel_rx, cmd_rx)
            .await;

        // If we get here, the loop exited cleanly.
    }
}

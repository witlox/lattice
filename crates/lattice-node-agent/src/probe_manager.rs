//! Probe manager — runs liveness probes for service allocations.
//!
//! Integrates with the node agent's tick loop. On each tick:
//! 1. Check which Running allocations have liveness probes configured
//! 2. For each, check if period has elapsed since last probe
//! 3. Run the probe asynchronously
//! 4. Track failures; return allocation IDs that exceeded the threshold

use std::collections::HashMap;
use std::time::Instant;

use lattice_common::types::{AllocId, LivenessProbe};
use tracing::{debug, info};

use crate::liveness::{run_probe, ProbeState};

/// Manages liveness probes for all allocations on a single node.
pub struct ProbeManager {
    /// Active probes, keyed by allocation ID.
    probes: HashMap<AllocId, ManagedProbe>,
    /// IP/hostname this node agent uses for probe targets.
    /// Probes connect to localhost since the workload runs on this node.
    target_addr: String,
}

struct ManagedProbe {
    state: ProbeState,
    /// When the initial delay expires and probing should begin.
    probe_start: Instant,
    /// When the last probe was executed.
    last_probe: Option<Instant>,
}

impl ProbeManager {
    pub fn new() -> Self {
        Self {
            probes: HashMap::new(),
            target_addr: "127.0.0.1".to_string(),
        }
    }

    /// Register a liveness probe for an allocation entering Running phase.
    pub fn register(&mut self, alloc_id: AllocId, probe: LivenessProbe) {
        let initial_delay = std::time::Duration::from_secs(probe.initial_delay_secs as u64);
        let probe_start = Instant::now() + initial_delay;
        debug!(
            alloc_id = %alloc_id,
            period = probe.period_secs,
            initial_delay = probe.initial_delay_secs,
            threshold = probe.failure_threshold,
            "Registered liveness probe"
        );
        self.probes.insert(
            alloc_id,
            ManagedProbe {
                state: ProbeState::new(alloc_id, probe),
                probe_start,
                last_probe: None,
            },
        );
    }

    /// Deregister a probe (allocation completed, failed, or cancelled).
    pub fn deregister(&mut self, alloc_id: &AllocId) {
        self.probes.remove(alloc_id);
    }

    /// Run a tick: execute due probes and return IDs of allocations that
    /// have exceeded their failure threshold (should be marked Failed).
    pub async fn tick(&mut self) -> Vec<AllocId> {
        let now = Instant::now();
        let mut failed = Vec::new();

        for (alloc_id, managed) in self.probes.iter_mut() {
            // Wait for initial delay
            if now < managed.probe_start {
                continue;
            }

            // Check if period has elapsed
            let period = std::time::Duration::from_secs(managed.state.probe.period_secs as u64);
            if let Some(last) = managed.last_probe {
                if now.duration_since(last) < period {
                    continue;
                }
            }

            // Run the probe
            managed.last_probe = Some(now);
            let result = run_probe(&managed.state.probe, &self.target_addr).await;

            let should_fail = managed.state.record(result);
            if should_fail {
                info!(
                    alloc_id = %alloc_id,
                    failures = managed.state.consecutive_failures,
                    "Liveness probe threshold exceeded, marking allocation failed"
                );
                failed.push(*alloc_id);
            }
        }

        // Remove probes for allocations that failed
        for id in &failed {
            self.probes.remove(id);
        }

        failed
    }

    /// Number of actively tracked probes.
    pub fn probe_count(&self) -> usize {
        self.probes.len()
    }

    /// Check if a probe is registered for an allocation.
    pub fn has_probe(&self, alloc_id: &AllocId) -> bool {
        self.probes.contains_key(alloc_id)
    }
}

impl Default for ProbeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::types::ProbeType;
    use uuid::Uuid;

    fn test_probe(failure_threshold: u32) -> LivenessProbe {
        LivenessProbe {
            probe_type: ProbeType::Tcp { port: 1 }, // port 1 will always fail
            period_secs: 0,                         // immediate
            initial_delay_secs: 0,
            failure_threshold,
            timeout_secs: 1,
        }
    }

    #[test]
    fn register_and_deregister() {
        let mut mgr = ProbeManager::new();
        let id = Uuid::new_v4();
        mgr.register(id, test_probe(3));
        assert!(mgr.has_probe(&id));
        assert_eq!(mgr.probe_count(), 1);

        mgr.deregister(&id);
        assert!(!mgr.has_probe(&id));
        assert_eq!(mgr.probe_count(), 0);
    }

    #[tokio::test]
    async fn probe_fails_after_threshold() {
        let mut mgr = ProbeManager::new();
        let id = Uuid::new_v4();
        // Threshold of 2 — probe to port 1 (closed) will fail each tick
        mgr.register(id, test_probe(2));

        // First tick: 1 failure, not yet threshold
        let failed = mgr.tick().await;
        assert!(failed.is_empty());

        // Second tick: 2 failures = threshold reached
        let failed = mgr.tick().await;
        assert_eq!(failed, vec![id]);

        // Probe should be removed after failure
        assert!(!mgr.has_probe(&id));
    }

    #[tokio::test]
    async fn initial_delay_defers_probing() {
        let mut mgr = ProbeManager::new();
        let id = Uuid::new_v4();
        let mut probe = test_probe(1);
        probe.initial_delay_secs = 3600; // 1 hour delay

        mgr.register(id, probe);

        // Should not probe yet (initial delay)
        let failed = mgr.tick().await;
        assert!(failed.is_empty());
        assert!(mgr.has_probe(&id));
    }

    #[tokio::test]
    async fn no_probes_returns_empty() {
        let mut mgr = ProbeManager::new();
        let failed = mgr.tick().await;
        assert!(failed.is_empty());
    }
}

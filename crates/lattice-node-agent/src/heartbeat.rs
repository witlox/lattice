//! Heartbeat protocol — periodic liveness signal to the quorum.
//!
//! Each heartbeat carries a monotonic sequence number (for replay
//! detection), a health summary, running allocation count, and
//! optional conformance fingerprint.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use lattice_common::types::{CompletionReport, NodeId};

/// Payload sent from node agent to quorum every `heartbeat_interval`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub node_id: NodeId,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub healthy: bool,
    pub issues: Vec<String>,
    pub running_allocations: u32,
    pub conformance_fingerprint: Option<String>,
    /// Owner version for stale heartbeat rejection (ADV-06).
    #[serde(default)]
    pub owner_version: u64,
    /// Agent is still reattaching to surviving Workload Processes after
    /// a restart (DEC-DISP-10 / INV-D5). Silent-sweep (INV-D8) honours this
    /// flag subject to the lifecycle rules at the quorum.
    #[serde(default)]
    pub reattach_in_progress: bool,
    /// Per-allocation state changes accumulated since the previous
    /// heartbeat (INV-D13 latest-wins per allocation).
    #[serde(default)]
    pub completion_reports: Vec<CompletionReport>,
}

/// Generates heartbeats with monotonically increasing sequence numbers.
pub struct HeartbeatGenerator {
    node_id: NodeId,
    next_sequence: u64,
}

impl HeartbeatGenerator {
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            next_sequence: 1,
        }
    }

    /// Generate a new heartbeat with current health data.
    #[allow(clippy::too_many_arguments)]
    pub fn generate(
        &mut self,
        healthy: bool,
        issues: Vec<String>,
        running_allocations: u32,
        conformance_fingerprint: Option<String>,
        reattach_in_progress: bool,
        completion_reports: Vec<CompletionReport>,
    ) -> Heartbeat {
        let seq = self.next_sequence;
        self.next_sequence += 1;
        Heartbeat {
            node_id: self.node_id.clone(),
            sequence: seq,
            timestamp: Utc::now(),
            healthy,
            issues,
            running_allocations,
            conformance_fingerprint,
            owner_version: 0, // Updated by agent when ownership changes
            reattach_in_progress,
            completion_reports,
        }
    }

    pub fn current_sequence(&self) -> u64 {
        self.next_sequence - 1
    }
}

/// Tracks received heartbeats and detects timeouts.
pub struct HeartbeatTracker {
    last_received: Option<DateTime<Utc>>,
    last_sequence: u64,
    timeout_secs: u64,
    grace_period_secs: u64,
}

impl HeartbeatTracker {
    /// Create a tracker with standard timeouts.
    pub fn standard() -> Self {
        Self {
            last_received: None,
            last_sequence: 0,
            timeout_secs: 30,
            grace_period_secs: 60,
        }
    }

    /// Create a tracker with sensitive workload timeouts (more conservative).
    pub fn sensitive() -> Self {
        Self {
            last_received: None,
            last_sequence: 0,
            timeout_secs: 120,
            grace_period_secs: 300,
        }
    }

    /// Create a tracker with borrowed-node timeouts (more aggressive).
    pub fn borrowed() -> Self {
        Self {
            last_received: None,
            last_sequence: 0,
            timeout_secs: 30,
            grace_period_secs: 30,
        }
    }

    /// Record a received heartbeat. Returns false if sequence number is stale.
    pub fn record(&mut self, heartbeat: &Heartbeat) -> bool {
        if heartbeat.sequence <= self.last_sequence {
            return false; // Replay detected
        }
        self.last_sequence = heartbeat.sequence;
        self.last_received = Some(heartbeat.timestamp);
        true
    }

    /// Check if the node has timed out (should transition to Degraded).
    pub fn is_timed_out(&self, now: DateTime<Utc>) -> bool {
        match self.last_received {
            None => false, // Never received — we don't know yet
            Some(last) => {
                let elapsed = (now - last).num_seconds() as u64;
                elapsed > self.timeout_secs
            }
        }
    }

    /// Check if the grace period has expired (should transition to Down).
    pub fn is_grace_expired(&self, now: DateTime<Utc>) -> bool {
        match self.last_received {
            None => false,
            Some(last) => {
                let elapsed = (now - last).num_seconds() as u64;
                elapsed > self.timeout_secs + self.grace_period_secs
            }
        }
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    pub fn grace_period_secs(&self) -> u64 {
        self.grace_period_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn heartbeat_generator_increments_sequence() {
        let mut gen = HeartbeatGenerator::new("node-0".to_string());

        let hb1 = gen.generate(true, vec![], 0, None, false, Vec::new());
        assert_eq!(hb1.sequence, 1);
        assert_eq!(hb1.node_id, "node-0");

        let hb2 = gen.generate(true, vec![], 1, None, false, Vec::new());
        assert_eq!(hb2.sequence, 2);

        let hb3 = gen.generate(false, vec!["gpu error".into()], 0, None, false, Vec::new());
        assert_eq!(hb3.sequence, 3);
        assert!(!hb3.healthy);
        assert_eq!(hb3.issues.len(), 1);
    }

    #[test]
    fn heartbeat_carries_conformance_fingerprint() {
        let mut gen = HeartbeatGenerator::new("node-0".to_string());
        let hb = gen.generate(
            true,
            vec![],
            0,
            Some("abc123".to_string()),
            false,
            Vec::new(),
        );
        assert_eq!(hb.conformance_fingerprint.as_deref(), Some("abc123"));
    }

    #[test]
    fn tracker_records_heartbeat() {
        let mut tracker = HeartbeatTracker::standard();
        let mut gen = HeartbeatGenerator::new("node-0".to_string());
        let hb = gen.generate(true, vec![], 0, None, false, Vec::new());

        assert!(tracker.record(&hb));
        assert_eq!(tracker.last_sequence(), 1);
    }

    #[test]
    fn tracker_rejects_replay() {
        let mut tracker = HeartbeatTracker::standard();
        let mut gen = HeartbeatGenerator::new("node-0".to_string());

        let hb1 = gen.generate(true, vec![], 0, None, false, Vec::new());
        let hb2 = gen.generate(true, vec![], 0, None, false, Vec::new());

        assert!(tracker.record(&hb2));
        // hb1 has lower sequence than hb2 — should be rejected
        assert!(!tracker.record(&hb1));
    }

    #[test]
    fn tracker_detects_timeout() {
        let mut tracker = HeartbeatTracker::standard();
        let past = Utc::now() - Duration::seconds(40);
        let hb = Heartbeat {
            node_id: "node-0".to_string(),
            sequence: 1,
            timestamp: past,
            healthy: true,
            issues: vec![],
            running_allocations: 0,
            conformance_fingerprint: None,
            owner_version: 0,
            reattach_in_progress: false,
            completion_reports: Vec::new(),
        };

        tracker.record(&hb);
        assert!(tracker.is_timed_out(Utc::now()));
        assert!(!tracker.is_grace_expired(Utc::now()));
    }

    #[test]
    fn tracker_detects_grace_expired() {
        let mut tracker = HeartbeatTracker::standard();
        // 30s timeout + 60s grace = 90s total
        let past = Utc::now() - Duration::seconds(100);
        let hb = Heartbeat {
            node_id: "node-0".to_string(),
            sequence: 1,
            timestamp: past,
            healthy: true,
            issues: vec![],
            running_allocations: 0,
            conformance_fingerprint: None,
            owner_version: 0,
            reattach_in_progress: false,
            completion_reports: Vec::new(),
        };

        tracker.record(&hb);
        assert!(tracker.is_timed_out(Utc::now()));
        assert!(tracker.is_grace_expired(Utc::now()));
    }

    #[test]
    fn sensitive_tracker_has_longer_timeouts() {
        let tracker = HeartbeatTracker::sensitive();
        assert_eq!(tracker.timeout_secs(), 120);
        assert_eq!(tracker.grace_period_secs(), 300);
    }

    #[test]
    fn borrowed_tracker_has_shorter_grace() {
        let tracker = HeartbeatTracker::borrowed();
        assert_eq!(tracker.timeout_secs(), 30);
        assert_eq!(tracker.grace_period_secs(), 30);
    }

    #[test]
    fn tracker_not_timed_out_when_never_received() {
        let tracker = HeartbeatTracker::standard();
        assert!(!tracker.is_timed_out(Utc::now()));
        assert!(!tracker.is_grace_expired(Utc::now()));
    }

    #[test]
    fn tracker_not_timed_out_when_recent() {
        let mut tracker = HeartbeatTracker::standard();
        let hb = Heartbeat {
            node_id: "node-0".to_string(),
            sequence: 1,
            timestamp: Utc::now(),
            healthy: true,
            issues: vec![],
            running_allocations: 0,
            conformance_fingerprint: None,
            owner_version: 0,
            reattach_in_progress: false,
            completion_reports: Vec::new(),
        };
        tracker.record(&hb);
        assert!(!tracker.is_timed_out(Utc::now()));
    }
}

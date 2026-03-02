//! Periodic heartbeat loop — drives the heartbeat/health-check cycle
//! and delivers payloads through a pluggable `HeartbeatSink`.
//!
//! The loop uses `tokio::time::interval` for periodic ticks and a
//! `tokio::sync::watch` channel for graceful cancellation.

use std::time::Duration;

use async_trait::async_trait;
use tracing::{debug, warn};

use crate::health::ObservedHealth;
use crate::heartbeat::{Heartbeat, HeartbeatGenerator};

/// Trait for delivering heartbeat payloads.
///
/// Implemented by the real quorum client (gRPC) and by test mocks.
#[async_trait]
pub trait HeartbeatSink: Send + Sync {
    /// Send a heartbeat payload. Returns an error description on failure.
    async fn send(&self, heartbeat: Heartbeat) -> Result<(), String>;
}

/// Collects observed health values from the node.
///
/// In production this would read from sysfs / NVML / etc.
/// For testing it can be stubbed to return fixed values.
#[async_trait]
pub trait HealthObserver: Send + Sync {
    /// Collect current health observations.
    async fn observe(&self) -> ObservedHealth;
}

/// A simple static health observer that always returns the same values.
///
/// Useful for testing and as a placeholder until real hardware
/// probing is wired up.
pub struct StaticHealthObserver {
    observed: ObservedHealth,
}

impl StaticHealthObserver {
    pub fn new(observed: ObservedHealth) -> Self {
        Self { observed }
    }
}

#[async_trait]
impl HealthObserver for StaticHealthObserver {
    async fn observe(&self) -> ObservedHealth {
        self.observed.clone()
    }
}

/// Drives the periodic heartbeat cycle.
///
/// Each tick:
/// 1. Collects health observations via `HealthObserver`
/// 2. Generates a `Heartbeat` payload (with monotonic sequence)
/// 3. Delivers it through the `HeartbeatSink`
///
/// The loop is cancelled via a `tokio::sync::watch` channel: when the
/// watched value becomes `true`, the loop exits cleanly.
pub struct HeartbeatLoop<S: HeartbeatSink, H: HealthObserver> {
    generator: HeartbeatGenerator,
    sink: S,
    observer: H,
    interval: Duration,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
    active_allocation_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    conformance_fingerprint: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
}

impl<S: HeartbeatSink, H: HealthObserver> HeartbeatLoop<S, H> {
    /// Create a new heartbeat loop.
    ///
    /// * `node_id` — identifier of the node this agent manages
    /// * `sink` — where heartbeat payloads are delivered
    /// * `observer` — source of health observations
    /// * `interval` — time between heartbeats (default: 10 s)
    /// * `cancel_rx` — watch channel receiver; loop exits when value is `true`
    pub fn new(
        node_id: String,
        sink: S,
        observer: H,
        interval: Duration,
        cancel_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            generator: HeartbeatGenerator::new(node_id),
            sink,
            observer,
            interval,
            cancel_rx,
            active_allocation_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            conformance_fingerprint: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    /// Get a handle that can update the active allocation count.
    pub fn allocation_count_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicU32> {
        self.active_allocation_count.clone()
    }

    /// Get a handle that can update the conformance fingerprint.
    pub fn conformance_handle(&self) -> std::sync::Arc<tokio::sync::RwLock<Option<String>>> {
        self.conformance_fingerprint.clone()
    }

    /// Execute a single heartbeat cycle: observe, generate, send.
    ///
    /// Returns `Ok(())` if the heartbeat was sent successfully, or
    /// `Err(msg)` if the sink reported an error. Sink errors are
    /// non-fatal — the loop logs them and continues.
    pub async fn run_once(&mut self) -> Result<(), String> {
        let observed = self.observer.observe().await;
        let alloc_count = self
            .active_allocation_count
            .load(std::sync::atomic::Ordering::Relaxed);
        let fingerprint = self.conformance_fingerprint.read().await.clone();

        // Determine health from observations (simplified: delegate to the
        // same logic the HealthChecker uses, but here we only check if the
        // observation has obvious failures — the full HealthChecker is in
        // the agent proper).
        let healthy = observed.nic_up && observed.ecc_errors < 10;
        let mut issues = Vec::new();
        if !observed.nic_up {
            issues.push("NIC is down".to_string());
        }
        if observed.ecc_errors >= 10 {
            issues.push(format!(
                "ECC error count {} exceeds threshold",
                observed.ecc_errors
            ));
        }

        let heartbeat = self
            .generator
            .generate(healthy, issues, alloc_count, fingerprint);

        debug!(
            seq = heartbeat.sequence,
            healthy = heartbeat.healthy,
            "sending heartbeat"
        );

        self.sink.send(heartbeat).await
    }

    /// Run the heartbeat loop until cancelled.
    ///
    /// Uses `tokio::select!` to wait on either:
    /// - the next interval tick, or
    /// - the cancellation signal.
    ///
    /// Sink errors are logged but do not stop the loop.
    pub async fn run(&mut self) {
        let mut ticker = tokio::time::interval(self.interval);
        // The first tick fires immediately — consume it and send the
        // initial heartbeat right away.
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = self.run_once().await {
                        warn!(error = %e, "heartbeat send failed");
                    }
                }
                result = self.cancel_rx.changed() => {
                    // Channel closed or value changed
                    if result.is_err() || *self.cancel_rx.borrow() {
                        debug!("heartbeat loop cancelled");
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    /// Mock sink that records all heartbeats it receives.
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

    #[async_trait]
    impl HeartbeatSink for RecordingSink {
        async fn send(&self, heartbeat: Heartbeat) -> Result<(), String> {
            self.heartbeats.lock().unwrap().push(heartbeat);
            Ok(())
        }
    }

    /// Mock sink that always fails.
    struct FailingSink;

    #[async_trait]
    impl HeartbeatSink for FailingSink {
        async fn send(&self, _heartbeat: Heartbeat) -> Result<(), String> {
            Err("connection refused".to_string())
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

    #[tokio::test]
    async fn run_once_sends_heartbeat() {
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_secs(10),
            rx,
        );

        let result = hb_loop.run_once().await;
        assert!(result.is_ok());

        let hbs = store.lock().unwrap();
        assert_eq!(hbs.len(), 1);
        assert_eq!(hbs[0].node_id, "node-0");
        assert_eq!(hbs[0].sequence, 1);
        assert!(hbs[0].healthy);
    }

    #[tokio::test]
    async fn run_once_increments_sequence() {
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_secs(10),
            rx,
        );

        hb_loop.run_once().await.unwrap();
        hb_loop.run_once().await.unwrap();
        hb_loop.run_once().await.unwrap();

        let hbs = store.lock().unwrap();
        assert_eq!(hbs.len(), 3);
        assert_eq!(hbs[0].sequence, 1);
        assert_eq!(hbs[1].sequence, 2);
        assert_eq!(hbs[2].sequence, 3);
    }

    #[tokio::test]
    async fn run_once_reports_sink_error() {
        let observer = StaticHealthObserver::new(healthy_observed());
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            FailingSink,
            observer,
            Duration::from_secs(10),
            rx,
        );

        let result = hb_loop.run_once().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("connection refused"));
    }

    #[tokio::test]
    async fn sink_errors_do_not_crash_loop() {
        let observer = StaticHealthObserver::new(healthy_observed());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            FailingSink,
            observer,
            Duration::from_millis(10),
            rx,
        );

        // Cancel after a brief delay — the loop should survive the sink errors.
        let cancel_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(true);
        });

        hb_loop.run().await;
        cancel_handle.await.unwrap();
        // If we get here, the loop did not panic.
    }

    #[tokio::test]
    async fn cancel_stops_loop() {
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_millis(10),
            rx,
        );

        // Cancel after letting a few heartbeats fire.
        let cancel_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(55)).await;
            let _ = tx.send(true);
        });

        hb_loop.run().await;
        cancel_handle.await.unwrap();

        let hbs = store.lock().unwrap();
        // We should have at least 1 heartbeat (immediate first tick) and at most
        // a handful — the exact count depends on scheduling but the loop must stop.
        assert!(!hbs.is_empty(), "expected at least one heartbeat");
        // Sequences must be strictly increasing.
        for window in hbs.windows(2) {
            assert!(window[1].sequence > window[0].sequence);
        }
    }

    #[tokio::test]
    async fn heartbeats_fire_at_interval() {
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_millis(20),
            rx,
        );

        let cancel_handle = tokio::spawn(async move {
            // Let ~5 ticks fire (20ms * 5 = 100ms) plus some slack.
            tokio::time::sleep(Duration::from_millis(110)).await;
            let _ = tx.send(true);
        });

        hb_loop.run().await;
        cancel_handle.await.unwrap();

        let count = store.lock().unwrap().len();
        // With 20ms interval over ~110ms we expect roughly 5-6 heartbeats
        // (the first fires immediately). Allow some tolerance for CI.
        assert!(
            (3..=10).contains(&count),
            "expected 3-10 heartbeats, got {count}"
        );
    }

    #[tokio::test]
    async fn allocation_count_reflected_in_heartbeat() {
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_secs(10),
            rx,
        );

        hb_loop
            .allocation_count_handle()
            .store(3, Ordering::Relaxed);

        hb_loop.run_once().await.unwrap();

        let hbs = store.lock().unwrap();
        assert_eq!(hbs[0].running_allocations, 3);
    }

    #[tokio::test]
    async fn conformance_fingerprint_reflected_in_heartbeat() {
        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_secs(10),
            rx,
        );

        // Set a fingerprint before sending
        *hb_loop.conformance_handle().write().await = Some("abc123".to_string());

        hb_loop.run_once().await.unwrap();

        let hbs = store.lock().unwrap();
        assert_eq!(hbs[0].conformance_fingerprint.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn unhealthy_observation_produces_unhealthy_heartbeat() {
        let unhealthy = ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 0,
            nic_up: false, // NIC down
        };

        let (sink, store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(unhealthy);
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_secs(10),
            rx,
        );

        hb_loop.run_once().await.unwrap();

        let hbs = store.lock().unwrap();
        assert!(!hbs[0].healthy);
        assert!(!hbs[0].issues.is_empty());
    }

    #[tokio::test]
    async fn cancel_channel_closed_stops_loop() {
        let (sink, _store) = RecordingSink::new();
        let observer = StaticHealthObserver::new(healthy_observed());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let mut hb_loop = HeartbeatLoop::new(
            "node-0".to_string(),
            sink,
            observer,
            Duration::from_millis(10),
            rx,
        );

        // Drop the sender — the receiver will see an error on changed().
        let cancel_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            drop(tx);
        });

        hb_loop.run().await;
        cancel_handle.await.unwrap();
        // Loop exited cleanly — test passes.
    }
}

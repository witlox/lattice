//! Stub interfaces for eBPF-based telemetry collectors.
//!
//! Real eBPF programs are loaded in production via libbpf. This module
//! defines the trait boundary so the rest of the agent can program against
//! a stable interface, and provides a `StubEbpfCollector` for testing and
//! platforms where eBPF is not available.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single event produced by an eBPF program.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EbpfEvent {
    /// Timestamp when the event was captured.
    pub timestamp: DateTime<Utc>,
    /// The kind of event (e.g. "gpu_util", "net_rx", "syscall").
    pub kind: String,
    /// Opaque payload -- the actual shape depends on the eBPF program.
    pub payload: Vec<u8>,
}

/// Lifecycle states for an eBPF collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorState {
    /// Created but not yet attached.
    Detached,
    /// Attached and actively collecting events.
    Attached,
}

/// Trait for an eBPF-based telemetry collector.
///
/// Implementations are expected to:
/// 1. Load and attach one or more eBPF programs on `attach()`.
/// 2. Detach and unload programs on `detach()`.
/// 3. Drain captured events via `read_events()`.
///
/// The trait is `async` because real eBPF interactions may involve
/// kernel ring buffer polling.
#[async_trait]
pub trait EbpfCollector: Send + Sync {
    /// Attach the eBPF programs to their kernel hooks.
    /// Returns an error if already attached or if the load fails.
    async fn attach(&mut self) -> Result<(), String>;

    /// Detach the eBPF programs.
    /// Returns an error if not currently attached.
    async fn detach(&mut self) -> Result<(), String>;

    /// Read and drain all pending events from the eBPF ring buffer.
    async fn read_events(&mut self) -> Result<Vec<EbpfEvent>, String>;

    /// Current lifecycle state.
    fn state(&self) -> CollectorState;
}

/// Stub implementation of [`EbpfCollector`] for testing and non-Linux platforms.
///
/// It tracks attach/detach state and can be pre-loaded with canned events
/// that will be returned from `read_events()`.
pub struct StubEbpfCollector {
    state: CollectorState,
    /// Pre-loaded events that will be drained on `read_events()`.
    pending_events: Vec<EbpfEvent>,
}

impl StubEbpfCollector {
    /// Create a new stub collector in the detached state.
    pub fn new() -> Self {
        Self {
            state: CollectorState::Detached,
            pending_events: Vec::new(),
        }
    }

    /// Pre-load events that will be returned by the next `read_events()` call.
    pub fn inject_events(&mut self, events: Vec<EbpfEvent>) {
        self.pending_events.extend(events);
    }
}

impl Default for StubEbpfCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EbpfCollector for StubEbpfCollector {
    async fn attach(&mut self) -> Result<(), String> {
        if self.state == CollectorState::Attached {
            return Err("already attached".to_string());
        }
        self.state = CollectorState::Attached;
        Ok(())
    }

    async fn detach(&mut self) -> Result<(), String> {
        if self.state == CollectorState::Detached {
            return Err("not attached".to_string());
        }
        self.state = CollectorState::Detached;
        Ok(())
    }

    async fn read_events(&mut self) -> Result<Vec<EbpfEvent>, String> {
        if self.state != CollectorState::Attached {
            return Err("collector is not attached".to_string());
        }
        Ok(std::mem::take(&mut self.pending_events))
    }

    fn state(&self) -> CollectorState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_collector_is_detached() {
        let collector = StubEbpfCollector::new();
        assert_eq!(collector.state(), CollectorState::Detached);
    }

    #[tokio::test]
    async fn attach_transitions_to_attached() {
        let mut collector = StubEbpfCollector::new();
        collector.attach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Attached);
    }

    #[tokio::test]
    async fn detach_transitions_to_detached() {
        let mut collector = StubEbpfCollector::new();
        collector.attach().await.unwrap();
        collector.detach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Detached);
    }

    #[tokio::test]
    async fn double_attach_fails() {
        let mut collector = StubEbpfCollector::new();
        collector.attach().await.unwrap();
        let result = collector.attach().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "already attached");
    }

    #[tokio::test]
    async fn detach_without_attach_fails() {
        let mut collector = StubEbpfCollector::new();
        let result = collector.detach().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "not attached");
    }

    #[tokio::test]
    async fn read_events_when_detached_fails() {
        let mut collector = StubEbpfCollector::new();
        let result = collector.read_events().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "collector is not attached");
    }

    #[tokio::test]
    async fn read_events_returns_empty_by_default() {
        let mut collector = StubEbpfCollector::new();
        collector.attach().await.unwrap();
        let events = collector.read_events().await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn injected_events_are_drained() {
        let mut collector = StubEbpfCollector::new();
        collector.inject_events(vec![
            EbpfEvent {
                timestamp: Utc::now(),
                kind: "gpu_util".to_string(),
                payload: vec![80],
            },
            EbpfEvent {
                timestamp: Utc::now(),
                kind: "net_rx".to_string(),
                payload: vec![1, 2, 3],
            },
        ]);

        collector.attach().await.unwrap();

        let events = collector.read_events().await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "gpu_util");
        assert_eq!(events[1].kind, "net_rx");

        // Second read should be empty (events were drained).
        let events = collector.read_events().await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn full_lifecycle_attach_read_detach() {
        let mut collector = StubEbpfCollector::new();
        assert_eq!(collector.state(), CollectorState::Detached);

        // Attach.
        collector.attach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Attached);

        // Inject and read.
        collector.inject_events(vec![EbpfEvent {
            timestamp: Utc::now(),
            kind: "syscall".to_string(),
            payload: vec![42],
        }]);
        let events = collector.read_events().await.unwrap();
        assert_eq!(events.len(), 1);

        // Detach.
        collector.detach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Detached);
    }

    #[tokio::test]
    async fn reattach_after_detach() {
        let mut collector = StubEbpfCollector::new();

        collector.attach().await.unwrap();
        collector.detach().await.unwrap();
        collector.attach().await.unwrap();

        assert_eq!(collector.state(), CollectorState::Attached);
    }
}

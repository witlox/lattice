//! Event bus for pub-sub streaming RPCs.
//!
//! Provides an [`EventBus`] backed by `tokio::sync::broadcast` channels.
//! Consumers subscribe to events for a specific allocation ID and receive
//! only the events that match.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

/// Capacity of each per-allocation broadcast channel.
const CHANNEL_CAPACITY: usize = 256;

/// Events that flow through the event bus.
#[derive(Debug, Clone, PartialEq)]
pub enum AllocationEvent {
    /// An allocation's state changed (e.g. Pending -> Running).
    StateChange {
        allocation_id: Uuid,
        old_state: String,
        new_state: String,
    },
    /// A log line emitted by an allocation's process.
    LogLine {
        allocation_id: Uuid,
        line: String,
        stream: LogStream,
    },
    /// A metric data point for an allocation.
    MetricPoint {
        allocation_id: Uuid,
        metric_name: String,
        value: f64,
        timestamp_epoch_ms: u64,
    },
}

/// Which output stream a log line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl AllocationEvent {
    /// Return the allocation ID this event belongs to.
    pub fn allocation_id(&self) -> &Uuid {
        match self {
            AllocationEvent::StateChange { allocation_id, .. } => allocation_id,
            AllocationEvent::LogLine { allocation_id, .. } => allocation_id,
            AllocationEvent::MetricPoint { allocation_id, .. } => allocation_id,
        }
    }
}

/// A pub-sub event bus for allocation lifecycle events.
///
/// Publishers call [`EventBus::publish`] to broadcast an event to all
/// subscribers watching the corresponding allocation. Subscribers call
/// [`EventBus::subscribe`] to receive a filtered stream.
pub struct EventBus {
    /// Per-allocation broadcast senders. A channel is lazily created on
    /// the first subscribe or publish for a given allocation ID.
    channels: RwLock<HashMap<Uuid, broadcast::Sender<AllocationEvent>>>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus").finish()
    }
}

impl EventBus {
    /// Create a new, empty event bus.
    pub fn new() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
        }
    }

    /// Subscribe to events for a specific allocation.
    ///
    /// Returns a [`broadcast::Receiver`] that yields only the events
    /// whose `allocation_id` matches `alloc_id`.
    pub async fn subscribe(&self, alloc_id: Uuid) -> broadcast::Receiver<AllocationEvent> {
        let mut channels = self.channels.write().await;
        let sender = channels
            .entry(alloc_id)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    /// Publish an event.
    ///
    /// The event is delivered to all subscribers of the event's allocation
    /// ID. If there are no subscribers, the event is silently dropped.
    /// Returns the number of receivers that received the event.
    pub async fn publish(&self, event: AllocationEvent) -> usize {
        let alloc_id = *event.allocation_id();
        let channels = self.channels.read().await;
        if let Some(sender) = channels.get(&alloc_id) {
            // send returns Err if there are no active receivers — that is fine.
            sender.send(event).unwrap_or(0)
        } else {
            0
        }
    }

    /// Remove the channel for an allocation, cleaning up resources.
    pub async fn remove(&self, alloc_id: &Uuid) {
        self.channels.write().await.remove(alloc_id);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper: create a shared `EventBus` wrapped in `Arc`.
pub fn new_event_bus() -> Arc<EventBus> {
    Arc::new(EventBus::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn publish_and_receive_single_subscriber() {
        let bus = EventBus::new();
        let alloc_id = Uuid::new_v4();

        let mut rx = bus.subscribe(alloc_id).await;

        let event = AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "running".to_string(),
        };

        let count = bus.publish(event.clone()).await;
        assert_eq!(count, 1);

        let received = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");
        assert_eq!(received, event);
    }

    #[tokio::test]
    async fn publish_to_correct_allocation_only() {
        let bus = EventBus::new();
        let alloc_a = Uuid::new_v4();
        let alloc_b = Uuid::new_v4();

        let mut rx_a = bus.subscribe(alloc_a).await;
        let mut rx_b = bus.subscribe(alloc_b).await;

        let event_a = AllocationEvent::LogLine {
            allocation_id: alloc_a,
            line: "hello from A".to_string(),
            stream: LogStream::Stdout,
        };
        let event_b = AllocationEvent::MetricPoint {
            allocation_id: alloc_b,
            metric_name: "gpu_util".to_string(),
            value: 0.95,
            timestamp_epoch_ms: 1000,
        };

        bus.publish(event_a.clone()).await;
        bus.publish(event_b.clone()).await;

        // rx_a should only get event_a
        let received_a = timeout(Duration::from_millis(100), rx_a.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");
        assert_eq!(received_a, event_a);

        // rx_a should NOT get event_b — try_recv should fail
        assert!(rx_a.try_recv().is_err());

        // rx_b should only get event_b
        let received_b = timeout(Duration::from_millis(100), rx_b.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");
        assert_eq!(received_b, event_b);

        // rx_b should NOT get event_a
        assert!(rx_b.try_recv().is_err());
    }

    #[tokio::test]
    async fn multiple_subscribers_same_allocation() {
        let bus = EventBus::new();
        let alloc_id = Uuid::new_v4();

        let mut rx1 = bus.subscribe(alloc_id).await;
        let mut rx2 = bus.subscribe(alloc_id).await;

        let event = AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "running".to_string(),
            new_state: "completed".to_string(),
        };

        let count = bus.publish(event.clone()).await;
        assert_eq!(count, 2);

        let r1 = timeout(Duration::from_millis(100), rx1.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");
        let r2 = timeout(Duration::from_millis(100), rx2.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");

        assert_eq!(r1, event);
        assert_eq!(r2, event);
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_silent() {
        let bus = EventBus::new();
        let alloc_id = Uuid::new_v4();

        let event = AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "running".to_string(),
        };

        // No subscribers — should not panic, count should be 0
        let count = bus.publish(event).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn remove_cleans_up_channel() {
        let bus = EventBus::new();
        let alloc_id = Uuid::new_v4();

        let _rx = bus.subscribe(alloc_id).await;
        bus.remove(&alloc_id).await;

        // Publishing after removal should return 0
        let event = AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "cancelled".to_string(),
        };
        let count = bus.publish(event).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn default_creates_empty_bus() {
        let bus = EventBus::default();
        let alloc_id = Uuid::new_v4();

        // Should work the same as new()
        let event = AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "running".to_string(),
        };
        let count = bus.publish(event).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn new_event_bus_helper() {
        let bus = new_event_bus();
        let alloc_id = Uuid::new_v4();

        let mut rx = bus.subscribe(alloc_id).await;

        let event = AllocationEvent::LogLine {
            allocation_id: alloc_id,
            line: "test output".to_string(),
            stream: LogStream::Stderr,
        };

        bus.publish(event.clone()).await;

        let received = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");
        assert_eq!(received, event);
    }

    #[tokio::test]
    async fn allocation_event_id_accessor() {
        let id = Uuid::new_v4();

        let state_change = AllocationEvent::StateChange {
            allocation_id: id,
            old_state: "pending".to_string(),
            new_state: "running".to_string(),
        };
        assert_eq!(*state_change.allocation_id(), id);

        let log_line = AllocationEvent::LogLine {
            allocation_id: id,
            line: "hello".to_string(),
            stream: LogStream::Stdout,
        };
        assert_eq!(*log_line.allocation_id(), id);

        let metric = AllocationEvent::MetricPoint {
            allocation_id: id,
            metric_name: "cpu".to_string(),
            value: 1.0,
            timestamp_epoch_ms: 0,
        };
        assert_eq!(*metric.allocation_id(), id);
    }
}

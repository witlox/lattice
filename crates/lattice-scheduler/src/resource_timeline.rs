//! Resource timeline for reservation-based backfill scheduling.
//!
//! Tracks when running allocations will release nodes (based on walltime),
//! enabling the scheduler to make reservations for large jobs and backfill
//! smaller jobs that will complete before the reservation starts.

use chrono::{DateTime, Utc};
use lattice_common::types::*;

/// A future node-release event.
#[derive(Debug, Clone)]
pub struct ReleaseEvent {
    /// When the allocation's walltime expires.
    pub release_at: DateTime<Utc>,
    /// Which allocation releases.
    pub allocation_id: AllocId,
    /// Which nodes become free.
    pub nodes: Vec<NodeId>,
}

/// Configuration for the resource timeline look-ahead window.
#[derive(Debug, Clone)]
pub struct TimelineConfig {
    /// How far into the future to consider release events.
    pub look_ahead: chrono::Duration,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            look_ahead: chrono::Duration::hours(24),
        }
    }
}

/// A timeline of future node releases, built from running allocations.
#[derive(Debug, Clone)]
pub struct ResourceTimeline {
    /// Release events sorted chronologically.
    pub events: Vec<ReleaseEvent>,
}

impl ResourceTimeline {
    /// Build a timeline from running allocations and the current time.
    ///
    /// Only bounded allocations with a known `started_at` and positive walltime
    /// produce release events. Unbounded and reactive allocations block nodes
    /// indefinitely (no events). Events beyond `look_ahead` are excluded.
    pub fn build(running: &[Allocation], now: DateTime<Utc>, look_ahead: chrono::Duration) -> Self {
        let horizon = now + look_ahead;
        let mut events = Vec::new();

        for alloc in running {
            let started_at = match alloc.started_at {
                Some(t) => t,
                None => continue,
            };

            let walltime = match &alloc.lifecycle.lifecycle_type {
                LifecycleType::Bounded { walltime } => *walltime,
                LifecycleType::Unbounded | LifecycleType::Reactive { .. } => continue,
            };

            if walltime.num_seconds() <= 0 {
                continue;
            }

            let release_at = started_at + walltime;

            // Skip events in the past or beyond the look-ahead horizon.
            if release_at <= now || release_at > horizon {
                continue;
            }

            if alloc.assigned_nodes.is_empty() {
                continue;
            }

            events.push(ReleaseEvent {
                release_at,
                allocation_id: alloc.id,
                nodes: alloc.assigned_nodes.clone(),
            });
        }

        events.sort_by_key(|e| e.release_at);

        ResourceTimeline { events }
    }

    /// Find the earliest time at which `needed` nodes become available.
    ///
    /// Walks release events chronologically, accumulating freed nodes
    /// (starting from `free_now` already-free nodes). Only considers nodes
    /// that pass `filter_fn`. Returns `None` if not achievable within the
    /// timeline window.
    pub fn earliest_start<F>(
        &self,
        needed: u32,
        free_now: u32,
        filter_fn: F,
    ) -> Option<DateTime<Utc>>
    where
        F: Fn(&NodeId) -> bool,
    {
        if free_now >= needed {
            return None; // Already have enough — no reservation needed.
        }

        let mut accumulated = free_now;

        for event in &self.events {
            let matching = event.nodes.iter().filter(|n| filter_fn(n)).count() as u32;
            accumulated += matching;

            if accumulated >= needed {
                return Some(event.release_at);
            }
        }

        None // Not achievable within the look-ahead window.
    }

    /// Check whether a backfill job can complete before the reservation time.
    pub fn is_backfill_safe(
        now: DateTime<Utc>,
        walltime: chrono::Duration,
        reservation_time: DateTime<Utc>,
    ) -> bool {
        now + walltime <= reservation_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    fn make_running(nodes: Vec<&str>, walltime_hours: i64, started_hours_ago: i64) -> Allocation {
        let now = Utc::now();
        let mut alloc = AllocationBuilder::new()
            .nodes(nodes.len() as u32)
            .lifecycle_bounded(walltime_hours as u64)
            .state(AllocationState::Running)
            .build();
        alloc.started_at = Some(now - chrono::Duration::hours(started_hours_ago));
        alloc.assigned_nodes = nodes.into_iter().map(String::from).collect();
        alloc
    }

    // ─── build() tests ───────────────────────────────────────────

    #[test]
    fn empty_running_produces_no_events() {
        let now = Utc::now();
        let timeline = ResourceTimeline::build(&[], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn bounded_allocation_produces_event() {
        let now = Utc::now();
        let alloc = make_running(vec!["n1", "n2"], 4, 1); // 4h wall, started 1h ago → 3h left
        let timeline = ResourceTimeline::build(
            std::slice::from_ref(&alloc),
            now,
            chrono::Duration::hours(24),
        );
        assert_eq!(timeline.events.len(), 1);
        assert_eq!(timeline.events[0].allocation_id, alloc.id);
        assert_eq!(timeline.events[0].nodes, vec!["n1", "n2"]);
    }

    #[test]
    fn unbounded_allocation_ignored() {
        let now = Utc::now();
        let mut alloc = AllocationBuilder::new()
            .nodes(2)
            .lifecycle_unbounded()
            .state(AllocationState::Running)
            .build();
        alloc.started_at = Some(now - chrono::Duration::hours(1));
        alloc.assigned_nodes = vec!["n1".into(), "n2".into()];

        let timeline = ResourceTimeline::build(&[alloc], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn mixed_lifecycle_only_bounded() {
        let now = Utc::now();
        let bounded = make_running(vec!["n1"], 2, 1);
        let mut unbounded = AllocationBuilder::new()
            .lifecycle_unbounded()
            .state(AllocationState::Running)
            .build();
        unbounded.started_at = Some(now - chrono::Duration::hours(1));
        unbounded.assigned_nodes = vec!["n2".into()];

        let timeline =
            ResourceTimeline::build(&[bounded, unbounded], now, chrono::Duration::hours(24));
        assert_eq!(timeline.events.len(), 1);
    }

    #[test]
    fn no_started_at_skipped() {
        let now = Utc::now();
        let mut alloc = AllocationBuilder::new()
            .lifecycle_bounded(4)
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = vec!["n1".into()];
        // started_at is None by default

        let timeline = ResourceTimeline::build(&[alloc], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn zero_walltime_skipped() {
        let now = Utc::now();
        let mut alloc = AllocationBuilder::new()
            .lifecycle_bounded(0)
            .state(AllocationState::Running)
            .build();
        alloc.started_at = Some(now - chrono::Duration::hours(1));
        alloc.assigned_nodes = vec!["n1".into()];

        let timeline = ResourceTimeline::build(&[alloc], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn past_deadline_skipped() {
        let now = Utc::now();
        // Started 5h ago, 2h walltime → released 3h ago (past)
        let alloc = make_running(vec!["n1"], 2, 5);
        let timeline = ResourceTimeline::build(&[alloc], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn beyond_look_ahead_skipped() {
        let now = Utc::now();
        // Started 1h ago, 48h walltime → releases 47h from now, beyond 24h window
        let alloc = make_running(vec!["n1"], 48, 1);
        let timeline = ResourceTimeline::build(&[alloc], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn events_sorted_chronologically() {
        let now = Utc::now();
        // a1: 1h left, a2: 3h left, a3: 2h left
        let a1 = make_running(vec!["n1"], 2, 1);
        let a2 = make_running(vec!["n2"], 4, 1);
        let a3 = make_running(vec!["n3"], 3, 1);

        let timeline = ResourceTimeline::build(&[a1, a2, a3], now, chrono::Duration::hours(24));
        assert_eq!(timeline.events.len(), 3);
        assert!(timeline.events[0].release_at <= timeline.events[1].release_at);
        assert!(timeline.events[1].release_at <= timeline.events[2].release_at);
    }

    // ─── earliest_start() tests ──────────────────────────────────

    #[test]
    fn earliest_start_immediate_when_enough_free() {
        let timeline = ResourceTimeline { events: vec![] };
        // Already have 4 free, need 3 → no reservation needed
        assert!(timeline.earliest_start(3, 4, |_| true).is_none());
    }

    #[test]
    fn earliest_start_after_release() {
        let now = Utc::now();
        let release_time = now + chrono::Duration::hours(2);
        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: release_time,
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["n1".into(), "n2".into()],
            }],
        };
        // Need 3, have 1 free, 2 coming → 1+2=3 at release_time
        let result = timeline.earliest_start(3, 1, |_| true);
        assert_eq!(result, Some(release_time));
    }

    #[test]
    fn earliest_start_with_filter() {
        let now = Utc::now();
        let release_time = now + chrono::Duration::hours(2);
        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: release_time,
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["n1".into(), "n2".into()],
            }],
        };
        // Filter rejects n1, so only n2 passes → 0+1=1 < 2 needed
        let result = timeline.earliest_start(2, 0, |n| n != "n1");
        assert!(result.is_none());
    }

    #[test]
    fn earliest_start_not_achievable() {
        let now = Utc::now();
        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: now + chrono::Duration::hours(2),
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["n1".into()],
            }],
        };
        // Need 5, only 1 free + 1 coming = 2 < 5
        let result = timeline.earliest_start(5, 1, |_| true);
        assert!(result.is_none());
    }

    #[test]
    fn earliest_start_single_node() {
        let now = Utc::now();
        let release_time = now + chrono::Duration::hours(1);
        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: release_time,
                allocation_id: uuid::Uuid::new_v4(),
                nodes: vec!["n1".into()],
            }],
        };
        // Need 1, have 0, get 1 at release_time
        let result = timeline.earliest_start(1, 0, |_| true);
        assert_eq!(result, Some(release_time));
    }

    // ─── is_backfill_safe() tests ────────────────────────────────

    #[test]
    fn backfill_safe_when_fits() {
        let now = Utc::now();
        let reservation = now + chrono::Duration::hours(3);
        assert!(ResourceTimeline::is_backfill_safe(
            now,
            chrono::Duration::hours(2),
            reservation
        ));
    }

    #[test]
    fn backfill_safe_exact_boundary() {
        let now = Utc::now();
        let reservation = now + chrono::Duration::hours(2);
        assert!(ResourceTimeline::is_backfill_safe(
            now,
            chrono::Duration::hours(2),
            reservation
        ));
    }

    #[test]
    fn backfill_unsafe_when_too_long() {
        let now = Utc::now();
        let reservation = now + chrono::Duration::hours(2);
        assert!(!ResourceTimeline::is_backfill_safe(
            now,
            chrono::Duration::hours(3),
            reservation
        ));
    }
}

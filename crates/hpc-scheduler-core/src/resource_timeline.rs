//! Resource timeline for reservation-based backfill scheduling.
//!
//! Tracks when running jobs will release nodes (based on walltime),
//! enabling the scheduler to make reservations for large jobs and backfill
//! smaller jobs that will complete before the reservation starts.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::traits::Job;

/// A future node-release event.
#[derive(Debug, Clone)]
pub struct ReleaseEvent {
    /// When the job's walltime expires.
    pub release_at: DateTime<Utc>,
    /// Which job releases.
    pub allocation_id: Uuid,
    /// Which nodes become free.
    pub nodes: Vec<String>,
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

/// A timeline of future node releases, built from running jobs.
#[derive(Debug, Clone)]
pub struct ResourceTimeline {
    /// Release events sorted chronologically.
    pub events: Vec<ReleaseEvent>,
}

impl ResourceTimeline {
    /// Build a timeline from running jobs and the current time.
    ///
    /// Only bounded jobs with a known `started_at` and positive walltime
    /// produce release events. Unbounded and reactive jobs block nodes
    /// indefinitely (no events). Events beyond `look_ahead` are excluded.
    pub fn build<J: Job>(running: &[J], now: DateTime<Utc>, look_ahead: chrono::Duration) -> Self {
        let horizon = now + look_ahead;
        let mut events = Vec::new();

        for job in running {
            let started_at = match job.started_at() {
                Some(t) => t,
                None => continue,
            };

            let walltime = match job.walltime() {
                Some(w) if w.num_seconds() > 0 => w,
                _ => continue,
            };

            let release_at = started_at + walltime;

            // Skip events in the past or beyond the look-ahead horizon.
            if release_at <= now || release_at > horizon {
                continue;
            }

            let nodes = job.assigned_nodes();
            if nodes.is_empty() {
                continue;
            }

            events.push(ReleaseEvent {
                release_at,
                allocation_id: job.id(),
                nodes: nodes.to_vec(),
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
        F: Fn(&str) -> bool,
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

    // Minimal test job implementation
    struct TestJob {
        id: Uuid,
        started_at: Option<DateTime<Utc>>,
        walltime: Option<chrono::Duration>,
        assigned_nodes: Vec<String>,
    }

    impl TestJob {
        fn bounded(nodes: Vec<&str>, walltime_hours: i64, started_hours_ago: i64) -> Self {
            let now = Utc::now();
            Self {
                id: Uuid::new_v4(),
                started_at: Some(now - chrono::Duration::hours(started_hours_ago)),
                walltime: Some(chrono::Duration::hours(walltime_hours)),
                assigned_nodes: nodes.into_iter().map(String::from).collect(),
            }
        }

        fn unbounded(nodes: Vec<&str>) -> Self {
            let now = Utc::now();
            Self {
                id: Uuid::new_v4(),
                started_at: Some(now - chrono::Duration::hours(1)),
                walltime: None,
                assigned_nodes: nodes.into_iter().map(String::from).collect(),
            }
        }
    }

    impl Job for TestJob {
        fn id(&self) -> Uuid {
            self.id
        }
        fn tenant_id(&self) -> &str {
            "test"
        }
        fn node_count_min(&self) -> u32 {
            self.assigned_nodes.len() as u32
        }
        fn node_count_max(&self) -> Option<u32> {
            None
        }
        fn walltime(&self) -> Option<chrono::Duration> {
            self.walltime
        }
        fn preemption_class(&self) -> u8 {
            5
        }
        fn created_at(&self) -> DateTime<Utc> {
            Utc::now()
        }
        fn started_at(&self) -> Option<DateTime<Utc>> {
            self.started_at
        }
        fn assigned_nodes(&self) -> &[String] {
            &self.assigned_nodes
        }
        fn checkpoint_kind(&self) -> crate::types::CheckpointKind {
            crate::types::CheckpointKind::None
        }
        fn is_running(&self) -> bool {
            true
        }
        fn is_sensitive(&self) -> bool {
            false
        }
        fn prefer_same_numa(&self) -> bool {
            false
        }
        fn topology_preference(&self) -> Option<crate::types::TopologyPreference> {
            None
        }
        fn constraints(&self) -> crate::types::NodeConstraints {
            crate::types::NodeConstraints::default()
        }
    }

    #[test]
    fn empty_running_produces_no_events() {
        let now = Utc::now();
        let empty: Vec<TestJob> = vec![];
        let timeline = ResourceTimeline::build(&empty, now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn bounded_job_produces_event() {
        let now = Utc::now();
        let job = TestJob::bounded(vec!["n1", "n2"], 4, 1);
        let timeline = ResourceTimeline::build(&[job], now, chrono::Duration::hours(24));
        assert_eq!(timeline.events.len(), 1);
        assert_eq!(timeline.events[0].nodes, vec!["n1", "n2"]);
    }

    #[test]
    fn unbounded_job_ignored() {
        let now = Utc::now();
        let job = TestJob::unbounded(vec!["n1", "n2"]);
        let timeline = ResourceTimeline::build(&[job], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn past_deadline_skipped() {
        let now = Utc::now();
        let job = TestJob::bounded(vec!["n1"], 2, 5);
        let timeline = ResourceTimeline::build(&[job], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn beyond_look_ahead_skipped() {
        let now = Utc::now();
        let job = TestJob::bounded(vec!["n1"], 48, 1);
        let timeline = ResourceTimeline::build(&[job], now, chrono::Duration::hours(24));
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn events_sorted_chronologically() {
        let now = Utc::now();
        let a1 = TestJob::bounded(vec!["n1"], 2, 1);
        let a2 = TestJob::bounded(vec!["n2"], 4, 1);
        let a3 = TestJob::bounded(vec!["n3"], 3, 1);

        let timeline = ResourceTimeline::build(&[a1, a2, a3], now, chrono::Duration::hours(24));
        assert_eq!(timeline.events.len(), 3);
        assert!(timeline.events[0].release_at <= timeline.events[1].release_at);
        assert!(timeline.events[1].release_at <= timeline.events[2].release_at);
    }

    #[test]
    fn earliest_start_immediate_when_enough_free() {
        let timeline = ResourceTimeline { events: vec![] };
        assert!(timeline.earliest_start(3, 4, |_| true).is_none());
    }

    #[test]
    fn earliest_start_after_release() {
        let now = Utc::now();
        let release_time = now + chrono::Duration::hours(2);
        let timeline = ResourceTimeline {
            events: vec![ReleaseEvent {
                release_at: release_time,
                allocation_id: Uuid::new_v4(),
                nodes: vec!["n1".into(), "n2".into()],
            }],
        };
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
                allocation_id: Uuid::new_v4(),
                nodes: vec!["n1".into(), "n2".into()],
            }],
        };
        let result = timeline.earliest_start(2, 0, |n| n != "n1");
        assert!(result.is_none());
    }

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

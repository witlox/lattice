//! Walltime enforcement for bounded jobs.
//!
//! Tracks per-job walltime timers and enforces termination via a
//! two-phase protocol: SIGTERM on expiry, then SIGKILL after a grace period.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

/// Default grace period between SIGTERM and SIGKILL (30 seconds).
const DEFAULT_GRACE_SECONDS: i64 = 30;

/// Phase of walltime expiry enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryPhase {
    /// Walltime exceeded; job should receive SIGTERM.
    Terminate,
    /// Grace period after SIGTERM has elapsed; job should receive SIGKILL.
    Kill,
}

/// Record of a walltime expiry event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalltimeExpiry {
    /// The job that exceeded its walltime.
    pub allocation_id: Uuid,
    /// Which enforcement phase this expiry is in.
    pub phase: ExpiryPhase,
    /// When the walltime limit was reached.
    pub expired_at: DateTime<Utc>,
}

/// Internal tracking state for a single job's walltime.
#[derive(Debug, Clone)]
struct WalltimeEntry {
    start_time: DateTime<Utc>,
    walltime: Duration,
    term_sent_at: Option<DateTime<Utc>>,
}

/// Enforces walltime limits for bounded jobs.
///
/// The enforcer tracks registered jobs and, on each `check_expired` call,
/// returns a list of jobs that need enforcement action:
///
/// 1. **Terminate phase**: Walltime exceeded but grace period not yet elapsed.
/// 2. **Kill phase**: Grace period after SIGTERM has elapsed.
#[derive(Debug)]
pub struct WalltimeEnforcer {
    entries: HashMap<Uuid, WalltimeEntry>,
    grace_period: Duration,
}

impl WalltimeEnforcer {
    /// Create a new enforcer with the default 30-second grace period.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            grace_period: Duration::seconds(DEFAULT_GRACE_SECONDS),
        }
    }

    /// Create a new enforcer with a custom grace period.
    pub fn with_grace_period(grace_period: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            grace_period,
        }
    }

    /// Register a job for walltime tracking.
    pub fn register(&mut self, id: Uuid, walltime: Duration, start_time: DateTime<Utc>) {
        self.entries.insert(
            id,
            WalltimeEntry {
                start_time,
                walltime,
                term_sent_at: None,
            },
        );
    }

    /// Unregister a job (e.g., it completed or was cancelled).
    pub fn unregister(&mut self, id: &Uuid) -> bool {
        self.entries.remove(id).is_some()
    }

    /// Check for expired jobs at the given time.
    pub fn check_expired(&mut self, now: DateTime<Utc>) -> Vec<WalltimeExpiry> {
        let grace = self.grace_period;
        let mut expired = Vec::new();

        for (id, entry) in self.entries.iter_mut() {
            let deadline = entry.start_time + entry.walltime;

            if now < deadline {
                continue;
            }

            match entry.term_sent_at {
                None => {
                    entry.term_sent_at = Some(now);
                    expired.push(WalltimeExpiry {
                        allocation_id: *id,
                        phase: ExpiryPhase::Terminate,
                        expired_at: deadline,
                    });
                }
                Some(term_time) => {
                    if now >= term_time + grace {
                        expired.push(WalltimeExpiry {
                            allocation_id: *id,
                            phase: ExpiryPhase::Kill,
                            expired_at: deadline,
                        });
                    } else {
                        expired.push(WalltimeExpiry {
                            allocation_id: *id,
                            phase: ExpiryPhase::Terminate,
                            expired_at: deadline,
                        });
                    }
                }
            }
        }

        expired.sort_by_key(|e| e.expired_at);
        expired
    }

    /// Number of tracked jobs.
    pub fn tracked_count(&self) -> usize {
        self.entries.len()
    }

    /// Check if a specific job is being tracked.
    pub fn is_tracked(&self, id: &Uuid) -> bool {
        self.entries.contains_key(id)
    }

    /// Get the remaining walltime for a job.
    pub fn remaining(&self, id: &Uuid, now: DateTime<Utc>) -> Option<Duration> {
        self.entries.get(id).and_then(|entry| {
            let deadline = entry.start_time + entry.walltime;
            let remaining = deadline - now;
            if remaining > Duration::zero() {
                Some(remaining)
            } else {
                None
            }
        })
    }
}

impl Default for WalltimeEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_tracks_job() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = Uuid::new_v4();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(1), now);
        assert!(enforcer.is_tracked(&id));
        assert_eq!(enforcer.tracked_count(), 1);
    }

    #[test]
    fn check_expired_returns_terminate_at_walltime() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = Uuid::new_v4();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        let expired = enforcer.check_expired(start + Duration::hours(1));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);
    }

    #[test]
    fn check_expired_returns_kill_after_grace_period() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = Uuid::new_v4();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        let term_time = start + Duration::hours(1);
        enforcer.check_expired(term_time);

        let expired = enforcer.check_expired(term_time + Duration::seconds(31));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Kill);
    }

    #[test]
    fn unregister_stops_tracking() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = Uuid::new_v4();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);
        enforcer.unregister(&id);

        let expired = enforcer.check_expired(start + Duration::hours(2));
        assert!(expired.is_empty());
    }
}

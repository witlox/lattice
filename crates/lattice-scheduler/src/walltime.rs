//! Walltime enforcement for bounded allocations.
//!
//! Tracks per-allocation walltime timers and enforces termination via a
//! two-phase protocol: SIGTERM on expiry, then SIGKILL after a grace period.
//! This ensures well-behaved applications get a chance to clean up before
//! being forcibly terminated.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use lattice_common::types::AllocId;

/// Default grace period between SIGTERM and SIGKILL (30 seconds).
const DEFAULT_GRACE_SECONDS: i64 = 30;

/// Phase of walltime expiry enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryPhase {
    /// Walltime exceeded; allocation should receive SIGTERM.
    Terminate,
    /// Grace period after SIGTERM has elapsed; allocation should receive SIGKILL.
    Kill,
}

/// Record of a walltime expiry event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalltimeExpiry {
    /// The allocation that exceeded its walltime.
    pub allocation_id: AllocId,
    /// Which enforcement phase this expiry is in.
    pub phase: ExpiryPhase,
    /// When the walltime limit was reached.
    pub expired_at: DateTime<Utc>,
}

/// Internal tracking state for a single allocation's walltime.
#[derive(Debug, Clone)]
struct WalltimeEntry {
    /// When the allocation started running.
    start_time: DateTime<Utc>,
    /// Maximum allowed wall-clock duration.
    walltime: Duration,
    /// When SIGTERM was sent (transitions to kill phase after grace period).
    term_sent_at: Option<DateTime<Utc>>,
}

/// Enforces walltime limits for bounded allocations.
///
/// The enforcer tracks registered allocations and, on each `check_expired` call,
/// returns a list of allocations that need enforcement action:
///
/// 1. **Terminate phase**: Walltime exceeded but grace period not yet elapsed.
///    The caller should send SIGTERM to the allocation's processes.
/// 2. **Kill phase**: Grace period after SIGTERM has elapsed.
///    The caller should send SIGKILL to forcibly terminate.
///
/// Once an allocation enters the Kill phase and is reported, subsequent calls
/// will continue reporting it in Kill phase until it is unregistered.
#[derive(Debug)]
pub struct WalltimeEnforcer {
    entries: HashMap<AllocId, WalltimeEntry>,
    /// Duration between SIGTERM and SIGKILL.
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

    /// Register an allocation for walltime tracking.
    ///
    /// - `alloc_id`: Unique identifier of the allocation.
    /// - `walltime`: Maximum allowed wall-clock duration.
    /// - `start_time`: When the allocation started running.
    pub fn register(&mut self, alloc_id: AllocId, walltime: Duration, start_time: DateTime<Utc>) {
        self.entries.insert(
            alloc_id,
            WalltimeEntry {
                start_time,
                walltime,
                term_sent_at: None,
            },
        );
    }

    /// Unregister an allocation (e.g., it completed or was cancelled).
    pub fn unregister(&mut self, alloc_id: &AllocId) -> bool {
        self.entries.remove(alloc_id).is_some()
    }

    /// Check for expired allocations at the given time.
    ///
    /// Returns a list of `WalltimeExpiry` records. The caller must act on each:
    /// - `Terminate`: Send SIGTERM to the allocation's processes.
    /// - `Kill`: Send SIGKILL to forcibly terminate.
    ///
    /// Internally updates state: the first time an allocation is reported as
    /// expired, it records the SIGTERM timestamp so subsequent checks can
    /// determine when the grace period elapses.
    pub fn check_expired(&mut self, now: DateTime<Utc>) -> Vec<WalltimeExpiry> {
        let grace = self.grace_period;
        let mut expired = Vec::new();

        for (alloc_id, entry) in self.entries.iter_mut() {
            let deadline = entry.start_time + entry.walltime;

            if now < deadline {
                // Not yet expired.
                continue;
            }

            match entry.term_sent_at {
                None => {
                    // First detection: enter Terminate phase.
                    entry.term_sent_at = Some(now);
                    expired.push(WalltimeExpiry {
                        allocation_id: *alloc_id,
                        phase: ExpiryPhase::Terminate,
                        expired_at: deadline,
                    });
                }
                Some(term_time) => {
                    if now >= term_time + grace {
                        // Grace period elapsed: enter Kill phase.
                        expired.push(WalltimeExpiry {
                            allocation_id: *alloc_id,
                            phase: ExpiryPhase::Kill,
                            expired_at: deadline,
                        });
                    } else {
                        // Still in grace period, re-report Terminate so the
                        // caller knows this allocation is pending termination.
                        expired.push(WalltimeExpiry {
                            allocation_id: *alloc_id,
                            phase: ExpiryPhase::Terminate,
                            expired_at: deadline,
                        });
                    }
                }
            }
        }

        // Sort by expired_at for deterministic output.
        expired.sort_by_key(|e| e.expired_at);
        expired
    }

    /// Number of tracked allocations.
    pub fn tracked_count(&self) -> usize {
        self.entries.len()
    }

    /// Check if a specific allocation is being tracked.
    pub fn is_tracked(&self, alloc_id: &AllocId) -> bool {
        self.entries.contains_key(alloc_id)
    }

    /// Get the remaining walltime for an allocation (returns None if not tracked
    /// or already expired).
    pub fn remaining(&self, alloc_id: &AllocId, now: DateTime<Utc>) -> Option<Duration> {
        self.entries.get(alloc_id).and_then(|entry| {
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
    use uuid::Uuid;

    // ─── Helper ────────────────────────────────────────────────

    fn alloc_id() -> AllocId {
        Uuid::new_v4()
    }

    // ─── Registration tests ────────────────────────────────────

    #[test]
    fn register_tracks_allocation() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(1), now);

        assert!(enforcer.is_tracked(&id));
        assert_eq!(enforcer.tracked_count(), 1);
    }

    #[test]
    fn register_multiple_allocations() {
        let mut enforcer = WalltimeEnforcer::new();
        let now = Utc::now();

        let ids: Vec<AllocId> = (0..5).map(|_| alloc_id()).collect();
        for (i, id) in ids.iter().enumerate() {
            enforcer.register(*id, Duration::hours(i as i64 + 1), now);
        }

        assert_eq!(enforcer.tracked_count(), 5);
        for id in &ids {
            assert!(enforcer.is_tracked(id));
        }
    }

    #[test]
    fn register_replaces_existing_entry() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(1), now);
        enforcer.register(id, Duration::hours(2), now);

        assert_eq!(enforcer.tracked_count(), 1);
        // Should use the new walltime (2 hours)
        let remaining = enforcer.remaining(&id, now).unwrap();
        assert!(remaining > Duration::hours(1));
    }

    // ─── Unregistration tests ──────────────────────────────────

    #[test]
    fn unregister_removes_allocation() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(1), now);
        let removed = enforcer.unregister(&id);

        assert!(removed);
        assert!(!enforcer.is_tracked(&id));
        assert_eq!(enforcer.tracked_count(), 0);
    }

    #[test]
    fn unregister_nonexistent_returns_false() {
        let mut enforcer = WalltimeEnforcer::new();
        assert!(!enforcer.unregister(&alloc_id()));
    }

    // ─── Remaining walltime tests ──────────────────────────────

    #[test]
    fn remaining_walltime_before_expiry() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(2), now);

        let remaining = enforcer
            .remaining(&id, now + Duration::minutes(30))
            .unwrap();
        assert!(remaining > Duration::minutes(89));
        assert!(remaining <= Duration::minutes(90));
    }

    #[test]
    fn remaining_returns_none_after_expiry() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(1), now);

        let remaining = enforcer.remaining(&id, now + Duration::hours(2));
        assert!(remaining.is_none());
    }

    #[test]
    fn remaining_returns_none_for_untracked() {
        let enforcer = WalltimeEnforcer::new();
        assert!(enforcer.remaining(&alloc_id(), Utc::now()).is_none());
    }

    // ─── Expiry detection: no expired ──────────────────────────

    #[test]
    fn check_expired_returns_empty_before_walltime() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::hours(1), now);

        let expired = enforcer.check_expired(now + Duration::minutes(30));
        assert!(expired.is_empty());
    }

    #[test]
    fn check_expired_returns_empty_with_no_allocations() {
        let mut enforcer = WalltimeEnforcer::new();
        let expired = enforcer.check_expired(Utc::now());
        assert!(expired.is_empty());
    }

    // ─── Expiry detection: SIGTERM phase ───────────────────────

    #[test]
    fn check_expired_returns_terminate_at_walltime() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        // Exactly at walltime boundary
        let expired = enforcer.check_expired(start + Duration::hours(1));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].allocation_id, id);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);
    }

    #[test]
    fn check_expired_returns_terminate_past_walltime() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        let expired = enforcer.check_expired(start + Duration::hours(1) + Duration::seconds(5));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);
    }

    #[test]
    fn check_expired_still_terminate_during_grace_period() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        // First check: triggers SIGTERM
        let term_time = start + Duration::hours(1);
        let expired = enforcer.check_expired(term_time);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);

        // Second check during grace period (15s later, grace is 30s)
        let expired = enforcer.check_expired(term_time + Duration::seconds(15));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);
    }

    // ─── Expiry detection: SIGKILL phase ───────────────────────

    #[test]
    fn check_expired_returns_kill_after_grace_period() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        // First check: triggers SIGTERM
        let term_time = start + Duration::hours(1);
        enforcer.check_expired(term_time);

        // Check after grace period (30s default)
        let expired = enforcer.check_expired(term_time + Duration::seconds(31));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].allocation_id, id);
        assert_eq!(expired[0].phase, ExpiryPhase::Kill);
    }

    #[test]
    fn check_expired_returns_kill_exactly_at_grace_boundary() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        let term_time = start + Duration::hours(1);
        enforcer.check_expired(term_time);

        // Exactly at grace boundary
        let expired = enforcer.check_expired(term_time + Duration::seconds(30));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Kill);
    }

    #[test]
    fn kill_phase_persists_on_subsequent_checks() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        let term_time = start + Duration::hours(1);
        enforcer.check_expired(term_time);

        // Well past grace period
        let expired = enforcer.check_expired(term_time + Duration::minutes(5));
        assert_eq!(expired[0].phase, ExpiryPhase::Kill);

        // Still Kill on subsequent check
        let expired = enforcer.check_expired(term_time + Duration::minutes(10));
        assert_eq!(expired[0].phase, ExpiryPhase::Kill);
    }

    // ─── Custom grace period ───────────────────────────────────

    #[test]
    fn custom_grace_period_respected() {
        let mut enforcer = WalltimeEnforcer::with_grace_period(Duration::seconds(60));
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        let term_time = start + Duration::hours(1);
        enforcer.check_expired(term_time);

        // 45s later: still in 60s grace period
        let expired = enforcer.check_expired(term_time + Duration::seconds(45));
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);

        // 60s later: grace period elapsed
        let expired = enforcer.check_expired(term_time + Duration::seconds(60));
        assert_eq!(expired[0].phase, ExpiryPhase::Kill);
    }

    // ─── Multiple allocations ──────────────────────────────────

    #[test]
    fn multiple_allocations_expire_independently() {
        let mut enforcer = WalltimeEnforcer::new();
        let id1 = alloc_id();
        let id2 = alloc_id();
        let start = Utc::now();

        enforcer.register(id1, Duration::hours(1), start);
        enforcer.register(id2, Duration::hours(2), start);

        // After 1 hour: only id1 expired
        let expired = enforcer.check_expired(start + Duration::hours(1));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].allocation_id, id1);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);

        // After 2 hours: both expired (id1 in Kill, id2 in Terminate)
        let expired = enforcer.check_expired(start + Duration::hours(2));
        assert_eq!(expired.len(), 2);

        let id1_expiry = expired.iter().find(|e| e.allocation_id == id1).unwrap();
        let id2_expiry = expired.iter().find(|e| e.allocation_id == id2).unwrap();
        assert_eq!(id1_expiry.phase, ExpiryPhase::Kill);
        assert_eq!(id2_expiry.phase, ExpiryPhase::Terminate);
    }

    #[test]
    fn different_start_times_handled_correctly() {
        let mut enforcer = WalltimeEnforcer::new();
        let id1 = alloc_id();
        let id2 = alloc_id();
        let base = Utc::now();

        // id1 started at base, 1hr walltime -> expires at base+1h
        enforcer.register(id1, Duration::hours(1), base);
        // id2 started 30min later, 1hr walltime -> expires at base+1h30m
        enforcer.register(id2, Duration::hours(1), base + Duration::minutes(30));

        // At base+1h: only id1 expired
        let expired = enforcer.check_expired(base + Duration::hours(1));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].allocation_id, id1);

        // At base+1h30m: both expired
        let expired = enforcer.check_expired(base + Duration::minutes(90));
        assert_eq!(expired.len(), 2);
    }

    // ─── Unregister stops tracking ─────────────────────────────

    #[test]
    fn unregister_before_expiry_prevents_reporting() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);
        enforcer.unregister(&id);

        let expired = enforcer.check_expired(start + Duration::hours(2));
        assert!(expired.is_empty());
    }

    #[test]
    fn unregister_after_term_stops_kill_phase() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(1), start);

        // Trigger SIGTERM
        let term_time = start + Duration::hours(1);
        let expired = enforcer.check_expired(term_time);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);

        // Unregister (e.g., process exited cleanly after SIGTERM)
        enforcer.unregister(&id);

        // Should not report Kill
        let expired = enforcer.check_expired(term_time + Duration::minutes(5));
        assert!(expired.is_empty());
    }

    // ─── Edge cases ────────────────────────────────────────────

    #[test]
    fn zero_walltime_expires_immediately() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::zero(), now);

        let expired = enforcer.check_expired(now);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);
    }

    #[test]
    fn very_short_walltime() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let now = Utc::now();

        enforcer.register(id, Duration::seconds(1), now);

        // Before expiry
        let expired = enforcer.check_expired(now);
        assert!(expired.is_empty());

        // After 1 second
        let expired = enforcer.check_expired(now + Duration::seconds(1));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].phase, ExpiryPhase::Terminate);
    }

    #[test]
    fn expired_at_reflects_walltime_deadline() {
        let mut enforcer = WalltimeEnforcer::new();
        let id = alloc_id();
        let start = Utc::now();

        enforcer.register(id, Duration::hours(2), start);

        let expired = enforcer.check_expired(start + Duration::hours(3));
        assert_eq!(expired[0].expired_at, start + Duration::hours(2));
    }

    // ─── Default trait ─────────────────────────────────────────

    #[test]
    fn default_enforcer_has_no_entries() {
        let enforcer = WalltimeEnforcer::default();
        assert_eq!(enforcer.tracked_count(), 0);
    }
}

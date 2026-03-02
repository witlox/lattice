//! Per-allocation checkpoint policy evaluation.
//!
//! Determines checkpoint frequency and behavior based on
//! allocation configuration, system state, and workload type.

use chrono::{DateTime, Utc};

use lattice_common::types::*;

/// Checkpoint policy for a running allocation.
#[derive(Debug, Clone)]
pub struct CheckpointPolicy {
    /// Minimum interval between checkpoints (seconds)
    pub min_interval_secs: u64,
    /// Maximum interval (checkpoint at least this often)
    pub max_interval_secs: u64,
    /// Timeout for checkpoint completion
    pub timeout_secs: u64,
    /// Maximum deferrals allowed (gRPC callback mode)
    pub max_deferrals: u32,
    /// Maximum deferral duration per request
    pub max_deferral_secs: u64,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            min_interval_secs: 300,   // 5 min
            max_interval_secs: 21600, // 6 hours
            timeout_secs: 600,        // 10 min
            max_deferrals: 3,
            max_deferral_secs: 300, // 5 min
        }
    }
}

/// Evaluate checkpoint policy for an allocation.
pub fn evaluate_policy(alloc: &Allocation) -> CheckpointPolicy {
    match alloc.checkpoint {
        CheckpointStrategy::Auto => {
            // Auto: scheduler-coordinated
            let base = CheckpointPolicy::default();
            // Medical: more conservative (less frequent, longer timeout)
            if is_medical(alloc) {
                CheckpointPolicy {
                    min_interval_secs: 600,
                    max_interval_secs: 3600,
                    timeout_secs: 900,
                    ..base
                }
            } else {
                base
            }
        }
        CheckpointStrategy::Manual => {
            // Manual: application controls; we just set timeouts
            CheckpointPolicy {
                min_interval_secs: 0,        // no minimum
                max_interval_secs: u64::MAX, // no forced checkpoint
                timeout_secs: 600,
                max_deferrals: 5,
                max_deferral_secs: 300,
            }
        }
        CheckpointStrategy::None => {
            // No checkpoint: effectively infinite intervals
            CheckpointPolicy {
                min_interval_secs: u64::MAX,
                max_interval_secs: u64::MAX,
                timeout_secs: 0,
                max_deferrals: 0,
                max_deferral_secs: 0,
            }
        }
    }
}

/// Check if enough time has passed since the last checkpoint.
pub fn is_checkpoint_due(
    last_checkpoint: Option<DateTime<Utc>>,
    policy: &CheckpointPolicy,
    now: DateTime<Utc>,
) -> bool {
    match last_checkpoint {
        Some(last) => {
            let elapsed = (now - last).num_seconds().max(0) as u64;
            elapsed >= policy.max_interval_secs
        }
        None => true, // Never checkpointed: always due
    }
}

/// Check if the minimum interval has been respected.
pub fn can_checkpoint_now(
    last_checkpoint: Option<DateTime<Utc>>,
    policy: &CheckpointPolicy,
    now: DateTime<Utc>,
) -> bool {
    match last_checkpoint {
        Some(last) => {
            let elapsed = (now - last).num_seconds().max(0) as u64;
            elapsed >= policy.min_interval_secs
        }
        None => true,
    }
}

fn is_medical(alloc: &Allocation) -> bool {
    alloc
        .tags
        .get("workload_class")
        .is_some_and(|v| v == "medical")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    #[test]
    fn auto_policy_defaults() {
        let alloc = AllocationBuilder::new().build();
        let policy = evaluate_policy(&alloc);
        assert_eq!(policy.min_interval_secs, 300);
        assert_eq!(policy.max_interval_secs, 21600);
        assert_eq!(policy.timeout_secs, 600);
    }

    #[test]
    fn medical_policy_more_conservative() {
        let alloc = AllocationBuilder::new().medical().build();
        let policy = evaluate_policy(&alloc);
        assert!(policy.min_interval_secs > 300);
        assert!(policy.timeout_secs > 600);
    }

    #[test]
    fn manual_policy_no_forced_checkpoint() {
        let mut alloc = AllocationBuilder::new().build();
        alloc.checkpoint = CheckpointStrategy::Manual;
        let policy = evaluate_policy(&alloc);
        assert_eq!(policy.max_interval_secs, u64::MAX);
    }

    #[test]
    fn none_policy_disabled() {
        let mut alloc = AllocationBuilder::new().build();
        alloc.checkpoint = CheckpointStrategy::None;
        let policy = evaluate_policy(&alloc);
        assert_eq!(policy.min_interval_secs, u64::MAX);
        assert_eq!(policy.timeout_secs, 0);
    }

    #[test]
    fn checkpoint_due_when_never_checkpointed() {
        let policy = CheckpointPolicy::default();
        assert!(is_checkpoint_due(None, &policy, Utc::now()));
    }

    #[test]
    fn checkpoint_not_due_when_recent() {
        let policy = CheckpointPolicy::default();
        let last = Utc::now() - chrono::Duration::minutes(5);
        assert!(!is_checkpoint_due(Some(last), &policy, Utc::now()));
    }

    #[test]
    fn checkpoint_due_after_max_interval() {
        let policy = CheckpointPolicy {
            max_interval_secs: 3600,
            ..Default::default()
        };
        let last = Utc::now() - chrono::Duration::hours(2);
        assert!(is_checkpoint_due(Some(last), &policy, Utc::now()));
    }

    #[test]
    fn can_checkpoint_respects_min_interval() {
        let policy = CheckpointPolicy {
            min_interval_secs: 600,
            ..Default::default()
        };
        let last = Utc::now() - chrono::Duration::minutes(5);
        assert!(!can_checkpoint_now(Some(last), &policy, Utc::now()));

        let last_old = Utc::now() - chrono::Duration::minutes(15);
        assert!(can_checkpoint_now(Some(last_old), &policy, Utc::now()));
    }

    #[test]
    fn can_checkpoint_when_never_done() {
        let policy = CheckpointPolicy::default();
        assert!(can_checkpoint_now(None, &policy, Utc::now()));
    }
}

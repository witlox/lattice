//! Preemption candidate selection.
//!
//! Implements victim selection: find the cheapest set of running jobs
//! to preempt in order to free enough nodes for a higher-priority job.

use std::collections::HashMap;

use crate::cost::TenantUsage;
use crate::traits::Job;
use crate::types::CheckpointKind;

/// Preemption cost for a single job (lower = cheaper to preempt).
#[derive(Debug, Clone)]
pub struct PreemptionCandidate {
    pub allocation_id: uuid::Uuid,
    pub cost: f64,
    pub nodes: Vec<String>,
}

/// Configuration for preemption decisions.
#[derive(Debug, Clone)]
pub struct PreemptionConfig {
    /// Maximum number of victims per preemption decision.
    pub max_victims: usize,
    /// Threshold for "near completion" (fraction of walltime used).
    pub near_completion_threshold: f64,
    /// Per-tenant usage info for burst-aware preemption.
    pub tenant_usage: HashMap<String, TenantUsage>,
}

impl Default for PreemptionConfig {
    fn default() -> Self {
        Self {
            max_victims: 3,
            near_completion_threshold: 0.9,
            tenant_usage: HashMap::new(),
        }
    }
}

/// Result of a preemption evaluation.
#[derive(Debug, Clone)]
pub enum PreemptionResult {
    /// Preemption is possible with the given victim set.
    Possible {
        victims: Vec<PreemptionCandidate>,
        freed_nodes: Vec<String>,
    },
    /// Preemption is not possible.
    NotPossible { reason: String },
}

/// Evaluate preemption candidates for a pending job.
///
/// Returns the cheapest set of victims that frees enough nodes,
/// or `NotPossible` if no valid victim set exists.
pub fn evaluate_preemption<J: Job>(
    pending: &J,
    running: &[J],
    config: &PreemptionConfig,
) -> PreemptionResult {
    let pending_class = pending.preemption_class();
    let requested = pending.node_count_min() as usize;

    let mut candidates: Vec<PreemptionCandidate> = running
        .iter()
        .filter(|a| a.preemption_class() < pending_class && a.is_running() && !a.is_sensitive())
        .map(|a| PreemptionCandidate {
            allocation_id: a.id(),
            cost: preemption_cost(a, config),
            nodes: a.assigned_nodes().to_vec(),
        })
        .collect();

    candidates.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut victims = Vec::new();
    let mut freed_nodes = Vec::new();

    for candidate in candidates {
        if freed_nodes.len() >= requested {
            break;
        }
        if victims.len() >= config.max_victims {
            break;
        }
        freed_nodes.extend(candidate.nodes.clone());
        victims.push(candidate);
    }

    if freed_nodes.len() >= requested {
        PreemptionResult::Possible {
            victims,
            freed_nodes,
        }
    } else {
        PreemptionResult::NotPossible {
            reason: format!(
                "need {} nodes, can free {} via preemption",
                requested,
                freed_nodes.len()
            ),
        }
    }
}

fn preemption_cost<J: Job>(job: &J, config: &PreemptionConfig) -> f64 {
    let checkpoint_cost = match job.checkpoint_kind() {
        CheckpointKind::Auto => 5.0,
        CheckpointKind::Manual => 10.0,
        CheckpointKind::None => {
            let nodes = job.assigned_nodes().len() as f64;
            let elapsed = job
                .started_at()
                .map(|s| (chrono::Utc::now() - s).num_minutes() as f64)
                .unwrap_or(0.0);
            nodes * elapsed
        }
    };

    let remaining_value = remaining_walltime_value(job, config);
    let base_cost = checkpoint_cost + remaining_value;

    let burst_factor = match config.tenant_usage.get(job.tenant_id()) {
        Some(usage)
            if usage.burst_allowance.is_some() && usage.actual_usage > usage.target_share =>
        {
            0.5
        }
        _ => 1.0,
    };

    base_cost * burst_factor
}

fn remaining_walltime_value<J: Job>(job: &J, config: &PreemptionConfig) -> f64 {
    let walltime_minutes = match job.walltime() {
        Some(w) => w.num_minutes() as f64,
        None => return 0.0,
    };

    let elapsed = job
        .started_at()
        .map(|s| (chrono::Utc::now() - s).num_minutes() as f64)
        .unwrap_or(0.0);

    if walltime_minutes > 0.0 {
        let fraction_used = elapsed / walltime_minutes;
        if fraction_used >= config.near_completion_threshold {
            1000.0
        } else if fraction_used < 0.1 {
            1.0
        } else {
            fraction_used * 100.0
        }
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodeConstraints;
    use chrono::{DateTime, Utc};

    struct TestJob {
        id: uuid::Uuid,
        tenant: String,
        nodes_min: u32,
        preemption_class: u8,
        running: bool,
        sensitive: bool,
        assigned_nodes: Vec<String>,
        checkpoint: CheckpointKind,
        started_at: Option<DateTime<Utc>>,
        walltime: Option<chrono::Duration>,
    }

    impl Default for TestJob {
        fn default() -> Self {
            Self {
                id: uuid::Uuid::new_v4(),
                tenant: "t1".into(),
                nodes_min: 2,
                preemption_class: 5,
                running: false,
                sensitive: false,
                assigned_nodes: vec![],
                checkpoint: CheckpointKind::Auto,
                started_at: None,
                walltime: Some(chrono::Duration::hours(1)),
            }
        }
    }

    impl Job for TestJob {
        fn id(&self) -> uuid::Uuid {
            self.id
        }
        fn tenant_id(&self) -> &str {
            &self.tenant
        }
        fn node_count_min(&self) -> u32 {
            self.nodes_min
        }
        fn node_count_max(&self) -> Option<u32> {
            None
        }
        fn walltime(&self) -> Option<chrono::Duration> {
            self.walltime
        }
        fn preemption_class(&self) -> u8 {
            self.preemption_class
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
        fn checkpoint_kind(&self) -> CheckpointKind {
            self.checkpoint
        }
        fn is_running(&self) -> bool {
            self.running
        }
        fn is_sensitive(&self) -> bool {
            self.sensitive
        }
        fn prefer_same_numa(&self) -> bool {
            false
        }
        fn topology_preference(&self) -> Option<crate::types::TopologyPreference> {
            None
        }
        fn constraints(&self) -> NodeConstraints {
            NodeConstraints::default()
        }
    }

    #[test]
    fn no_candidates_returns_not_possible() {
        let pending = TestJob {
            preemption_class: 5,
            ..Default::default()
        };
        let result = evaluate_preemption(&pending, &[], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn lower_class_can_be_preempted() {
        let pending = TestJob {
            preemption_class: 5,
            nodes_min: 2,
            ..Default::default()
        };
        let victim = TestJob {
            preemption_class: 2,
            running: true,
            assigned_nodes: vec!["n1".into(), "n2".into()],
            ..Default::default()
        };
        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::Possible { .. }));
    }

    #[test]
    fn same_class_cannot_be_preempted() {
        let pending = TestJob {
            preemption_class: 5,
            nodes_min: 2,
            ..Default::default()
        };
        let victim = TestJob {
            preemption_class: 5,
            running: true,
            assigned_nodes: vec!["n1".into(), "n2".into()],
            ..Default::default()
        };
        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn sensitive_never_preempted() {
        let pending = TestJob {
            preemption_class: 9,
            nodes_min: 2,
            ..Default::default()
        };
        let victim = TestJob {
            preemption_class: 5,
            running: true,
            sensitive: true,
            assigned_nodes: vec!["n1".into(), "n2".into()],
            ..Default::default()
        };
        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }
}

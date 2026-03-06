//! Preemption candidate selection.
//!
//! Implements the victim selection algorithm from
//! docs/architecture/preemption.md.

use std::collections::HashMap;

use lattice_common::types::*;

use crate::cost::TenantUsage;

/// Preemption cost for a single allocation (lower = cheaper to preempt).
#[derive(Debug, Clone)]
pub struct PreemptionCandidate {
    pub allocation_id: AllocId,
    pub cost: f64,
    pub nodes: Vec<NodeId>,
}

/// Configuration for preemption decisions.
#[derive(Debug, Clone)]
pub struct PreemptionConfig {
    /// Maximum number of victims per preemption decision.
    pub max_victims: usize,
    /// Threshold for "near completion" (fraction of walltime used).
    pub near_completion_threshold: f64,
    /// Per-tenant usage info for burst-aware preemption.
    /// Allocations from tenants above their base target share (in burst territory)
    /// are cheaper to preempt.
    pub tenant_usage: HashMap<TenantId, TenantUsage>,
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
        freed_nodes: Vec<NodeId>,
    },
    /// Preemption is not possible (not enough resources even with preemption).
    NotPossible { reason: String },
}

/// Evaluate preemption candidates for a pending allocation.
///
/// Returns the cheapest set of victims that frees enough nodes,
/// or `NotPossible` if no valid victim set exists.
pub fn evaluate_preemption(
    pending: &Allocation,
    running: &[Allocation],
    config: &PreemptionConfig,
) -> PreemptionResult {
    let pending_class = pending.lifecycle.preemption_class;
    let requested = match pending.resources.nodes {
        NodeCount::Exact(n) => n as usize,
        NodeCount::Range { min, .. } => min as usize,
    };

    // Filter candidates: lower class, not sensitive, not already checkpointing
    let mut candidates: Vec<PreemptionCandidate> = running
        .iter()
        .filter(|a| {
            a.lifecycle.preemption_class < pending_class
                && a.state == AllocationState::Running
                && !is_sensitive(a)
        })
        .map(|a| PreemptionCandidate {
            allocation_id: a.id,
            cost: preemption_cost(a, config),
            nodes: a.assigned_nodes.clone(),
        })
        .collect();

    // Sort by cost ascending (cheapest to preempt first)
    candidates.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Greedy selection: pick cheapest until enough nodes freed
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

/// Compute the cost of preempting an allocation.
///
/// Burst-aware: if the tenant is above their base target share (using burst
/// capacity), preemption cost is halved — burst allocations should be the
/// first to give back resources when needed.
fn preemption_cost(alloc: &Allocation, config: &PreemptionConfig) -> f64 {
    let checkpoint_cost = match alloc.checkpoint {
        CheckpointStrategy::Auto => 5.0, // estimated checkpoint minutes
        CheckpointStrategy::Manual => 10.0,
        CheckpointStrategy::None => {
            // No checkpoint: cost = estimated recompute time (node-hours lost)
            let nodes = alloc.assigned_nodes.len() as f64;
            let elapsed = alloc
                .started_at
                .map(|s| (chrono::Utc::now() - s).num_minutes() as f64)
                .unwrap_or(0.0);
            nodes * elapsed
        }
    };

    let remaining_value = remaining_walltime_value(alloc, config);
    let base_cost = checkpoint_cost + remaining_value;

    // Apply burst discount: tenants above their base target are using burst capacity
    let burst_factor = match config.tenant_usage.get(&alloc.tenant) {
        Some(usage) if usage.burst_allowance.is_some() && usage.actual_usage > usage.target_share => {
            0.5 // Burst allocations are half as expensive to preempt
        }
        _ => 1.0,
    };

    base_cost * burst_factor
}

/// Higher cost if the allocation is near completion (let it finish).
fn remaining_walltime_value(alloc: &Allocation, config: &PreemptionConfig) -> f64 {
    let walltime_minutes = match &alloc.lifecycle.lifecycle_type {
        LifecycleType::Bounded { walltime } => walltime.num_minutes() as f64,
        _ => return 0.0, // unbounded/reactive: no walltime concept
    };

    let elapsed = alloc
        .started_at
        .map(|s| (chrono::Utc::now() - s).num_minutes() as f64)
        .unwrap_or(0.0);

    if walltime_minutes > 0.0 {
        let fraction_used = elapsed / walltime_minutes;
        if fraction_used >= config.near_completion_threshold {
            // Near completion: very high cost
            1000.0
        } else if fraction_used < 0.1 {
            // Just started: low remaining value
            1.0
        } else {
            // Proportional to progress
            fraction_used * 100.0
        }
    } else {
        0.0
    }
}

/// Check if an allocation is sensitive (never preempted).
fn is_sensitive(alloc: &Allocation) -> bool {
    alloc.lifecycle.preemption_class >= 10
        || alloc
            .tags
            .get("workload_class")
            .is_some_and(|v| v == "sensitive")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    #[test]
    fn no_candidates_returns_not_possible() {
        let pending = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(5)
            .build();

        let result = evaluate_preemption(&pending, &[], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn lower_class_can_be_preempted() {
        let pending = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(5)
            .build();

        let mut victim = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(2)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into(), "n2".into()];

        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::Possible { .. }));
    }

    #[test]
    fn same_class_cannot_be_preempted() {
        let pending = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(5)
            .build();

        let mut victim = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(5)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into(), "n2".into()];

        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn higher_class_cannot_be_preempted() {
        let pending = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(3)
            .build();

        let mut victim = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(7)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into(), "n2".into()];

        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn sensitive_never_preempted() {
        let pending = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(9)
            .build();

        let mut victim = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(5)
            .state(AllocationState::Running)
            .sensitive()
            .build();
        victim.assigned_nodes = vec!["n1".into(), "n2".into()];

        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn max_victims_respected() {
        let pending = AllocationBuilder::new()
            .nodes(4)
            .preemption_class(8)
            .build();

        let victims: Vec<Allocation> = (0..5)
            .map(|_| {
                let mut a = AllocationBuilder::new()
                    .nodes(1)
                    .preemption_class(1)
                    .state(AllocationState::Running)
                    .build();
                a.assigned_nodes = vec![format!("n{}", a.id)];
                a
            })
            .collect();

        let config = PreemptionConfig {
            max_victims: 2,
            ..Default::default()
        };
        let result = evaluate_preemption(&pending, &victims, &config);
        // max_victims = 2 → can only free 2 nodes, need 4
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn cheapest_victims_selected_first() {
        let pending = AllocationBuilder::new()
            .nodes(1)
            .preemption_class(8)
            .build();

        let mut v1 = AllocationBuilder::new()
            .nodes(1)
            .preemption_class(1)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        v1.assigned_nodes = vec!["n1".into()];
        v1.checkpoint = CheckpointStrategy::None; // expensive: recompute cost
        v1.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(1)); // running for 1hr

        let mut v2 = AllocationBuilder::new()
            .nodes(1)
            .preemption_class(1)
            .state(AllocationState::Running)
            .lifecycle_bounded(4)
            .build();
        v2.assigned_nodes = vec!["n2".into()];
        v2.checkpoint = CheckpointStrategy::Auto; // cheap: 5 min checkpoint
        v2.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));

        let result = evaluate_preemption(
            &pending,
            &[v1.clone(), v2.clone()],
            &PreemptionConfig::default(),
        );
        if let PreemptionResult::Possible { victims, .. } = result {
            // Should pick the cheaper one (v2 with Auto checkpoint)
            assert_eq!(victims.len(), 1);
            assert_eq!(victims[0].allocation_id, v2.id);
        } else {
            panic!("Expected Possible");
        }
    }

    #[test]
    fn pending_allocations_not_preemptable() {
        let pending = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(5)
            .build();

        let mut victim = AllocationBuilder::new()
            .nodes(2)
            .preemption_class(1)
            .state(AllocationState::Pending) // Not running
            .build();
        victim.assigned_nodes = vec!["n1".into(), "n2".into()];

        let result = evaluate_preemption(&pending, &[victim], &PreemptionConfig::default());
        assert!(matches!(result, PreemptionResult::NotPossible { .. }));
    }

    #[test]
    fn burst_tenant_preempted_before_non_burst() {
        let pending = AllocationBuilder::new()
            .nodes(1)
            .preemption_class(8)
            .build();

        // v1: non-burst tenant (normal cost)
        let mut v1 = AllocationBuilder::new()
            .nodes(1)
            .preemption_class(1)
            .tenant("normal")
            .state(AllocationState::Running)
            .build();
        v1.assigned_nodes = vec!["n1".into()];
        v1.checkpoint = CheckpointStrategy::Auto;

        // v2: burst tenant above target share (cost halved)
        let mut v2 = AllocationBuilder::new()
            .nodes(1)
            .preemption_class(1)
            .tenant("burst")
            .state(AllocationState::Running)
            .build();
        v2.assigned_nodes = vec!["n2".into()];
        v2.checkpoint = CheckpointStrategy::Auto;

        let mut tenant_usage = HashMap::new();
        tenant_usage.insert(
            "burst".to_string(),
            TenantUsage {
                target_share: 0.2,
                actual_usage: 0.3, // above target → using burst capacity
                burst_allowance: Some(0.5),
                system_utilization: 0.5,
            },
        );
        tenant_usage.insert(
            "normal".to_string(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.2, // below target → not bursting
                burst_allowance: None,
                system_utilization: 0.5,
            },
        );

        let config = PreemptionConfig {
            tenant_usage,
            ..Default::default()
        };

        let result = evaluate_preemption(&pending, &[v1.clone(), v2.clone()], &config);
        if let PreemptionResult::Possible { victims, .. } = result {
            assert_eq!(victims.len(), 1);
            // Burst tenant should be the victim (cheaper to preempt)
            assert_eq!(victims[0].allocation_id, v2.id);
        } else {
            panic!("Expected Possible");
        }
    }
}

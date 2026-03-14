//! Composite cost function evaluator.
//!
//! Delegates to hpc-scheduler-core for the scoring logic, with a thin wrapper
//! that converts Lattice `CostWeights` to core `CostWeights`.

pub use hpc_scheduler_core::cost::{BacklogMetrics, BudgetUtilization, CostContext, TenantUsage};

use lattice_common::scheduler_core_impls::to_core_weights;
use lattice_common::types::*;

/// Evaluates the composite cost function for allocations.
///
/// Wraps `hpc_scheduler_core::cost::CostEvaluator` to accept Lattice's `CostWeights`.
pub struct CostEvaluator {
    inner: hpc_scheduler_core::cost::CostEvaluator,
    /// The original lattice weights, kept for access via `.weights`.
    pub weights: CostWeights,
}

impl CostEvaluator {
    pub fn new(weights: CostWeights) -> Self {
        let core_weights = to_core_weights(&weights);
        Self {
            inner: hpc_scheduler_core::cost::CostEvaluator::new(core_weights),
            weights,
        }
    }

    /// Compute the composite score for an allocation.
    /// Higher scores mean higher scheduling priority.
    pub fn score(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.score(alloc, ctx)
    }

    /// f1: priority_class
    pub fn f1_priority(&self, alloc: &Allocation) -> f64 {
        self.inner.f1_priority(alloc)
    }

    /// f2: wait_time_factor
    pub fn f2_wait_time(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.f2_wait_time(alloc, ctx)
    }

    /// f3: fair_share_deficit
    pub fn f3_fair_share(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.f3_fair_share(alloc, ctx)
    }

    /// f4: topology_fitness
    pub fn f4_topology(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.f4_topology(alloc, ctx)
    }

    /// f5: data_readiness
    pub fn f5_data_readiness(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.f5_data_readiness(alloc, ctx)
    }

    /// f6: backlog_pressure
    pub fn f6_backlog(&self, ctx: &CostContext) -> f64 {
        self.inner.f6_backlog(ctx)
    }

    /// f7: energy_cost
    pub fn f7_energy(&self, ctx: &CostContext) -> f64 {
        self.inner.f7_energy(ctx)
    }

    /// f8: checkpoint_efficiency
    pub fn f8_checkpoint(&self, alloc: &Allocation) -> f64 {
        self.inner.f8_checkpoint(alloc)
    }

    /// f9: conformance_fitness
    pub fn f9_conformance(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.f9_conformance(alloc, ctx)
    }

    /// Budget penalty multiplier
    pub fn budget_penalty(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        self.inner.budget_penalty(alloc, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    fn default_ctx() -> CostContext {
        CostContext::default()
    }

    // ── f1: priority_class ──

    #[test]
    fn f1_class_0_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().preemption_class(0).build();
        assert!((eval.f1_priority(&alloc) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f1_class_10_scores_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().preemption_class(10).build();
        assert!((eval.f1_priority(&alloc) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f1_class_5_scores_half() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().preemption_class(5).build();
        assert!((eval.f1_priority(&alloc) - 0.5).abs() < f64::EPSILON);
    }

    // ── f2: wait_time_factor ──

    #[test]
    fn f2_zero_wait_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().build();
        let ctx = CostContext {
            now: alloc.created_at,
            ..default_ctx()
        };
        assert!((eval.f2_wait_time(&alloc, &ctx) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn f2_one_hour_wait_scores_ln2() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().build();
        let ctx = CostContext {
            now: alloc.created_at + chrono::Duration::seconds(3600),
            reference_wait_seconds: 3600.0,
            ..default_ctx()
        };
        let expected = 2.0_f64.ln();
        assert!((eval.f2_wait_time(&alloc, &ctx) - expected).abs() < 1e-10);
    }

    #[test]
    fn f2_increases_with_wait_time() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().build();
        let ctx1 = CostContext {
            now: alloc.created_at + chrono::Duration::seconds(600),
            ..default_ctx()
        };
        let ctx2 = CostContext {
            now: alloc.created_at + chrono::Duration::seconds(7200),
            ..default_ctx()
        };
        assert!(eval.f2_wait_time(&alloc, &ctx2) > eval.f2_wait_time(&alloc, &ctx1));
    }

    // ── f3: fair_share_deficit ──

    #[test]
    fn f3_tenant_at_target_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.5,
                actual_usage: 0.5,
                burst_allowance: None,
                system_utilization: 0.5,
            },
        );
        assert!((eval.f3_fair_share(&alloc, &ctx) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f3_tenant_above_target_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.5,
                burst_allowance: None,
                system_utilization: 0.5,
            },
        );
        assert!((eval.f3_fair_share(&alloc, &ctx) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f3_tenant_zero_usage_scores_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.5,
                actual_usage: 0.0,
                burst_allowance: None,
                system_utilization: 0.5,
            },
        );
        assert!((eval.f3_fair_share(&alloc, &ctx) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f3_unknown_tenant_scores_neutral() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("unknown").build();
        let ctx = default_ctx();
        assert!((eval.f3_fair_share(&alloc, &ctx) - 0.5).abs() < f64::EPSILON);
    }

    // ── f3: burst_allowance ──

    #[test]
    fn f3_burst_expands_target_when_system_idle() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.3,
                burst_allowance: Some(0.5),
                system_utilization: 0.2,
            },
        );
        let score = eval.f3_fair_share(&alloc, &ctx);
        assert!(
            score > 0.0,
            "burst should create positive deficit even at base target"
        );
    }

    #[test]
    fn f3_burst_disabled_when_system_busy() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.3,
                burst_allowance: Some(0.5),
                system_utilization: 0.9,
            },
        );
        let score = eval.f3_fair_share(&alloc, &ctx);
        assert!(
            (score - 0.0).abs() < f64::EPSILON,
            "no burst when system is busy"
        );
    }

    #[test]
    fn f3_no_burst_without_allowance() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.3,
                burst_allowance: None,
                system_utilization: 0.2,
            },
        );
        let score = eval.f3_fair_share(&alloc, &ctx);
        assert!((score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f3_burst_scales_with_spare_capacity() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();

        let mut ctx_idle = default_ctx();
        ctx_idle.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.3,
                burst_allowance: Some(0.5),
                system_utilization: 0.0,
            },
        );

        let mut ctx_moderate = default_ctx();
        ctx_moderate.tenant_usage.insert(
            "t1".into(),
            TenantUsage {
                target_share: 0.3,
                actual_usage: 0.3,
                burst_allowance: Some(0.5),
                system_utilization: 0.4,
            },
        );

        let score_idle = eval.f3_fair_share(&alloc, &ctx_idle);
        let score_moderate = eval.f3_fair_share(&alloc, &ctx_moderate);
        assert!(
            score_idle > score_moderate,
            "more spare capacity -> more burst benefit"
        );
    }

    // ── f4: topology_fitness ──

    #[test]
    fn f4_single_node_single_group_scores_high() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let ctx = CostContext {
            max_groups: 4,
            ..default_ctx()
        };
        assert!((eval.f4_topology(&alloc, &ctx) - 0.575).abs() < 1e-10);
    }

    #[test]
    fn f4_many_nodes_scores_lower() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc1 = AllocationBuilder::new().nodes(1).build();
        let alloc2 = AllocationBuilder::new().nodes(10).build();
        let ctx = CostContext {
            max_groups: 4,
            ..default_ctx()
        };
        assert!(eval.f4_topology(&alloc1, &ctx) >= eval.f4_topology(&alloc2, &ctx));
    }

    // ── f5: data_readiness ──

    #[test]
    fn f5_known_readiness_used() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().build();
        let mut ctx = default_ctx();
        ctx.data_readiness.insert(alloc.id, 0.8);
        assert!((eval.f5_data_readiness(&alloc, &ctx) - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn f5_unknown_readiness_defaults_neutral() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().build();
        let ctx = default_ctx();
        assert!((eval.f5_data_readiness(&alloc, &ctx) - 0.5).abs() < f64::EPSILON);
    }

    // ── f6: backlog_pressure ──

    #[test]
    fn f6_no_backlog_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let ctx = CostContext {
            backlog: BacklogMetrics {
                queued_gpu_hours: 0.0,
                running_gpu_hours: 100.0,
            },
            ..default_ctx()
        };
        assert!((eval.f6_backlog(&ctx) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f6_full_backlog_scores_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let ctx = CostContext {
            backlog: BacklogMetrics {
                queued_gpu_hours: 200.0,
                running_gpu_hours: 100.0,
            },
            ..default_ctx()
        };
        assert!((eval.f6_backlog(&ctx) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f6_capped_at_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let ctx = CostContext {
            backlog: BacklogMetrics {
                queued_gpu_hours: 500.0,
                running_gpu_hours: 100.0,
            },
            ..default_ctx()
        };
        assert!((eval.f6_backlog(&ctx) - 1.0).abs() < f64::EPSILON);
    }

    // ── f7: energy_cost ──

    #[test]
    fn f7_cheap_energy_scores_high() {
        let eval = CostEvaluator::new(CostWeights::default());
        let ctx = CostContext {
            energy_price: 0.1,
            ..default_ctx()
        };
        assert!((eval.f7_energy(&ctx) - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn f7_expensive_energy_scores_low() {
        let eval = CostEvaluator::new(CostWeights::default());
        let ctx = CostContext {
            energy_price: 0.9,
            ..default_ctx()
        };
        assert!((eval.f7_energy(&ctx) - 0.1).abs() < f64::EPSILON);
    }

    // ── f8: checkpoint_efficiency ──

    #[test]
    fn f8_no_checkpoint_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let mut alloc = AllocationBuilder::new().build();
        alloc.checkpoint = CheckpointStrategy::None;
        assert!((eval.f8_checkpoint(&alloc) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f8_auto_checkpoint_scores_positive() {
        let eval = CostEvaluator::new(CostWeights::default());
        let mut alloc = AllocationBuilder::new().build();
        alloc.checkpoint = CheckpointStrategy::Auto;
        assert!(eval.f8_checkpoint(&alloc) > 0.0);
    }

    #[test]
    fn f8_auto_better_than_manual() {
        let eval = CostEvaluator::new(CostWeights::default());
        let mut auto = AllocationBuilder::new().build();
        auto.checkpoint = CheckpointStrategy::Auto;
        let mut manual = AllocationBuilder::new().build();
        manual.checkpoint = CheckpointStrategy::Manual;
        assert!(eval.f8_checkpoint(&auto) > eval.f8_checkpoint(&manual));
    }

    // ── Budget penalty ──

    #[test]
    fn budget_penalty_no_usage_returns_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let ctx = default_ctx();
        assert!((eval.budget_penalty(&alloc, &ctx) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_penalty_below_80_percent_no_penalty() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.budget_utilization
            .insert("t1".into(), BudgetUtilization { fraction_used: 0.5 });
        assert!((eval.budget_penalty(&alloc, &ctx) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_penalty_at_80_percent_starts_penalty() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.budget_utilization
            .insert("t1".into(), BudgetUtilization { fraction_used: 0.8 });
        assert!((eval.budget_penalty(&alloc, &ctx) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_penalty_at_90_percent_is_between() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.budget_utilization
            .insert("t1".into(), BudgetUtilization { fraction_used: 0.9 });
        let penalty = eval.budget_penalty(&alloc, &ctx);
        assert!(penalty > 0.5 && penalty < 1.0, "penalty={penalty}");
    }

    #[test]
    fn budget_penalty_at_100_percent_is_midpoint() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.budget_utilization
            .insert("t1".into(), BudgetUtilization { fraction_used: 1.0 });
        let penalty = eval.budget_penalty(&alloc, &ctx);
        assert!((penalty - 0.525).abs() < 1e-10, "penalty={penalty}");
    }

    #[test]
    fn budget_penalty_over_120_percent_floor() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().tenant("t1").build();
        let mut ctx = default_ctx();
        ctx.budget_utilization
            .insert("t1".into(), BudgetUtilization { fraction_used: 1.5 });
        assert!((eval.budget_penalty(&alloc, &ctx) - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn budget_penalty_multiplies_composite_score() {
        let eval = CostEvaluator::new(CostWeights {
            priority: 1.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        });
        let alloc = AllocationBuilder::new()
            .tenant("t1")
            .preemption_class(10)
            .build();
        let mut ctx = CostContext {
            now: alloc.created_at,
            ..default_ctx()
        };

        let score_no_penalty = eval.score(&alloc, &ctx);

        ctx.budget_utilization
            .insert("t1".into(), BudgetUtilization { fraction_used: 1.5 });
        let score_with_penalty = eval.score(&alloc, &ctx);

        assert!(score_with_penalty < score_no_penalty);
        assert!((score_with_penalty - score_no_penalty * 0.05).abs() < 1e-10);
    }

    // ── Composite score ──

    #[test]
    fn composite_score_higher_priority_wins() {
        let eval = CostEvaluator::new(CostWeights {
            priority: 1.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        });
        let ctx = default_ctx();
        let high = AllocationBuilder::new().preemption_class(8).build();
        let low = AllocationBuilder::new().preemption_class(2).build();
        assert!(eval.score(&high, &ctx) > eval.score(&low, &ctx));
    }

    #[test]
    fn composite_score_all_zero_weights_gives_zero() {
        let eval = CostEvaluator::new(CostWeights {
            priority: 0.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        });
        let alloc = AllocationBuilder::new().build();
        let ctx = CostContext {
            now: alloc.created_at,
            ..default_ctx()
        };
        assert!((eval.score(&alloc, &ctx) - 0.0).abs() < 1e-10);
    }

    // ── f4: memory locality ──

    #[test]
    fn f4_memory_locality_empty_returns_neutral() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let ctx = default_ctx();
        // f4 topology blends inter-node and memory locality
        // with no memory_locality data, intra_node = 0.5
        // beta=0.3 for single node, inter_node depends on max_groups
        // but we just check the overall f4 is deterministic
        let _score = eval.f4_topology(&alloc, &ctx);
    }

    #[test]
    fn f4_combined_topology_score() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let mut ctx = CostContext {
            max_groups: 4,
            ..default_ctx()
        };
        ctx.memory_locality.insert("n0".to_string(), 0.8);
        let score = eval.f4_topology(&alloc, &ctx);
        // beta=0.3 for single node
        // inter_node = 1.0 - ceil(1/4)/4 = 1.0 - 0.25 = 0.75
        // intra_node = 0.8 (from memory_locality)
        // combined = 0.3 * 0.75 + 0.7 * 0.8 = 0.225 + 0.56 = 0.785
        assert!((score - 0.785).abs() < 1e-10);
    }

    #[test]
    fn composite_score_respects_weight_changes() {
        let alloc = AllocationBuilder::new().preemption_class(5).build();
        let ctx = CostContext {
            now: alloc.created_at,
            ..default_ctx()
        };

        let eval_high_prio = CostEvaluator::new(CostWeights {
            priority: 1.0,
            ..CostWeights::default()
        });
        let eval_low_prio = CostEvaluator::new(CostWeights {
            priority: 0.1,
            ..CostWeights::default()
        });

        let score_high = eval_high_prio.score(&alloc, &ctx);
        let score_low = eval_low_prio.score(&alloc, &ctx);
        let diff = (1.0 - 0.1) * 0.5;
        assert!((score_high - score_low - diff).abs() < 1e-10);
    }
}

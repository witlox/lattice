//! Composite cost function evaluator.
//!
//! Computes `Score(j) = Σ wᵢ · fᵢ(j)` for each pending allocation
//! against the current system state.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use lattice_common::types::*;

/// Per-tenant resource usage snapshot (for fair-share calculation).
#[derive(Debug, Clone)]
pub struct TenantUsage {
    /// Target share fraction (from tenant quota, 0.0-1.0)
    pub target_share: f64,
    /// Actual usage fraction (nodes in use / total available nodes, 0.0-1.0)
    pub actual_usage: f64,
}

/// System-wide backlog metrics.
#[derive(Debug, Clone)]
pub struct BacklogMetrics {
    pub queued_gpu_hours: f64,
    pub running_gpu_hours: f64,
}

impl Default for BacklogMetrics {
    fn default() -> Self {
        Self {
            queued_gpu_hours: 0.0,
            running_gpu_hours: 1.0, // avoid division by zero
        }
    }
}

/// Input context for the cost evaluator.
#[derive(Debug, Clone)]
pub struct CostContext {
    /// Per-tenant usage snapshots
    pub tenant_usage: HashMap<TenantId, TenantUsage>,
    /// System-wide backlog
    pub backlog: BacklogMetrics,
    /// Normalized energy price (0.0 = cheapest, 1.0 = most expensive)
    pub energy_price: f64,
    /// Pre-fetched data readiness scores per allocation (0.0-1.0)
    pub data_readiness: HashMap<AllocId, f64>,
    /// Reference wait time in seconds (default: 3600 = 1 hour)
    pub reference_wait_seconds: f64,
    /// Total available groups in the topology
    pub max_groups: u32,
    /// Current time (for wait-time computation)
    pub now: DateTime<Utc>,
    /// Pre-computed memory locality scores per node (0.0-1.0)
    pub memory_locality: HashMap<NodeId, f64>,
}

impl Default for CostContext {
    fn default() -> Self {
        Self {
            tenant_usage: HashMap::new(),
            backlog: BacklogMetrics::default(),
            energy_price: 0.5,
            data_readiness: HashMap::new(),
            reference_wait_seconds: 3600.0,
            max_groups: 1,
            now: Utc::now(),
            memory_locality: HashMap::new(),
        }
    }
}

/// Evaluates the composite cost function for allocations.
pub struct CostEvaluator {
    pub weights: CostWeights,
}

impl CostEvaluator {
    pub fn new(weights: CostWeights) -> Self {
        Self { weights }
    }

    /// Compute the composite score for an allocation.
    ///
    /// Higher scores mean higher scheduling priority.
    pub fn score(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        let w = &self.weights;

        w.priority * self.f1_priority(alloc)
            + w.wait_time * self.f2_wait_time(alloc, ctx)
            + w.fair_share * self.f3_fair_share(alloc, ctx)
            + w.topology * self.f4_topology(alloc, ctx)
            + w.data_readiness * self.f5_data_readiness(alloc, ctx)
            + w.backlog * self.f6_backlog(ctx)
            + w.energy * self.f7_energy(ctx)
            + w.checkpoint_efficiency * self.f8_checkpoint(alloc)
        // f9 (conformance) is handled during node selection, not scoring
    }

    /// f₁: priority_class — normalized to [0.0, 1.0]
    pub fn f1_priority(&self, alloc: &Allocation) -> f64 {
        alloc.lifecycle.preemption_class as f64 / 10.0
    }

    /// f₂: wait_time_factor — log-scaled anti-starvation
    pub fn f2_wait_time(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        let wait_seconds = (ctx.now - alloc.created_at).num_seconds().max(0) as f64;
        (1.0 + wait_seconds / ctx.reference_wait_seconds).ln()
    }

    /// f₃: fair_share_deficit — how far the tenant is from their target share
    pub fn f3_fair_share(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        match ctx.tenant_usage.get(&alloc.tenant) {
            Some(usage) if usage.target_share > 0.0 => {
                ((usage.target_share - usage.actual_usage) / usage.target_share).max(0.0)
            }
            _ => 0.5, // neutral when unknown
        }
    }

    /// f₄: topology_fitness — inter-node group packing + intra-node memory locality
    ///
    /// Combines two signals:
    /// - Inter-node: jobs needing fewer dragonfly groups score higher
    /// - Intra-node: nodes where resources share a memory domain score higher
    ///
    /// The blend factor (beta) depends on the workload type:
    /// - GPU-heavy (gpu_count > cpu_cores/8): 0.7 inter-node, 0.3 memory
    /// - CPU-heavy: 0.3 inter-node, 0.7 memory
    /// - Default: 0.5 / 0.5
    pub fn f4_topology(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        let inter_node = self.f4_inter_node(alloc, ctx);
        let intra_node = self.f4_memory_locality(alloc, ctx);
        let beta = self.memory_topology_beta(alloc);
        beta * inter_node + (1.0 - beta) * intra_node
    }

    /// Inter-node topology score: jobs needing fewer groups score higher.
    fn f4_inter_node(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        if ctx.max_groups == 0 {
            return 0.5;
        }
        let requested_nodes = match alloc.resources.nodes {
            NodeCount::Exact(n) => n,
            NodeCount::Range { min, .. } => min,
        };
        let groups_needed = (requested_nodes as f64 / ctx.max_groups as f64)
            .ceil()
            .max(1.0);
        1.0 - (groups_needed / ctx.max_groups as f64).min(1.0)
    }

    /// Intra-node memory locality score: average memory locality across candidate nodes.
    ///
    /// Uses pre-computed `memory_locality` from CostContext. Nodes without scores
    /// return 0.5 (neutral). Boosted by `prefer_same_numa` soft constraint.
    fn f4_memory_locality(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        if ctx.memory_locality.is_empty() {
            return 0.5;
        }

        // Average locality across assigned nodes (or all known nodes if not yet assigned)
        let nodes: Vec<&NodeId> = if !alloc.assigned_nodes.is_empty() {
            alloc.assigned_nodes.iter().collect()
        } else {
            ctx.memory_locality.keys().collect()
        };

        if nodes.is_empty() {
            return 0.5;
        }

        let sum: f64 = nodes
            .iter()
            .map(|n| ctx.memory_locality.get(*n).copied().unwrap_or(0.5))
            .sum();
        let avg = sum / nodes.len() as f64;

        // Boost if prefer_same_numa is set
        if alloc.resources.constraints.prefer_same_numa {
            (avg * 1.2).min(1.0)
        } else {
            avg
        }
    }

    /// Compute the blend factor between inter-node and memory locality scoring.
    fn memory_topology_beta(&self, alloc: &Allocation) -> f64 {
        let requested_nodes = match alloc.resources.nodes {
            NodeCount::Exact(n) => n,
            NodeCount::Range { min, .. } => min,
        };

        if requested_nodes <= 1 {
            // Single-node: memory locality matters more
            return 0.3;
        }

        // Multi-node: inter-node topology matters more
        0.7
    }

    /// f₅: data_readiness — fraction of input data on hot tier
    pub fn f5_data_readiness(&self, alloc: &Allocation, ctx: &CostContext) -> f64 {
        ctx.data_readiness.get(&alloc.id).copied().unwrap_or(0.5)
    }

    /// f₆: backlog_pressure — system-wide queue pressure
    pub fn f6_backlog(&self, ctx: &CostContext) -> f64 {
        if ctx.backlog.running_gpu_hours <= 0.0 {
            return 0.0;
        }
        (ctx.backlog.queued_gpu_hours / ctx.backlog.running_gpu_hours).min(1.0)
    }

    /// f₇: energy_cost — higher when energy is cheaper
    pub fn f7_energy(&self, ctx: &CostContext) -> f64 {
        1.0 - ctx.energy_price.clamp(0.0, 1.0)
    }

    /// f₈: checkpoint_efficiency — fast checkpoint = more attractive
    pub fn f8_checkpoint(&self, alloc: &Allocation) -> f64 {
        match alloc.checkpoint {
            CheckpointStrategy::Auto => {
                // Assume moderate checkpoint time (5 min) for auto
                1.0 / (1.0 + 5.0)
            }
            CheckpointStrategy::Manual => {
                // Manual: application handles it, assume 10 min
                1.0 / (1.0 + 10.0)
            }
            CheckpointStrategy::None => {
                // No checkpoint: infinite cost
                0.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    fn default_ctx() -> CostContext {
        CostContext::default()
    }

    // ── f₁: priority_class ──

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

    // ── f₂: wait_time_factor ──

    #[test]
    fn f2_zero_wait_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().build();
        let ctx = CostContext {
            now: alloc.created_at,
            ..default_ctx()
        };
        // ln(1 + 0/3600) = ln(1) = 0
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
        // ln(1 + 3600/3600) = ln(2) ≈ 0.693
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

    // ── f₃: fair_share_deficit ──

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

    // ── f₄: topology_fitness ──

    #[test]
    fn f4_single_node_single_group_scores_high() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let ctx = CostContext {
            max_groups: 4,
            ..default_ctx()
        };
        // Single-node: beta=0.3, inter_node=0.75, intra_node=0.5 (no memory data)
        // blended = 0.3 * 0.75 + 0.7 * 0.5 = 0.575
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

    // ── f₅: data_readiness ──

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

    // ── f₆: backlog_pressure ──

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

    // ── f₇: energy_cost ──

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

    // ── f₈: checkpoint_efficiency ──

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

    // ── f₄: memory locality ──

    #[test]
    fn f4_memory_locality_empty_returns_neutral() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let ctx = default_ctx();
        let score = eval.f4_memory_locality(&alloc, &ctx);
        assert!((score - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn f4_memory_locality_with_scores() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let mut ctx = default_ctx();
        ctx.memory_locality.insert("n0".to_string(), 1.0);
        ctx.memory_locality.insert("n1".to_string(), 0.5);
        let score = eval.f4_memory_locality(&alloc, &ctx);
        // Average of 1.0 and 0.5 = 0.75
        assert!((score - 0.75).abs() < 1e-10);
    }

    #[test]
    fn f4_prefer_same_numa_boosts_score() {
        let eval = CostEvaluator::new(CostWeights::default());
        let mut alloc = AllocationBuilder::new().nodes(1).build();
        alloc.resources.constraints.prefer_same_numa = true;
        let mut ctx = default_ctx();
        ctx.memory_locality.insert("n0".to_string(), 0.8);
        let score = eval.f4_memory_locality(&alloc, &ctx);
        // 0.8 * 1.2 = 0.96
        assert!((score - 0.96).abs() < 1e-10);
    }

    #[test]
    fn f4_prefer_same_numa_capped_at_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let mut alloc = AllocationBuilder::new().nodes(1).build();
        alloc.resources.constraints.prefer_same_numa = true;
        let mut ctx = default_ctx();
        ctx.memory_locality.insert("n0".to_string(), 1.0);
        let score = eval.f4_memory_locality(&alloc, &ctx);
        assert!((score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn f4_single_node_uses_memory_weight() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(1).build();
        let beta = eval.memory_topology_beta(&alloc);
        // Single node: memory locality matters more → beta = 0.3
        assert!((beta - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn f4_multi_node_uses_inter_node_weight() {
        let eval = CostEvaluator::new(CostWeights::default());
        let alloc = AllocationBuilder::new().nodes(4).build();
        let beta = eval.memory_topology_beta(&alloc);
        // Multi-node: inter-node topology matters more → beta = 0.7
        assert!((beta - 0.7).abs() < f64::EPSILON);
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

        // Same allocation, different weights — priority-heavy should score higher
        // since the allocation has class=5 giving f1=0.5
        let score_high = eval_high_prio.score(&alloc, &ctx);
        let score_low = eval_low_prio.score(&alloc, &ctx);
        // Not necessarily higher since other factors contribute, but priority component differs
        let diff = (1.0 - 0.1) * 0.5; // weight difference * f1 value
        assert!((score_high - score_low - diff).abs() < 1e-10);
    }
}

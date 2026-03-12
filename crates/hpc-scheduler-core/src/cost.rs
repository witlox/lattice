//! Composite cost function evaluator.
//!
//! Computes `Score(j) = Σ wᵢ · fᵢ(j)` for each pending job
//! against the current system state.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::traits::Job;
use crate::types::{CheckpointKind, CostWeights};

/// Per-tenant resource usage snapshot (for fair-share calculation).
#[derive(Debug, Clone)]
pub struct TenantUsage {
    /// Target share fraction (from tenant quota, 0.0-1.0)
    pub target_share: f64,
    /// Actual usage fraction (nodes in use / total available nodes, 0.0-1.0)
    pub actual_usage: f64,
    /// Burst allowance multiplier. When the system has spare capacity,
    /// the effective target is expanded by this factor.
    pub burst_allowance: Option<f64>,
    /// Fraction of system capacity currently in use (0.0-1.0).
    pub system_utilization: f64,
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
            running_gpu_hours: 1.0,
        }
    }
}

/// Per-tenant GPU-hours budget utilization (for soft penalty).
#[derive(Debug, Clone)]
pub struct BudgetUtilization {
    /// Fraction of budget consumed (0.0 = nothing used, >1.0 = over budget).
    pub fraction_used: f64,
}

/// Input context for the cost evaluator.
#[derive(Debug, Clone)]
pub struct CostContext {
    /// Per-tenant usage snapshots (keyed by tenant ID).
    pub tenant_usage: HashMap<String, TenantUsage>,
    /// Per-tenant GPU-hours budget utilization.
    pub budget_utilization: HashMap<String, BudgetUtilization>,
    /// System-wide backlog.
    pub backlog: BacklogMetrics,
    /// Normalized energy price (0.0 = cheapest, 1.0 = most expensive).
    pub energy_price: f64,
    /// Pre-fetched data readiness scores per job (0.0-1.0).
    pub data_readiness: HashMap<uuid::Uuid, f64>,
    /// Reference wait time in seconds (default: 3600 = 1 hour).
    pub reference_wait_seconds: f64,
    /// Total available groups in the topology.
    pub max_groups: u32,
    /// Current time (for wait-time computation).
    pub now: DateTime<Utc>,
    /// Pre-computed memory locality scores per node (0.0-1.0).
    pub memory_locality: HashMap<String, f64>,
    /// Pre-computed conformance fitness per job (0.0-1.0).
    /// Score = largest_conformance_group_size / requested_nodes.
    /// 1.0 means all candidate nodes share the same configuration.
    pub conformance_fitness: HashMap<uuid::Uuid, f64>,
}

impl Default for CostContext {
    fn default() -> Self {
        Self {
            tenant_usage: HashMap::new(),
            budget_utilization: HashMap::new(),
            backlog: BacklogMetrics::default(),
            energy_price: 0.5,
            data_readiness: HashMap::new(),
            reference_wait_seconds: 3600.0,
            max_groups: 1,
            now: Utc::now(),
            memory_locality: HashMap::new(),
            conformance_fitness: HashMap::new(),
        }
    }
}

/// Evaluates the composite cost function for jobs.
pub struct CostEvaluator {
    pub weights: CostWeights,
}

impl CostEvaluator {
    pub fn new(weights: CostWeights) -> Self {
        Self { weights }
    }

    /// Compute the composite score for a job.
    /// Higher scores mean higher scheduling priority.
    pub fn score<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        let w = &self.weights;

        let base = w.priority * self.f1_priority(job)
            + w.wait_time * self.f2_wait_time(job, ctx)
            + w.fair_share * self.f3_fair_share(job, ctx)
            + w.topology * self.f4_topology(job, ctx)
            + w.data_readiness * self.f5_data_readiness(job, ctx)
            + w.backlog * self.f6_backlog(ctx)
            + w.energy * self.f7_energy(ctx)
            + w.checkpoint_efficiency * self.f8_checkpoint(job)
            + w.conformance * self.f9_conformance(job, ctx);

        base * self.budget_penalty(job, ctx)
    }

    /// f₁: priority_class — normalized to [0.0, 1.0]
    pub fn f1_priority<J: Job>(&self, job: &J) -> f64 {
        job.preemption_class() as f64 / 10.0
    }

    /// f₂: wait_time_factor — log-scaled anti-starvation
    pub fn f2_wait_time<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        let wait_seconds = (ctx.now - job.created_at()).num_seconds().max(0) as f64;
        (1.0 + wait_seconds / ctx.reference_wait_seconds).ln()
    }

    /// f₃: fair_share_deficit — how far the tenant is from their target share.
    /// Burst-aware: expands effective target when system has spare capacity.
    pub fn f3_fair_share<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        match ctx.tenant_usage.get(job.tenant_id()) {
            Some(usage) if usage.target_share > 0.0 => {
                let effective_target = match usage.burst_allowance {
                    Some(burst) if usage.system_utilization < 0.8 => {
                        let spare_factor = (0.8 - usage.system_utilization) / 0.8;
                        usage.target_share * (1.0 + burst * spare_factor)
                    }
                    _ => usage.target_share,
                };
                ((effective_target - usage.actual_usage) / effective_target).max(0.0)
            }
            _ => 0.5,
        }
    }

    /// f₄: topology_fitness — inter-node group packing + intra-node memory locality.
    pub fn f4_topology<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        let inter_node = self.f4_inter_node(job, ctx);
        let intra_node = self.f4_memory_locality(job, ctx);
        let beta = self.memory_topology_beta(job);
        beta * inter_node + (1.0 - beta) * intra_node
    }

    fn f4_inter_node<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        if ctx.max_groups == 0 {
            return 0.5;
        }
        let requested_nodes = job.node_count_min();
        let groups_needed = (requested_nodes as f64 / ctx.max_groups as f64)
            .ceil()
            .max(1.0);
        1.0 - (groups_needed / ctx.max_groups as f64).min(1.0)
    }

    fn f4_memory_locality<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        if ctx.memory_locality.is_empty() {
            return 0.5;
        }

        let nodes: Vec<&String> = if !job.assigned_nodes().is_empty() {
            job.assigned_nodes().iter().collect()
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

        if job.prefer_same_numa() {
            (avg * 1.2).min(1.0)
        } else {
            avg
        }
    }

    fn memory_topology_beta<J: Job>(&self, job: &J) -> f64 {
        if job.node_count_min() <= 1 {
            0.3 // Single-node: memory locality matters more
        } else {
            0.7 // Multi-node: inter-node topology matters more
        }
    }

    /// f₅: data_readiness — fraction of input data on hot tier.
    pub fn f5_data_readiness<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        ctx.data_readiness.get(&job.id()).copied().unwrap_or(0.5)
    }

    /// f₆: backlog_pressure — system-wide queue pressure.
    pub fn f6_backlog(&self, ctx: &CostContext) -> f64 {
        if ctx.backlog.running_gpu_hours <= 0.0 {
            return 0.0;
        }
        (ctx.backlog.queued_gpu_hours / ctx.backlog.running_gpu_hours).min(1.0)
    }

    /// f₇: energy_cost — higher when energy is cheaper.
    pub fn f7_energy(&self, ctx: &CostContext) -> f64 {
        1.0 - ctx.energy_price.clamp(0.0, 1.0)
    }

    /// GPU-hours budget penalty.
    pub fn budget_penalty<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        match ctx.budget_utilization.get(job.tenant_id()) {
            Some(bu) => budget_penalty_curve(bu.fraction_used),
            None => 1.0,
        }
    }

    /// f₉: conformance_fitness — configuration homogeneity of candidate nodes.
    /// Pre-computed per job and stored in `CostContext::conformance_fitness`.
    /// Returns 0.5 (neutral) if not available.
    pub fn f9_conformance<J: Job>(&self, job: &J, ctx: &CostContext) -> f64 {
        ctx.conformance_fitness
            .get(&job.id())
            .copied()
            .unwrap_or(0.5)
    }

    /// f₈: checkpoint_efficiency — fast checkpoint = more attractive.
    pub fn f8_checkpoint<J: Job>(&self, job: &J) -> f64 {
        match job.checkpoint_kind() {
            CheckpointKind::Auto => 1.0 / (1.0 + 5.0),
            CheckpointKind::Manual => 1.0 / (1.0 + 10.0),
            CheckpointKind::None => 0.0,
        }
    }
}

/// Smooth budget penalty curve.
///
/// - u ≤ 0.8: 1.0 (no penalty)
/// - 0.8 < u ≤ 1.2: smooth cosine ramp from 1.0 → 0.05
/// - u > 1.2: 0.05 (floor)
fn budget_penalty_curve(u: f64) -> f64 {
    if u <= 0.8 {
        1.0
    } else if u <= 1.2 {
        let t = (u - 0.8) / 0.4; // 0.0 at u=0.8, 1.0 at u=1.2
        let cosine = (1.0 + (t * std::f64::consts::PI).cos()) / 2.0; // 1.0 → 0.0
        0.05 + 0.95 * cosine // 1.0 → 0.05
    } else {
        0.05
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodeConstraints;

    struct TestJob {
        id: uuid::Uuid,
        tenant: String,
        preemption_class: u8,
        created_at: DateTime<Utc>,
        nodes_min: u32,
        assigned_nodes: Vec<String>,
        checkpoint: CheckpointKind,
        prefer_same_numa: bool,
    }

    impl Default for TestJob {
        fn default() -> Self {
            Self {
                id: uuid::Uuid::new_v4(),
                tenant: "t1".into(),
                preemption_class: 5,
                created_at: Utc::now(),
                nodes_min: 1,
                assigned_nodes: vec![],
                checkpoint: CheckpointKind::Auto,
                prefer_same_numa: false,
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
            Some(chrono::Duration::hours(1))
        }
        fn preemption_class(&self) -> u8 {
            self.preemption_class
        }
        fn created_at(&self) -> DateTime<Utc> {
            self.created_at
        }
        fn started_at(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn assigned_nodes(&self) -> &[String] {
            &self.assigned_nodes
        }
        fn checkpoint_kind(&self) -> CheckpointKind {
            self.checkpoint
        }
        fn is_running(&self) -> bool {
            false
        }
        fn is_sensitive(&self) -> bool {
            false
        }
        fn prefer_same_numa(&self) -> bool {
            self.prefer_same_numa
        }
        fn topology_preference(&self) -> Option<crate::types::TopologyPreference> {
            None
        }
        fn constraints(&self) -> NodeConstraints {
            NodeConstraints::default()
        }
    }

    fn default_ctx() -> CostContext {
        CostContext::default()
    }

    #[test]
    fn f1_class_0_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let job = TestJob {
            preemption_class: 0,
            ..Default::default()
        };
        assert!((eval.f1_priority(&job) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f1_class_10_scores_one() {
        let eval = CostEvaluator::new(CostWeights::default());
        let job = TestJob {
            preemption_class: 10,
            ..Default::default()
        };
        assert!((eval.f1_priority(&job) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f3_tenant_at_target_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let job = TestJob::default();
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
        assert!((eval.f3_fair_share(&job, &ctx) - 0.0).abs() < f64::EPSILON);
    }

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
    fn f8_no_checkpoint_scores_zero() {
        let eval = CostEvaluator::new(CostWeights::default());
        let job = TestJob {
            checkpoint: CheckpointKind::None,
            ..Default::default()
        };
        assert!((eval.f8_checkpoint(&job) - 0.0).abs() < f64::EPSILON);
    }

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
        let high = TestJob {
            preemption_class: 8,
            ..Default::default()
        };
        let low = TestJob {
            preemption_class: 2,
            ..Default::default()
        };
        assert!(eval.score(&high, &ctx) > eval.score(&low, &ctx));
    }
}

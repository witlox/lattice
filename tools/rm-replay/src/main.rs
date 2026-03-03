use anyhow::{anyhow, Result};
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use lattice_common::types::*;
use lattice_scheduler::cost::{BacklogMetrics, CostContext, CostEvaluator, TenantUsage};

/// RM-Replay: Scheduler weight simulator for testing cost function changes before production
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to JSON trace file containing allocation events
    #[arg(short, long)]
    input: PathBuf,

    /// Path to JSON weights file (uses defaults if not provided)
    #[arg(short, long)]
    weights: Option<PathBuf>,

    /// Path to output file (prints to stdout if not provided)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Scoring mode: 'real' uses CostEvaluator from lattice-scheduler, 'simple' uses
    /// the original heuristic scoring for comparison
    #[arg(short, long, value_enum, default_value = "real")]
    mode: ScoringMode,

    /// Normalized energy price (0.0 = cheapest, 1.0 = most expensive). Defaults to 0.5.
    #[arg(long, default_value = "0.5")]
    energy_price: f64,

    /// Max topology groups in the system. Defaults to 4.
    #[arg(long, default_value = "4")]
    max_groups: u32,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScoringMode {
    /// Use the real CostEvaluator from lattice-scheduler
    Real,
    /// Use the original simple heuristic scoring
    Simple,
}

/// A single allocation entry from the trace file
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TraceEntry {
    /// Unique allocation identifier
    allocation_id: String,
    /// Timestamp in seconds since epoch
    timestamp: u64,
    /// Number of nodes required
    node_count: u32,
    /// Priority class (lower = higher priority)
    priority: u32,
    /// Tenant identifier
    tenant: String,
    /// Optional: checkpoint strategy ("auto", "manual", "none"). Defaults to "auto".
    #[serde(default = "default_checkpoint")]
    checkpoint: String,
    /// Optional: data readiness score (0.0-1.0). Defaults to 0.5.
    #[serde(default = "default_data_readiness")]
    data_readiness: f64,
    /// Optional: tenant target share (0.0-1.0). Defaults to 0.1.
    #[serde(default = "default_target_share")]
    target_share: f64,
    /// Optional: tenant actual usage (0.0-1.0). Defaults to 0.0.
    #[serde(default)]
    actual_usage: f64,
}

fn default_checkpoint() -> String {
    "auto".into()
}

fn default_data_readiness() -> f64 {
    0.5
}

fn default_target_share() -> f64 {
    0.1
}

/// Scheduling cost function weights (for both simple and real modes)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Weights {
    /// Priority class weight (preemption tier)
    priority: f64,
    /// Wait time weight (anti-starvation)
    wait_time: f64,
    /// Fair share deficit weight (tenant equity)
    fair_share: f64,
    /// Topology fitness weight (Slingshot dragonfly packing)
    topology: f64,
    /// Data readiness weight (hot tier proximity)
    data_readiness: f64,
    /// Backlog pressure weight (queue depth)
    backlog: f64,
    /// Energy cost weight (time-varying electricity)
    energy: f64,
    /// Checkpoint efficiency weight (preemption cost)
    checkpoint: f64,
    /// Conformance fitness weight (node config homogeneity)
    conformance: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            priority: 1.0,
            wait_time: 0.8,
            fair_share: 0.7,
            topology: 0.6,
            data_readiness: 0.5,
            backlog: 0.4,
            energy: 0.3,
            checkpoint: 0.5,
            conformance: 0.4,
        }
    }
}

impl From<&Weights> for CostWeights {
    fn from(w: &Weights) -> Self {
        CostWeights {
            priority: w.priority,
            wait_time: w.wait_time,
            fair_share: w.fair_share,
            topology: w.topology,
            data_readiness: w.data_readiness,
            backlog: w.backlog,
            energy: w.energy,
            checkpoint_efficiency: w.checkpoint,
            conformance: w.conformance,
        }
    }
}

/// Per-factor score breakdown for detailed analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScoreBreakdown {
    /// f1: priority_class factor
    priority: f64,
    /// f2: wait_time_factor
    wait_time: f64,
    /// f3: fair_share_deficit
    fair_share: f64,
    /// f4: topology_fitness
    topology: f64,
    /// f5: data_readiness
    data_readiness: f64,
    /// f6: backlog_pressure
    backlog: f64,
    /// f7: energy_cost
    energy: f64,
    /// f8: checkpoint_efficiency
    checkpoint: f64,
}

/// Result of simulating an allocation's scheduling score
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SimulationResult {
    /// Allocation identifier
    allocation_id: String,
    /// Composite score from the cost function
    score: f64,
    /// Rank in the final ordering (1 = highest priority)
    rank: usize,
    /// Per-factor score breakdown (only present in real mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    breakdown: Option<ScoreBreakdown>,
}

/// Convert a TraceEntry to a lattice Allocation for the real evaluator
fn trace_to_allocation(entry: &TraceEntry) -> Allocation {
    let checkpoint = match entry.checkpoint.as_str() {
        "manual" => CheckpointStrategy::Manual,
        "none" => CheckpointStrategy::None,
        _ => CheckpointStrategy::Auto,
    };

    Allocation {
        id: Uuid::new_v4(),
        tenant: entry.tenant.clone(),
        project: "replay".into(),
        vcluster: "default".into(),
        user: "replay-user".into(),
        tags: HashMap::new(),
        allocation_type: AllocationType::Single,
        environment: Environment {
            uenv: None,
            view: None,
            image: None,
            tools_uenv: None,
            sign_required: false,
            scan_required: false,
            approved_bases_only: false,
        },
        entrypoint: format!("trace:{}", entry.allocation_id),
        resources: ResourceRequest {
            nodes: NodeCount::Exact(entry.node_count),
            constraints: ResourceConstraints::default(),
        },
        lifecycle: Lifecycle {
            lifecycle_type: LifecycleType::Bounded {
                walltime: chrono::Duration::hours(1),
            },
            preemption_class: entry.priority.min(10) as u8,
        },
        requeue_policy: RequeuePolicy::Never,
        max_requeue: 0,
        data: DataRequirements::default(),
        connectivity: Connectivity::default(),
        depends_on: Vec::new(),
        checkpoint,
        telemetry_mode: TelemetryMode::Prod,
        state: AllocationState::Pending,
        created_at: Utc.timestamp_opt(entry.timestamp as i64, 0).unwrap(),
        started_at: None,
        completed_at: None,
        assigned_nodes: Vec::new(),
        dag_id: None,
        exit_code: None,
        message: None,
        requeue_count: 0,
        preempted_count: 0,
        resume_from_checkpoint: false,
    }
}

/// Build a CostContext from the trace entries and configuration
fn build_cost_context(
    trace: &[TraceEntry],
    alloc_map: &HashMap<String, Allocation>,
    now: DateTime<Utc>,
    energy_price: f64,
    max_groups: u32,
) -> CostContext {
    let mut tenant_usage: HashMap<TenantId, TenantUsage> = HashMap::new();
    for entry in trace {
        tenant_usage
            .entry(entry.tenant.clone())
            .or_insert_with(|| TenantUsage {
                target_share: entry.target_share,
                actual_usage: entry.actual_usage,
            });
    }

    let mut data_readiness: HashMap<AllocId, f64> = HashMap::new();
    for entry in trace {
        if let Some(alloc) = alloc_map.get(&entry.allocation_id) {
            data_readiness.insert(alloc.id, entry.data_readiness);
        }
    }

    let total_queued: f64 = trace.iter().map(|e| e.node_count as f64).sum();
    let backlog = BacklogMetrics {
        queued_gpu_hours: total_queued,
        running_gpu_hours: (total_queued * 2.0).max(1.0),
    };

    CostContext {
        tenant_usage,
        backlog,
        energy_price,
        data_readiness,
        reference_wait_seconds: 3600.0,
        max_groups,
        now,
    }
}

/// Score using the real CostEvaluator and return breakdown
fn score_real(
    _entry: &TraceEntry,
    alloc: &Allocation,
    evaluator: &CostEvaluator,
    ctx: &CostContext,
) -> (f64, ScoreBreakdown) {
    let composite = evaluator.score(alloc, ctx);

    let breakdown = ScoreBreakdown {
        priority: evaluator.f1_priority(alloc),
        wait_time: evaluator.f2_wait_time(alloc, ctx),
        fair_share: evaluator.f3_fair_share(alloc, ctx),
        topology: evaluator.f4_topology(alloc, ctx),
        data_readiness: evaluator.f5_data_readiness(alloc, ctx),
        backlog: evaluator.f6_backlog(ctx),
        energy: evaluator.f7_energy(ctx),
        checkpoint: evaluator.f8_checkpoint(alloc),
    };

    (composite, breakdown)
}

/// Calculate the composite score using the simple heuristic (legacy mode)
fn calculate_score(entry: &TraceEntry, weights: &Weights, base_time: u64) -> f64 {
    let wait_time_factor = (base_time.saturating_sub(entry.timestamp)) as f64 / 3600.0;
    let priority_factor = entry.priority as f64;
    let node_factor = entry.node_count as f64 / 100.0;
    let tenant_factor = entry.tenant.len() as f64;

    let mut score = 0.0;
    score += weights.priority * priority_factor;
    score += weights.wait_time * wait_time_factor;
    score += weights.fair_share * tenant_factor;
    score += weights.topology * node_factor;
    score += weights.data_readiness * (node_factor * 0.5);
    score += weights.backlog * priority_factor.max(1.0);
    score += weights.energy * (1.0 / (node_factor + 1.0));
    score += weights.checkpoint * (1.0 / (priority_factor + 1.0));
    score += weights.conformance * node_factor;

    score
}

/// Load and parse a JSON trace file
fn load_trace(path: &PathBuf) -> Result<Vec<TraceEntry>> {
    let content =
        fs::read_to_string(path).map_err(|e| anyhow!("Failed to read trace file: {}", e))?;
    serde_json::from_str(&content).map_err(|e| anyhow!("Failed to parse trace file: {}", e))
}

/// Load and parse a JSON weights file
fn load_weights(path: &PathBuf) -> Result<Weights> {
    let content =
        fs::read_to_string(path).map_err(|e| anyhow!("Failed to read weights file: {}", e))?;
    serde_json::from_str(&content).map_err(|e| anyhow!("Failed to parse weights file: {}", e))
}

/// Run simulation in simple (legacy) mode
fn simulate_simple(trace: Vec<TraceEntry>, weights: Weights) -> Result<Vec<SimulationResult>> {
    if trace.is_empty() {
        return Ok(Vec::new());
    }

    let base_time = trace.iter().map(|e| e.timestamp).min().unwrap_or(0);

    let mut results: Vec<SimulationResult> = trace
        .iter()
        .map(|entry| {
            let score = calculate_score(entry, &weights, base_time);
            SimulationResult {
                allocation_id: entry.allocation_id.clone(),
                score,
                rank: 0,
                breakdown: None,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (idx, result) in results.iter_mut().enumerate() {
        result.rank = idx + 1;
    }

    Ok(results)
}

/// Run simulation in real mode using CostEvaluator
fn simulate_real(
    trace: Vec<TraceEntry>,
    weights: Weights,
    energy_price: f64,
    max_groups: u32,
) -> Result<Vec<SimulationResult>> {
    if trace.is_empty() {
        return Ok(Vec::new());
    }

    let mut alloc_map: HashMap<String, Allocation> = HashMap::new();
    for entry in &trace {
        alloc_map.insert(entry.allocation_id.clone(), trace_to_allocation(entry));
    }

    let max_ts = trace.iter().map(|e| e.timestamp).max().unwrap_or(0);
    let now = Utc.timestamp_opt(max_ts as i64 + 3600, 0).unwrap();

    let ctx = build_cost_context(&trace, &alloc_map, now, energy_price, max_groups);

    let cost_weights: CostWeights = (&weights).into();
    let evaluator = CostEvaluator::new(cost_weights);

    let mut results: Vec<SimulationResult> = trace
        .iter()
        .map(|entry| {
            let alloc = alloc_map.get(&entry.allocation_id).unwrap();
            let (score, breakdown) = score_real(entry, alloc, &evaluator, &ctx);
            SimulationResult {
                allocation_id: entry.allocation_id.clone(),
                score,
                rank: 0,
                breakdown: Some(breakdown),
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (idx, result) in results.iter_mut().enumerate() {
        result.rank = idx + 1;
    }

    Ok(results)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let trace = load_trace(&args.input)?;
    eprintln!("Loaded {} allocations from trace", trace.len());

    let weights = if let Some(weights_path) = &args.weights {
        load_weights(weights_path)?
    } else {
        eprintln!("Using default weights");
        Weights::default()
    };

    let mode_name = match args.mode {
        ScoringMode::Real => "real (CostEvaluator)",
        ScoringMode::Simple => "simple (heuristic)",
    };
    eprintln!("Scoring mode: {}", mode_name);

    let results = match args.mode {
        ScoringMode::Simple => simulate_simple(trace, weights)?,
        ScoringMode::Real => simulate_real(trace, weights, args.energy_price, args.max_groups)?,
    };
    eprintln!("Simulation complete, scored {} allocations", results.len());

    let output_json = serde_json::to_string_pretty(&results)
        .map_err(|e| anyhow!("Failed to serialize results: {}", e))?;

    if let Some(output_path) = &args.output {
        fs::write(output_path, &output_json)
            .map_err(|e| anyhow!("Failed to write output file: {}", e))?;
        eprintln!("Results written to {}", output_path.display());
    } else {
        println!("{}", output_json);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str, timestamp: u64, node_count: u32, priority: u32) -> TraceEntry {
        TraceEntry {
            allocation_id: id.to_string(),
            timestamp,
            node_count,
            priority,
            tenant: "tenant-a".to_string(),
            checkpoint: "auto".to_string(),
            data_readiness: 0.5,
            target_share: 0.1,
            actual_usage: 0.0,
        }
    }

    #[test]
    fn test_score_calculation() {
        let entry = make_entry("alloc-1", 1000, 10, 1);
        let weights = Weights::default();
        let base_time = 1000;
        let score = calculate_score(&entry, &weights, base_time);
        assert!(score > 0.0, "Score should be positive");
    }

    #[test]
    fn test_ranking_simple() {
        let trace = vec![
            make_entry("alloc-1", 1000, 10, 1),
            make_entry("alloc-2", 500, 20, 0),
        ];
        let weights = Weights::default();
        let results = simulate_simple(trace, weights).expect("Simulation should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[1].rank, 2);
        assert!(results[0].score >= results[1].score);
        assert!(results[0].breakdown.is_none());
    }

    #[test]
    fn test_default_weights() {
        let weights = Weights::default();
        assert_eq!(weights.priority, 1.0);
        assert_eq!(weights.wait_time, 0.8);
        assert_eq!(weights.fair_share, 0.7);
        assert_eq!(weights.topology, 0.6);
        assert_eq!(weights.data_readiness, 0.5);
        assert_eq!(weights.backlog, 0.4);
        assert_eq!(weights.energy, 0.3);
        assert_eq!(weights.checkpoint, 0.5);
        assert_eq!(weights.conformance, 0.4);
    }

    #[test]
    fn test_trace_to_allocation_basic() {
        let entry = make_entry("alloc-1", 1000, 4, 5);
        let alloc = trace_to_allocation(&entry);
        assert_eq!(alloc.tenant, "tenant-a");
        assert_eq!(alloc.lifecycle.preemption_class, 5);
        assert!(matches!(alloc.resources.nodes, NodeCount::Exact(4)));
        assert!(matches!(alloc.checkpoint, CheckpointStrategy::Auto));
        assert_eq!(alloc.state, AllocationState::Pending);
    }

    #[test]
    fn test_trace_to_allocation_checkpoint_manual() {
        let mut entry = make_entry("alloc-2", 2000, 1, 3);
        entry.checkpoint = "manual".to_string();
        let alloc = trace_to_allocation(&entry);
        assert!(matches!(alloc.checkpoint, CheckpointStrategy::Manual));
    }

    #[test]
    fn test_trace_to_allocation_checkpoint_none() {
        let mut entry = make_entry("alloc-3", 3000, 1, 1);
        entry.checkpoint = "none".to_string();
        let alloc = trace_to_allocation(&entry);
        assert!(matches!(alloc.checkpoint, CheckpointStrategy::None));
    }

    #[test]
    fn test_trace_to_allocation_priority_capped_at_10() {
        let entry = make_entry("alloc-4", 1000, 1, 99);
        let alloc = trace_to_allocation(&entry);
        assert_eq!(alloc.lifecycle.preemption_class, 10);
    }

    #[test]
    fn test_trace_to_allocation_timestamp() {
        let entry = make_entry("alloc-5", 1700000000, 1, 1);
        let alloc = trace_to_allocation(&entry);
        assert_eq!(alloc.created_at.timestamp(), 1700000000);
    }

    #[test]
    fn test_real_evaluator_produces_scores() {
        let trace = vec![
            make_entry("alloc-1", 1000, 4, 5),
            make_entry("alloc-2", 2000, 8, 3),
        ];
        let weights = Weights::default();
        let results =
            simulate_real(trace, weights, 0.5, 4).expect("Real simulation should succeed");
        assert_eq!(results.len(), 2);
        for result in &results {
            assert!(result.score >= 0.0, "Score should be non-negative");
            assert!(
                result.breakdown.is_some(),
                "Real mode should include breakdown"
            );
        }
    }

    #[test]
    fn test_real_evaluator_ranking() {
        let trace = vec![
            make_entry("alloc-1", 1000, 4, 8),
            make_entry("alloc-2", 2000, 8, 2),
        ];
        let weights = Weights {
            priority: 10.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint: 0.0,
            conformance: 0.0,
        };
        let results = simulate_real(trace, weights, 0.5, 4).expect("Simulation should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[1].rank, 2);
        assert!(results[0].score > results[1].score);
        assert_eq!(results[0].allocation_id, "alloc-1");
    }

    #[test]
    fn test_real_evaluator_score_breakdown_has_all_factors() {
        let trace = vec![make_entry("alloc-1", 1000, 4, 5)];
        let weights = Weights::default();
        let results = simulate_real(trace, weights, 0.3, 4).expect("Simulation should succeed");
        let breakdown = results[0].breakdown.as_ref().unwrap();
        assert!(breakdown.priority.is_finite());
        assert!(breakdown.wait_time.is_finite());
        assert!(breakdown.fair_share.is_finite());
        assert!(breakdown.topology.is_finite());
        assert!(breakdown.data_readiness.is_finite());
        assert!(breakdown.backlog.is_finite());
        assert!(breakdown.energy.is_finite());
        assert!(breakdown.checkpoint.is_finite());
    }

    #[test]
    fn test_real_evaluator_energy_price_affects_score() {
        let trace = vec![make_entry("alloc-1", 1000, 4, 5)];
        let weights = Weights {
            priority: 0.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 1.0,
            checkpoint: 0.0,
            conformance: 0.0,
        };
        let cheap = simulate_real(trace.clone(), weights.clone(), 0.1, 4).unwrap();
        let expensive = simulate_real(trace, weights, 0.9, 4).unwrap();
        assert!(cheap[0].score > expensive[0].score);
    }

    #[test]
    fn test_empty_trace_simple() {
        let trace = vec![];
        let weights = Weights::default();
        let results = simulate_simple(trace, weights).expect("Should handle empty trace");
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_trace_real() {
        let trace = vec![];
        let weights = Weights::default();
        let results = simulate_real(trace, weights, 0.5, 4).expect("Should handle empty trace");
        assert!(results.is_empty());
    }

    #[test]
    fn test_simple_mode_unchanged_behavior() {
        let entry = TraceEntry {
            allocation_id: "alloc-1".to_string(),
            timestamp: 1000,
            node_count: 10,
            priority: 1,
            tenant: "tenant-a".to_string(),
            checkpoint: "auto".to_string(),
            data_readiness: 0.5,
            target_share: 0.1,
            actual_usage: 0.0,
        };
        let weights = Weights::default();
        let base_time = 1000;
        let score = calculate_score(&entry, &weights, base_time);
        let wait_time_factor = 0.0;
        let priority_factor: f64 = 1.0;
        let node_factor: f64 = 10.0 / 100.0;
        let tenant_factor: f64 = 8.0;
        let mut expected: f64 = 0.0;
        expected += 1.0 * priority_factor;
        expected += 0.8 * wait_time_factor;
        expected += 0.7 * tenant_factor;
        expected += 0.6 * node_factor;
        expected += 0.5 * (node_factor * 0.5);
        expected += 0.4 * priority_factor.max(1.0);
        expected += 0.3 * (1.0 / (node_factor + 1.0));
        expected += 0.5 * (1.0 / (priority_factor + 1.0));
        expected += 0.4 * node_factor;
        assert!(
            (score - expected).abs() < 1e-10,
            "Score {} should match expected {}",
            score,
            expected
        );
    }

    #[test]
    fn test_weights_to_cost_weights_conversion() {
        let weights = Weights {
            priority: 0.2,
            wait_time: 0.3,
            fair_share: 0.4,
            topology: 0.5,
            data_readiness: 0.6,
            backlog: 0.7,
            energy: 0.8,
            checkpoint: 0.9,
            conformance: 1.0,
        };
        let cw: CostWeights = (&weights).into();
        assert!((cw.priority - 0.2).abs() < f64::EPSILON);
        assert!((cw.wait_time - 0.3).abs() < f64::EPSILON);
        assert!((cw.fair_share - 0.4).abs() < f64::EPSILON);
        assert!((cw.topology - 0.5).abs() < f64::EPSILON);
        assert!((cw.data_readiness - 0.6).abs() < f64::EPSILON);
        assert!((cw.backlog - 0.7).abs() < f64::EPSILON);
        assert!((cw.energy - 0.8).abs() < f64::EPSILON);
        assert!((cw.checkpoint_efficiency - 0.9).abs() < f64::EPSILON);
        assert!((cw.conformance - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_build_cost_context() {
        let trace = vec![
            make_entry("alloc-1", 1000, 4, 5),
            make_entry("alloc-2", 2000, 8, 3),
        ];
        let mut alloc_map: HashMap<String, Allocation> = HashMap::new();
        for entry in &trace {
            alloc_map.insert(entry.allocation_id.clone(), trace_to_allocation(entry));
        }
        let now = Utc.timestamp_opt(5000, 0).unwrap();
        let ctx = build_cost_context(&trace, &alloc_map, now, 0.3, 8);
        assert!((ctx.energy_price - 0.3).abs() < f64::EPSILON);
        assert_eq!(ctx.max_groups, 8);
        assert!(ctx.tenant_usage.contains_key("tenant-a"));
        assert_eq!(ctx.data_readiness.len(), 2);
        assert!(ctx.backlog.queued_gpu_hours > 0.0);
    }

    #[test]
    fn test_single_entry_trace_real() {
        let trace = vec![make_entry("solo", 1000, 1, 5)];
        let weights = Weights::default();
        let results = simulate_real(trace, weights, 0.5, 4).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[0].allocation_id, "solo");
    }

    #[test]
    fn test_single_entry_trace_simple() {
        let trace = vec![make_entry("solo", 1000, 1, 5)];
        let weights = Weights::default();
        let results = simulate_simple(trace, weights).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rank, 1);
    }
}

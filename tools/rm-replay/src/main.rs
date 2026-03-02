use anyhow::{anyhow, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
}

/// Scheduling cost function weights
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

/// Result of simulating an allocation's scheduling score
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SimulationResult {
    /// Allocation identifier
    allocation_id: String,
    /// Composite score from the cost function
    score: f64,
    /// Rank in the final ordering (1 = highest priority)
    rank: usize,
}

/// Calculate the composite score for an allocation using the cost function
///
/// The score combines multiple factors with weighted averaging:
/// Score(j) = Σ wᵢ · fᵢ(j)
fn calculate_score(entry: &TraceEntry, weights: &Weights, base_time: u64) -> f64 {
    let wait_time_factor = (base_time.saturating_sub(entry.timestamp)) as f64 / 3600.0; // Hours waited
    let priority_factor = entry.priority as f64;
    let node_factor = entry.node_count as f64 / 100.0; // Normalize node count
    let tenant_factor = entry.tenant.len() as f64; // Simple tenant affinity proxy

    let mut score = 0.0;
    score += weights.priority * priority_factor;
    score += weights.wait_time * wait_time_factor;
    score += weights.fair_share * tenant_factor;
    score += weights.topology * node_factor;
    score += weights.data_readiness * (node_factor * 0.5);
    score += weights.backlog * priority_factor.max(1.0);
    score += weights.energy * (1.0 / (node_factor + 1.0)); // Inverse energy efficiency
    score += weights.checkpoint * (1.0 / (priority_factor + 1.0));
    score += weights.conformance * node_factor;

    score
}

/// Load and parse a JSON trace file
fn load_trace(path: &PathBuf) -> Result<Vec<TraceEntry>> {
    let content = fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read trace file: {}", e))?;
    serde_json::from_str(&content)
        .map_err(|e| anyhow!("Failed to parse trace file: {}", e))
}

/// Load and parse a JSON weights file
fn load_weights(path: &PathBuf) -> Result<Weights> {
    let content = fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read weights file: {}", e))?;
    serde_json::from_str(&content)
        .map_err(|e| anyhow!("Failed to parse weights file: {}", e))
}

/// Run simulation with the provided trace and weights
fn simulate(trace: Vec<TraceEntry>, weights: Weights) -> Result<Vec<SimulationResult>> {
    if trace.is_empty() {
        return Err(anyhow!("Trace is empty"));
    }

    // Find the base time (first allocation timestamp)
    let base_time = trace.iter().map(|e| e.timestamp).min().unwrap_or(0);

    // Calculate scores for all allocations
    let mut results: Vec<SimulationResult> = trace
        .iter()
        .map(|entry| {
            let score = calculate_score(entry, &weights, base_time);
            SimulationResult {
                allocation_id: entry.allocation_id.clone(),
                score,
                rank: 0, // Will be set after sorting
            }
        })
        .collect();

    // Sort by score (descending) and assign ranks
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    for (idx, result) in results.iter_mut().enumerate() {
        result.rank = idx + 1;
    }

    Ok(results)
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Load trace
    let trace = load_trace(&args.input)?;
    eprintln!("Loaded {} allocations from trace", trace.len());

    // Load weights or use defaults
    let weights = if let Some(weights_path) = &args.weights {
        load_weights(weights_path)?
    } else {
        eprintln!("Using default weights");
        Weights::default()
    };

    // Run simulation
    let results = simulate(trace, weights)?;
    eprintln!("Simulation complete, scored {} allocations", results.len());

    // Serialize results
    let output_json = serde_json::to_string_pretty(&results)
        .map_err(|e| anyhow!("Failed to serialize results: {}", e))?;

    // Write output
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

    /// Test basic score calculation
    #[test]
    fn test_score_calculation() {
        let entry = TraceEntry {
            allocation_id: "alloc-1".to_string(),
            timestamp: 1000,
            node_count: 10,
            priority: 1,
            tenant: "tenant-a".to_string(),
        };
        let weights = Weights::default();
        let base_time = 1000;

        let score = calculate_score(&entry, &weights, base_time);
        assert!(score > 0.0, "Score should be positive");
    }

    /// Test ranking order (highest scores first)
    #[test]
    fn test_ranking() {
        let trace = vec![
            TraceEntry {
                allocation_id: "alloc-1".to_string(),
                timestamp: 1000,
                node_count: 10,
                priority: 1,
                tenant: "tenant-a".to_string(),
            },
            TraceEntry {
                allocation_id: "alloc-2".to_string(),
                timestamp: 500,
                node_count: 20,
                priority: 0,
                tenant: "tenant-b".to_string(),
            },
        ];
        let weights = Weights::default();

        let results = simulate(trace, weights).expect("Simulation should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[1].rank, 2);
        assert!(results[0].score >= results[1].score);
    }

    /// Test default weights
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
}

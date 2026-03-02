//! Checkpoint cost model.
//!
//! Implements `Should_checkpoint(j, t) = Value(j, t) > Cost(j, t)`
//! from docs/architecture/checkpoint-broker.md.

use chrono::{DateTime, Utc};

use lattice_common::types::*;

/// Parameters for checkpoint cost evaluation.
#[derive(Debug, Clone)]
pub struct CheckpointParams {
    /// GPU memory usage per node (bytes)
    pub gpu_memory_per_node_bytes: u64,
    /// GPUs per node
    pub gpus_per_node: u32,
    /// Storage write bandwidth (bytes/sec)
    pub storage_write_bandwidth: f64,
    /// Cost per GB on target storage tier
    pub cost_per_gb: f64,
    /// System-wide backlog pressure (0.0-1.0)
    pub backlog_pressure: f64,
    /// Number of waiting higher-priority jobs
    pub waiting_higher_priority_jobs: u32,
    /// Failure probability estimate for the node (0.0-1.0)
    pub failure_probability: f64,
    /// Current time
    pub now: DateTime<Utc>,
}

impl Default for CheckpointParams {
    fn default() -> Self {
        Self {
            gpu_memory_per_node_bytes: 80 * 1024 * 1024 * 1024, // 80 GB
            gpus_per_node: 4,
            storage_write_bandwidth: 10.0 * 1024.0 * 1024.0 * 1024.0, // 10 GB/s
            cost_per_gb: 0.02,
            backlog_pressure: 0.0,
            waiting_higher_priority_jobs: 0,
            failure_probability: 0.001,
            now: Utc::now(),
        }
    }
}

/// Result of checkpoint cost evaluation.
#[derive(Debug, Clone)]
pub struct CheckpointEvaluation {
    pub should_checkpoint: bool,
    pub cost: f64,
    pub value: f64,
}

/// Evaluate whether an allocation should checkpoint now.
pub fn evaluate_checkpoint(alloc: &Allocation, params: &CheckpointParams) -> CheckpointEvaluation {
    let cost = compute_cost(alloc, params);
    let value = compute_value(alloc, params);

    CheckpointEvaluation {
        should_checkpoint: value > cost,
        cost,
        value,
    }
}

/// Compute the total cost of checkpointing.
fn compute_cost(alloc: &Allocation, params: &CheckpointParams) -> f64 {
    let node_count = alloc.assigned_nodes.len() as f64;
    let checkpoint_size_bytes = params.gpu_memory_per_node_bytes as f64 * node_count;
    let checkpoint_size_gb = checkpoint_size_bytes / (1024.0 * 1024.0 * 1024.0);

    // write_time: checkpoint_size / storage_write_bandwidth (in seconds)
    let write_time_secs = if params.storage_write_bandwidth > 0.0 {
        checkpoint_size_bytes / params.storage_write_bandwidth
    } else {
        f64::INFINITY // Storage unavailable
    };

    // compute_waste: GPU-seconds lost during checkpoint
    let compute_waste = write_time_secs * node_count * params.gpus_per_node as f64;

    // storage_cost: checkpoint_size × cost_per_GB
    let storage_cost = checkpoint_size_gb * params.cost_per_gb;

    // Storage cost is in dollars — a minor additive term compared to
    // compute waste (GPU-seconds).  No normalization needed; cloud
    // storage is cheap relative to GPU time.
    write_time_secs + compute_waste + storage_cost
}

/// Compute the total value of checkpointing.
fn compute_value(alloc: &Allocation, params: &CheckpointParams) -> f64 {
    let node_count = alloc.assigned_nodes.len().max(1) as f64;

    // recompute_saved: time since last checkpoint × nodes × gpus × failure_probability
    let time_since_checkpoint = alloc
        .started_at
        .map(|s| (params.now - s).num_seconds().max(0) as f64)
        .unwrap_or(0.0);

    let recompute_saved = time_since_checkpoint
        * node_count
        * params.gpus_per_node as f64
        * params.failure_probability;

    // preemptability: value of being able to preempt this job
    let preemptability = params.waiting_higher_priority_jobs as f64
        * 100.0 // urgency weight per waiting job
        * (1.0 - alloc.lifecycle.preemption_class as f64 / 10.0); // lower class = more preemptable

    // backlog_relief: how much would freeing nodes help the queue
    let backlog_relief = params.backlog_pressure * node_count * 60.0; // scaled to seconds

    recompute_saved + preemptability + backlog_relief
}

/// Estimate checkpoint size in bytes for an allocation.
pub fn estimate_checkpoint_size(alloc: &Allocation, gpu_memory_per_node: u64) -> u64 {
    let node_count = alloc.assigned_nodes.len() as u64;
    gpu_memory_per_node * node_count
}

/// Estimate write time for a checkpoint in seconds.
pub fn estimate_write_time(checkpoint_size_bytes: u64, bandwidth_bytes_per_sec: f64) -> f64 {
    if bandwidth_bytes_per_sec <= 0.0 {
        return f64::INFINITY;
    }
    checkpoint_size_bytes as f64 / bandwidth_bytes_per_sec
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    fn running_alloc(nodes: usize) -> Allocation {
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = (0..nodes).map(|i| format!("n{i}")).collect();
        alloc.started_at = Some(Utc::now() - chrono::Duration::hours(2));
        alloc
    }

    #[test]
    fn no_checkpoint_when_quiet_system() {
        let alloc = running_alloc(2);
        let params = CheckpointParams {
            backlog_pressure: 0.0,
            waiting_higher_priority_jobs: 0,
            failure_probability: 0.001,
            ..Default::default()
        };

        let eval = evaluate_checkpoint(&alloc, &params);
        // Quiet system, low failure prob → cost should exceed value
        assert!(!eval.should_checkpoint);
    }

    #[test]
    fn checkpoint_when_high_backlog() {
        let alloc = running_alloc(4);
        let params = CheckpointParams {
            backlog_pressure: 0.9,
            waiting_higher_priority_jobs: 3,
            failure_probability: 0.01,
            ..Default::default()
        };

        let eval = evaluate_checkpoint(&alloc, &params);
        assert!(eval.should_checkpoint);
    }

    #[test]
    fn checkpoint_when_node_degrading() {
        let alloc = running_alloc(2);
        let params = CheckpointParams {
            backlog_pressure: 0.0,
            waiting_higher_priority_jobs: 0,
            failure_probability: 0.5, // high failure probability
            ..Default::default()
        };

        let eval = evaluate_checkpoint(&alloc, &params);
        assert!(eval.should_checkpoint);
    }

    #[test]
    fn no_checkpoint_when_storage_unavailable() {
        let alloc = running_alloc(2);
        let params = CheckpointParams {
            storage_write_bandwidth: 0.0, // storage down
            backlog_pressure: 0.5,
            waiting_higher_priority_jobs: 2,
            failure_probability: 0.01,
            ..Default::default()
        };

        let eval = evaluate_checkpoint(&alloc, &params);
        // Infinite cost → should not checkpoint
        assert!(!eval.should_checkpoint);
    }

    #[test]
    fn cost_increases_with_node_count() {
        let small = running_alloc(1);
        let large = running_alloc(8);
        let params = CheckpointParams::default();

        let eval_small = evaluate_checkpoint(&small, &params);
        let eval_large = evaluate_checkpoint(&large, &params);
        assert!(eval_large.cost > eval_small.cost);
    }

    #[test]
    fn value_increases_with_running_time() {
        let mut recent = running_alloc(2);
        recent.started_at = Some(Utc::now() - chrono::Duration::minutes(5));

        let mut long_running = running_alloc(2);
        long_running.started_at = Some(Utc::now() - chrono::Duration::hours(12));

        let params = CheckpointParams {
            failure_probability: 0.01,
            ..Default::default()
        };

        let eval_recent = evaluate_checkpoint(&recent, &params);
        let eval_long = evaluate_checkpoint(&long_running, &params);
        assert!(eval_long.value > eval_recent.value);
    }

    #[test]
    fn estimate_checkpoint_size_scales_with_nodes() {
        let alloc_small = running_alloc(1);
        let alloc_large = running_alloc(4);
        let gpu_mem = 80 * 1024 * 1024 * 1024; // 80 GB

        let small = estimate_checkpoint_size(&alloc_small, gpu_mem);
        let large = estimate_checkpoint_size(&alloc_large, gpu_mem);
        assert_eq!(large, small * 4);
    }

    #[test]
    fn estimate_write_time_basic() {
        let size = 80 * 1024 * 1024 * 1024; // 80 GB
        let bandwidth = 10.0 * 1024.0 * 1024.0 * 1024.0; // 10 GB/s
        let time = estimate_write_time(size, bandwidth);
        assert!((time - 8.0).abs() < 0.01); // 80 GB / 10 GB/s = 8 seconds
    }

    #[test]
    fn estimate_write_time_zero_bandwidth() {
        let time = estimate_write_time(100, 0.0);
        assert!(time.is_infinite());
    }
}

//! Prometheus metrics definitions for the Lattice scheduler.
//!
//! All metrics are registered in a global `Registry` and can be scraped
//! via the `/metrics` HTTP endpoint.

use prometheus::{
    register_counter_vec, register_gauge, register_gauge_vec, register_histogram,
    register_histogram_vec, CounterVec, Encoder, Gauge, GaugeVec, Histogram, HistogramVec,
    TextEncoder,
};
use std::sync::OnceLock;

// ─── API metrics ─────────────────────────────────────────────

fn api_requests_total() -> &'static CounterVec {
    static METRIC: OnceLock<CounterVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_counter_vec!(
            "lattice_api_requests_total",
            "Total API requests",
            &["method", "status"]
        )
        .unwrap()
    })
}

/// Record an API request with the given method and status.
pub fn record_api_request(method: &str, status: &str) {
    api_requests_total()
        .with_label_values(&[method, status])
        .inc();
}

fn api_request_duration_seconds() -> &'static HistogramVec {
    static METRIC: OnceLock<HistogramVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_histogram_vec!(
            "lattice_api_request_duration_seconds",
            "API request duration in seconds",
            &["method"],
            vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
        )
        .unwrap()
    })
}

/// Observe the duration of an API request.
pub fn observe_api_request_duration(method: &str, duration_secs: f64) {
    api_request_duration_seconds()
        .with_label_values(&[method])
        .observe(duration_secs);
}

fn api_active_streams() -> &'static GaugeVec {
    static METRIC: OnceLock<GaugeVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_gauge_vec!(
            "lattice_api_active_streams",
            "Number of active streaming connections",
            &["stream_type"]
        )
        .unwrap()
    })
}

/// Increment active streams for the given stream type.
pub fn inc_active_streams(stream_type: &str) {
    api_active_streams().with_label_values(&[stream_type]).inc();
}

/// Decrement active streams for the given stream type.
pub fn dec_active_streams(stream_type: &str) {
    api_active_streams().with_label_values(&[stream_type]).dec();
}

// ─── Scheduler metrics ──────────────────────────────────────

fn scheduling_cycle_duration_seconds() -> &'static HistogramVec {
    static METRIC: OnceLock<HistogramVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_histogram_vec!(
            "lattice_scheduling_cycle_duration_seconds",
            "Duration of a scheduling cycle in seconds",
            &["vcluster"],
            vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]
        )
        .unwrap()
    })
}

/// Observe the duration of a scheduling cycle for a vCluster.
pub fn observe_scheduling_cycle(vcluster: &str, duration_secs: f64) {
    scheduling_cycle_duration_seconds()
        .with_label_values(&[vcluster])
        .observe(duration_secs);
}

fn scheduling_queue_depth() -> &'static GaugeVec {
    static METRIC: OnceLock<GaugeVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_gauge_vec!(
            "lattice_scheduling_queue_depth",
            "Number of allocations waiting in the scheduling queue",
            &["vcluster"]
        )
        .unwrap()
    })
}

/// Set the scheduling queue depth for a vCluster.
pub fn set_scheduling_queue_depth(vcluster: &str, depth: f64) {
    scheduling_queue_depth()
        .with_label_values(&[vcluster])
        .set(depth);
}

fn scheduling_proposals_total() -> &'static CounterVec {
    static METRIC: OnceLock<CounterVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_counter_vec!(
            "lattice_scheduling_proposals_total",
            "Total scheduling proposals",
            &["vcluster", "result"]
        )
        .unwrap()
    })
}

/// Record a scheduling proposal result for a vCluster.
pub fn record_scheduling_proposal(vcluster: &str, result: &str) {
    scheduling_proposals_total()
        .with_label_values(&[vcluster, result])
        .inc();
}

// ─── Raft metrics ───────────────────────────────────────────

fn raft_leader() -> &'static Gauge {
    static METRIC: OnceLock<Gauge> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_gauge!(
            "lattice_raft_leader",
            "1 if this node is the Raft leader, 0 otherwise"
        )
        .unwrap()
    })
}

/// Set whether this node is the Raft leader (1.0 = leader, 0.0 = follower).
pub fn set_raft_leader(is_leader: bool) {
    raft_leader().set(if is_leader { 1.0 } else { 0.0 });
}

fn raft_commit_latency_seconds() -> &'static Histogram {
    static METRIC: OnceLock<Histogram> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_histogram!(
            "lattice_raft_commit_latency_seconds",
            "Raft commit latency in seconds",
            vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]
        )
        .unwrap()
    })
}

/// Observe a Raft commit latency.
pub fn observe_raft_commit_latency(duration_secs: f64) {
    raft_commit_latency_seconds().observe(duration_secs);
}

// ─── Agent metrics ──────────────────────────────────────────

fn agent_heartbeat_latency_seconds() -> &'static HistogramVec {
    static METRIC: OnceLock<HistogramVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_histogram_vec!(
            "lattice_agent_heartbeat_latency_seconds",
            "Heartbeat round-trip latency in seconds",
            &["node_id"],
            vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0]
        )
        .unwrap()
    })
}

/// Observe a heartbeat latency for a node agent.
pub fn observe_agent_heartbeat_latency(node_id: &str, duration_secs: f64) {
    agent_heartbeat_latency_seconds()
        .with_label_values(&[node_id])
        .observe(duration_secs);
}

fn agent_allocation_startup_seconds() -> &'static Histogram {
    static METRIC: OnceLock<Histogram> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_histogram!(
            "lattice_agent_allocation_startup_seconds",
            "Time to start an allocation on a node",
            vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0]
        )
        .unwrap()
    })
}

/// Observe the startup time of an allocation.
pub fn observe_agent_allocation_startup(duration_secs: f64) {
    agent_allocation_startup_seconds().observe(duration_secs);
}

// ─── Checkpoint metrics ─────────────────────────────────────

fn checkpoint_evaluations_total() -> &'static CounterVec {
    static METRIC: OnceLock<CounterVec> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_counter_vec!(
            "lattice_checkpoint_evaluations_total",
            "Total checkpoint evaluations",
            &["decision"]
        )
        .unwrap()
    })
}

/// Record a checkpoint evaluation decision ("checkpoint" or "skip").
pub fn record_checkpoint_evaluation(decision: &str) {
    checkpoint_evaluations_total()
        .with_label_values(&[decision])
        .inc();
}

// ─── Cluster metrics ────────────────────────────────────────

fn allocations_active() -> &'static Gauge {
    static METRIC: OnceLock<Gauge> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_gauge!(
            "lattice_allocations_active",
            "Number of currently active allocations"
        )
        .unwrap()
    })
}

/// Set the number of active allocations.
pub fn set_active_allocations(count: f64) {
    allocations_active().set(count);
}

fn nodes_ready() -> &'static Gauge {
    static METRIC: OnceLock<Gauge> = OnceLock::new();
    METRIC.get_or_init(|| {
        register_gauge!("lattice_nodes_ready", "Number of nodes in Ready state").unwrap()
    })
}

/// Set the number of ready nodes.
pub fn set_nodes_ready(count: f64) {
    nodes_ready().set(count);
}

// ─── Dispatch metrics (INV-D / DEC-DISP) ─────────────────────

fn dispatch_attempt_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_dispatch_attempt_total",
            "Total Dispatch Attempts to node agents (DEC-DISP-04)",
            &["node_id", "result"]
        )
        .unwrap()
    })
}
pub fn record_dispatch_attempt(node_id: &str, result: &str) {
    dispatch_attempt_total()
        .with_label_values(&[node_id, result])
        .inc();
}

fn dispatch_rollback_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_dispatch_rollback_total",
            "Total RollbackDispatch commits (INV-D6 / DEC-DISP-07)",
            &["reason"]
        )
        .unwrap()
    })
}
pub fn record_dispatch_rollback(reason: &str) {
    dispatch_rollback_total().with_label_values(&[reason]).inc();
}

fn dispatch_rollback_stop_sent_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_dispatch_rollback_stop_sent_total",
            "StopAllocation cleanup outcomes after rollback (DEC-DISP-08)",
            &["result"]
        )
        .unwrap()
    })
}
pub fn record_rollback_stop_sent(result: &str) {
    dispatch_rollback_stop_sent_total()
        .with_label_values(&[result])
        .inc();
}

fn completion_report_cross_node_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_completion_report_cross_node_total",
            "Completion Reports rejected because source node is not assigned (INV-D12)",
            &["node_id"]
        )
        .unwrap()
    })
}
pub fn record_cross_node_report(node_id: &str) {
    completion_report_cross_node_total()
        .with_label_values(&[node_id])
        .inc();
}

fn completion_report_phase_regression_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_completion_report_phase_regression_total",
            "Completion Reports rejected for phase regression (INV-D7)",
            &["current_phase", "reported_phase"]
        )
        .unwrap()
    })
}
pub fn record_phase_regression(current: &str, reported: &str) {
    completion_report_phase_regression_total()
        .with_label_values(&[current, reported])
        .inc();
}

fn dispatch_unknown_refusal_reason_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_dispatch_unknown_refusal_reason_total",
            "Unknown refusal_reason values received from agent (D-ADV-ARCH-10)",
            &["reason_code"]
        )
        .unwrap()
    })
}
pub fn record_unknown_refusal_reason(reason_code: &str) {
    dispatch_unknown_refusal_reason_total()
        .with_label_values(&[reason_code])
        .inc();
}

fn node_degraded_by_dispatch_total() -> &'static CounterVec {
    static M: OnceLock<CounterVec> = OnceLock::new();
    M.get_or_init(|| {
        register_counter_vec!(
            "lattice_node_degraded_by_dispatch_total",
            "Nodes transitioned to Degraded via INV-D11 cap",
            &["node_id"]
        )
        .unwrap()
    })
}
pub fn record_node_degraded(node_id: &str) {
    node_degraded_by_dispatch_total()
        .with_label_values(&[node_id])
        .inc();
}

// ─── Encoding ───────────────────────────────────────────────

/// Encode all registered metrics in Prometheus text exposition format.
pub fn encode_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_request_counter_increments() {
        record_api_request("GetAllocation", "ok");
        record_api_request("GetAllocation", "ok");
        record_api_request("GetAllocation", "error");

        let val = api_requests_total()
            .with_label_values(&["GetAllocation", "ok"])
            .get();
        assert!(val >= 2.0, "expected at least 2, got {val}");

        let val_err = api_requests_total()
            .with_label_values(&["GetAllocation", "error"])
            .get();
        assert!(val_err >= 1.0, "expected at least 1, got {val_err}");
    }

    #[test]
    fn scheduling_cycle_histogram_records() {
        observe_scheduling_cycle("hpc", 0.42);

        let count = scheduling_cycle_duration_seconds()
            .with_label_values(&["hpc"])
            .get_sample_count();
        assert!(count >= 1, "expected at least 1 observation, got {count}");
    }

    #[test]
    fn metrics_endpoint_returns_text() {
        // Ensure at least one metric is initialized.
        set_active_allocations(5.0);

        let output = encode_metrics();
        assert!(
            output.contains("lattice_allocations_active"),
            "expected metric name in output"
        );
    }

    #[test]
    fn active_allocations_gauge() {
        set_active_allocations(42.0);
        let val = allocations_active().get();
        assert!((val - 42.0).abs() < f64::EPSILON, "expected 42, got {val}");

        set_active_allocations(0.0);
        let val = allocations_active().get();
        assert!((val - 0.0).abs() < f64::EPSILON, "expected 0, got {val}");
    }

    #[test]
    fn checkpoint_evaluation_counter() {
        record_checkpoint_evaluation("checkpoint");
        record_checkpoint_evaluation("checkpoint");
        record_checkpoint_evaluation("skip");

        let ckpt = checkpoint_evaluations_total()
            .with_label_values(&["checkpoint"])
            .get();
        assert!(ckpt >= 2.0, "expected at least 2, got {ckpt}");

        let skip = checkpoint_evaluations_total()
            .with_label_values(&["skip"])
            .get();
        assert!(skip >= 1.0, "expected at least 1, got {skip}");
    }

    #[test]
    fn all_metric_names_follow_convention() {
        // Initialize all metrics so they are registered.
        record_api_request("_test", "_test");
        observe_api_request_duration("_test", 0.001);
        inc_active_streams("_test");
        observe_scheduling_cycle("_test", 0.001);
        set_scheduling_queue_depth("_test", 0.0);
        record_scheduling_proposal("_test", "_test");
        set_raft_leader(false);
        observe_raft_commit_latency(0.001);
        observe_agent_heartbeat_latency("_test", 0.001);
        observe_agent_allocation_startup(0.001);
        record_checkpoint_evaluation("_test");
        set_active_allocations(0.0);
        set_nodes_ready(0.0);

        let families = prometheus::gather();
        for f in &families {
            let name = f.name();
            // Only check our metrics (skip any from other crates/tests).
            if name.starts_with("lattice_") {
                assert!(
                    name.starts_with("lattice_"),
                    "metric {name} does not start with lattice_"
                );
            }
        }
        // Make sure we found at least our 13 metrics.
        let lattice_count = families
            .iter()
            .filter(|f| f.name().starts_with("lattice_"))
            .count();
        assert!(
            lattice_count >= 13,
            "expected at least 13 lattice_ metrics, got {lattice_count}"
        );
    }

    #[test]
    fn queue_depth_gauge_updates() {
        set_scheduling_queue_depth("interactive", 10.0);
        let val = scheduling_queue_depth()
            .with_label_values(&["interactive"])
            .get();
        assert!((val - 10.0).abs() < f64::EPSILON, "expected 10, got {val}");

        set_scheduling_queue_depth("interactive", 3.0);
        let val = scheduling_queue_depth()
            .with_label_values(&["interactive"])
            .get();
        assert!((val - 3.0).abs() < f64::EPSILON, "expected 3, got {val}");
    }

    #[test]
    fn raft_leader_gauge() {
        set_raft_leader(true);
        let val = raft_leader().get();
        assert!(
            (val - 1.0).abs() < f64::EPSILON,
            "expected 1.0 for leader, got {val}"
        );

        set_raft_leader(false);
        let val = raft_leader().get();
        assert!(
            (val - 0.0).abs() < f64::EPSILON,
            "expected 0.0 for follower, got {val}"
        );
    }
}

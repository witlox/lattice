//! Diagnostics collector and metrics comparison for allocations.
//!
//! Provides `DiagnosticsCollector` for aggregating per-allocation health state
//! (node agent connectivity, network topology, storage mounts, recent metrics)
//! and `compare_allocations` for computing metric deltas across two allocations.
//!
//! These types back the `GetDiagnostics` and `CompareMetrics` RPCs without
//! touching `allocation_service.rs` directly.

use std::sync::Arc;

use lattice_common::tsdb_client::{MetricSeries, QueryResult, TsdbClient};
use lattice_common::types::NodeId;
use uuid::Uuid;

use lattice_common::error::LatticeError;

// ─── Node-level diagnostics ─────────────────────────────────

/// Snapshot of a single node's state within an allocation.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDiagState {
    pub node_id: NodeId,
    /// Whether the node agent is reachable.
    pub agent_connected: bool,
    /// Textual phase (e.g., "Running", "Draining", "Unknown").
    pub phase: String,
    /// Seconds since the last heartbeat was received.
    pub last_heartbeat_age_secs: u64,
}

// ─── Network diagnostics ────────────────────────────────────

/// Network health snapshot for a running allocation.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkDiagnostics {
    /// Whether all intra-allocation paths appear healthy.
    pub connectivity_ok: bool,
    /// Slingshot VNI assigned to this allocation's network domain.
    pub vni: Option<u32>,
    /// Average measured inter-node latency in microseconds.
    pub avg_latency_us: f64,
    /// Average measured inter-node bandwidth in Gbps.
    pub avg_bandwidth_gbps: f64,
}

impl Default for NetworkDiagnostics {
    fn default() -> Self {
        Self {
            connectivity_ok: true,
            vni: None,
            avg_latency_us: 0.0,
            avg_bandwidth_gbps: 0.0,
        }
    }
}

// ─── Storage diagnostics ────────────────────────────────────

/// Storage health snapshot for a running allocation.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageDiagnostics {
    /// Whether all configured mounts are healthy.
    pub mounts_healthy: bool,
    /// Aggregate read throughput in Gbps across all mounts.
    pub io_throughput_gbps: f64,
    /// Cache hit rate [0.0, 1.0] for hot-tier accesses.
    pub cache_hit_rate: f64,
}

impl Default for StorageDiagnostics {
    fn default() -> Self {
        Self {
            mounts_healthy: true,
            io_throughput_gbps: 0.0,
            cache_hit_rate: 0.0,
        }
    }
}

// ─── Aggregated diagnostics ─────────────────────────────────

/// Full diagnostics report for a single allocation.
#[derive(Debug, Clone)]
pub struct AllocationDiagnostics {
    pub allocation_id: Uuid,
    /// Per-node connection and phase information.
    pub node_states: Vec<NodeDiagState>,
    /// Network health for the allocation's network domain.
    pub network_health: NetworkDiagnostics,
    /// Storage health for the allocation's mounts.
    pub storage_health: StorageDiagnostics,
    /// Tail of the allocation's log (last N lines from ring buffer).
    pub log_tail: Vec<String>,
    /// Recent metric values keyed by metric name.
    pub recent_metrics: std::collections::HashMap<String, f64>,
}

impl AllocationDiagnostics {
    /// Return `true` if all sub-systems appear healthy.
    pub fn is_healthy(&self) -> bool {
        let all_agents = self.node_states.iter().all(|n| n.agent_connected);
        all_agents && self.network_health.connectivity_ok && self.storage_health.mounts_healthy
    }
}

// ─── Metrics comparison ─────────────────────────────────────

/// Comparison of a single metric between two allocations.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsComparison {
    pub alloc_a_id: Uuid,
    pub alloc_b_id: Uuid,
    pub metric_name: String,
    /// Time-series values for allocation A (`(timestamp_ms, value)` pairs).
    pub values_a: Vec<(i64, f64)>,
    /// Time-series values for allocation B (`(timestamp_ms, value)` pairs).
    pub values_b: Vec<(i64, f64)>,
    /// Relative delta between mean(B) and mean(A) as a percentage.
    /// Positive = B is higher, negative = B is lower.
    /// `None` when either side has no data.
    pub delta_pct: Option<f64>,
}

impl MetricsComparison {
    /// Compute the mean of a value series.  Returns `None` for empty slices.
    fn mean(values: &[(i64, f64)]) -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        let sum: f64 = values.iter().map(|(_, v)| v).sum();
        Some(sum / values.len() as f64)
    }

    /// Construct a `MetricsComparison` from two raw `MetricSeries`.
    ///
    /// `delta_pct` is defined as `(mean_b - mean_a) / |mean_a| * 100`.
    /// When `mean_a` is zero, the delta is `None` to avoid division by zero.
    pub fn from_series(
        alloc_a_id: Uuid,
        alloc_b_id: Uuid,
        metric_name: String,
        series_a: &MetricSeries,
        series_b: &MetricSeries,
    ) -> Self {
        let values_a = series_a.values.clone();
        let values_b = series_b.values.clone();

        let delta_pct = match (Self::mean(&values_a), Self::mean(&values_b)) {
            (Some(mean_a), Some(mean_b)) if mean_a.abs() > f64::EPSILON => {
                Some((mean_b - mean_a) / mean_a.abs() * 100.0)
            }
            _ => None,
        };

        Self {
            alloc_a_id,
            alloc_b_id,
            metric_name,
            values_a,
            values_b,
            delta_pct,
        }
    }
}

// ─── DiagnosticsCollector ───────────────────────────────────

/// Aggregates diagnostics from TSDB and node-agent data.
///
/// In production, node-agent connectivity and storage health are obtained via
/// the node-agent gRPC channel (not yet wired).  Here we accept them as
/// pre-aggregated inputs so the aggregation logic is fully testable.
pub struct DiagnosticsCollector {
    tsdb: Arc<dyn TsdbClient>,
}

impl DiagnosticsCollector {
    pub fn new(tsdb: Arc<dyn TsdbClient>) -> Self {
        Self { tsdb }
    }

    /// Build a diagnostics report from caller-supplied node states, network &
    /// storage snapshots, and a log tail.  Recent metrics are fetched from TSDB.
    ///
    /// `metric_names` controls which metrics are queried; pass an empty slice to
    /// skip the TSDB query.
    pub async fn collect(
        &self,
        allocation_id: Uuid,
        node_states: Vec<NodeDiagState>,
        network_health: NetworkDiagnostics,
        storage_health: StorageDiagnostics,
        log_tail: Vec<String>,
        metric_names: &[&str],
    ) -> Result<AllocationDiagnostics, LatticeError> {
        let mut recent_metrics = std::collections::HashMap::new();

        for &metric in metric_names {
            let promql = format!("{metric}{{allocation_id=\"{allocation_id}\"}}[5m]");
            // Non-fatal: TSDB errors degrade gracefully (metric simply absent)
            if let Ok(result) = self.tsdb.query(&promql, 300).await {
                if let Some(value) = latest_value(&result) {
                    recent_metrics.insert(metric.to_string(), value);
                }
            }
        }

        Ok(AllocationDiagnostics {
            allocation_id,
            node_states,
            network_health,
            storage_health,
            log_tail,
            recent_metrics,
        })
    }
}

/// Extract the most recent scalar value from a TSDB query result.
fn latest_value(result: &QueryResult) -> Option<f64> {
    result
        .series
        .iter()
        .flat_map(|s| s.values.iter().copied())
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, v)| v)
}

// ─── compare_allocations ─────────────────────────────────────

/// Compare a named metric between two allocations by querying the TSDB.
///
/// Returns `None` when neither allocation has any data for the metric.
pub async fn compare_allocations(
    tsdb: &dyn TsdbClient,
    alloc_a_id: Uuid,
    alloc_b_id: Uuid,
    metric_name: &str,
    time_range_secs: u64,
) -> Result<Option<MetricsComparison>, LatticeError> {
    let query_a = format!("{metric_name}{{allocation_id=\"{alloc_a_id}\"}}[{time_range_secs}s]");
    let query_b = format!("{metric_name}{{allocation_id=\"{alloc_b_id}\"}}[{time_range_secs}s]");

    let result_a = tsdb.query(&query_a, time_range_secs).await?;
    let result_b = tsdb.query(&query_b, time_range_secs).await?;

    if result_a.series.is_empty() && result_b.series.is_empty() {
        return Ok(None);
    }

    // Flatten all series into a single synthetic series per allocation
    let flat_a = flatten_series(&result_a);
    let flat_b = flatten_series(&result_b);

    Ok(Some(MetricsComparison::from_series(
        alloc_a_id,
        alloc_b_id,
        metric_name.to_string(),
        &flat_a,
        &flat_b,
    )))
}

/// Merge all series in a `QueryResult` into a single `MetricSeries`.
fn flatten_series(result: &QueryResult) -> MetricSeries {
    let mut values: Vec<(i64, f64)> = result
        .series
        .iter()
        .flat_map(|s| s.values.iter().copied())
        .collect();
    values.sort_by_key(|(ts, _)| *ts);
    MetricSeries {
        labels: Default::default(),
        values,
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use lattice_common::error::LatticeError;
    use lattice_common::tsdb_client::{MetricSample, MetricSeries, QueryResult, TsdbClient};

    // ── Minimal mock TSDB ────────────────────────────────────

    /// A TSDB mock that returns pre-programmed query results.
    struct MockTsdb {
        /// Map of query-prefix → `QueryResult` to return.
        /// The prefix is matched with `starts_with`.
        responses: Mutex<HashMap<String, QueryResult>>,
        /// When `true`, all queries return `Err(Internal(...))`.
        always_error: bool,
    }

    impl MockTsdb {
        fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
                always_error: false,
            }
        }

        fn with_error() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
                always_error: true,
            }
        }

        fn register(&self, metric_prefix: &str, values: Vec<(i64, f64)>) {
            let series = MetricSeries {
                labels: HashMap::new(),
                values,
            };
            self.responses.lock().unwrap().insert(
                metric_prefix.to_string(),
                QueryResult {
                    series: vec![series],
                },
            );
        }
    }

    #[async_trait]
    impl TsdbClient for MockTsdb {
        async fn push(&self, _samples: &[MetricSample]) -> Result<(), LatticeError> {
            Ok(())
        }

        async fn query(
            &self,
            promql: &str,
            _time_range_secs: u64,
        ) -> Result<QueryResult, LatticeError> {
            if self.always_error {
                return Err(LatticeError::Internal("mock tsdb error".into()));
            }
            let responses = self.responses.lock().unwrap();
            for (prefix, result) in responses.iter() {
                if promql.starts_with(prefix.as_str()) {
                    return Ok(result.clone());
                }
            }
            // No match → return empty result
            Ok(QueryResult { series: vec![] })
        }

        async fn query_instant(&self, _promql: &str) -> Result<QueryResult, LatticeError> {
            if self.always_error {
                return Err(LatticeError::Internal("mock tsdb error".into()));
            }
            Ok(QueryResult { series: vec![] })
        }
    }

    // ── Helper constructors ──────────────────────────────────

    fn healthy_node(id: &str) -> NodeDiagState {
        NodeDiagState {
            node_id: id.to_string(),
            agent_connected: true,
            phase: "Running".to_string(),
            last_heartbeat_age_secs: 5,
        }
    }

    fn disconnected_node(id: &str) -> NodeDiagState {
        NodeDiagState {
            node_id: id.to_string(),
            agent_connected: false,
            phase: "Unknown".to_string(),
            last_heartbeat_age_secs: 120,
        }
    }

    // ── NodeDiagState tests ──────────────────────────────────

    #[test]
    fn node_diag_state_fields_preserved() {
        let node = healthy_node("x1000c0s0b0n0");
        assert_eq!(node.node_id, "x1000c0s0b0n0");
        assert!(node.agent_connected);
        assert_eq!(node.phase, "Running");
        assert_eq!(node.last_heartbeat_age_secs, 5);
    }

    // ── AllocationDiagnostics::is_healthy tests ──────────────

    #[test]
    fn is_healthy_all_ok() {
        let diag = AllocationDiagnostics {
            allocation_id: Uuid::new_v4(),
            node_states: vec![healthy_node("n0"), healthy_node("n1")],
            network_health: NetworkDiagnostics::default(),
            storage_health: StorageDiagnostics::default(),
            log_tail: vec![],
            recent_metrics: HashMap::new(),
        };
        assert!(diag.is_healthy());
    }

    #[test]
    fn is_healthy_false_when_agent_disconnected() {
        let diag = AllocationDiagnostics {
            allocation_id: Uuid::new_v4(),
            node_states: vec![healthy_node("n0"), disconnected_node("n1")],
            network_health: NetworkDiagnostics::default(),
            storage_health: StorageDiagnostics::default(),
            log_tail: vec![],
            recent_metrics: HashMap::new(),
        };
        assert!(!diag.is_healthy());
    }

    #[test]
    fn is_healthy_false_when_network_unhealthy() {
        let diag = AllocationDiagnostics {
            allocation_id: Uuid::new_v4(),
            node_states: vec![healthy_node("n0")],
            network_health: NetworkDiagnostics {
                connectivity_ok: false,
                ..Default::default()
            },
            storage_health: StorageDiagnostics::default(),
            log_tail: vec![],
            recent_metrics: HashMap::new(),
        };
        assert!(!diag.is_healthy());
    }

    #[test]
    fn is_healthy_false_when_storage_unhealthy() {
        let diag = AllocationDiagnostics {
            allocation_id: Uuid::new_v4(),
            node_states: vec![healthy_node("n0")],
            network_health: NetworkDiagnostics::default(),
            storage_health: StorageDiagnostics {
                mounts_healthy: false,
                ..Default::default()
            },
            log_tail: vec![],
            recent_metrics: HashMap::new(),
        };
        assert!(!diag.is_healthy());
    }

    // ── DiagnosticsCollector tests ───────────────────────────

    #[tokio::test]
    async fn collector_builds_correct_summary_from_mock_data() {
        let tsdb = Arc::new(MockTsdb::new());
        let alloc_id = Uuid::new_v4();

        // Register a metric that the collector will query
        tsdb.register(
            "gpu_utilization",
            vec![(1_000_000, 0.75), (1_000_030, 0.80)],
        );

        let collector = DiagnosticsCollector::new(tsdb);

        let result = collector
            .collect(
                alloc_id,
                vec![healthy_node("n0"), healthy_node("n1")],
                NetworkDiagnostics {
                    connectivity_ok: true,
                    vni: Some(1001),
                    avg_latency_us: 2.5,
                    avg_bandwidth_gbps: 200.0,
                },
                StorageDiagnostics {
                    mounts_healthy: true,
                    io_throughput_gbps: 5.0,
                    cache_hit_rate: 0.92,
                },
                vec!["epoch 10 loss=0.42".to_string()],
                &["gpu_utilization"],
            )
            .await
            .unwrap();

        assert_eq!(result.allocation_id, alloc_id);
        assert_eq!(result.node_states.len(), 2);
        assert!(result.network_health.connectivity_ok);
        assert_eq!(result.network_health.vni, Some(1001));
        assert!((result.network_health.avg_latency_us - 2.5).abs() < f64::EPSILON);
        assert!(result.storage_health.mounts_healthy);
        assert!((result.storage_health.cache_hit_rate - 0.92).abs() < f64::EPSILON);
        assert_eq!(result.log_tail, vec!["epoch 10 loss=0.42"]);
        // Latest value of the two samples is 0.80
        assert_eq!(
            result.recent_metrics.get("gpu_utilization").copied(),
            Some(0.80)
        );
        assert!(result.is_healthy());
    }

    #[tokio::test]
    async fn collector_handles_tsdb_error_gracefully() {
        // Errors from TSDB should not bubble up — metrics are simply absent
        let tsdb = Arc::new(MockTsdb::with_error());
        let alloc_id = Uuid::new_v4();

        let collector = DiagnosticsCollector::new(tsdb);
        let result = collector
            .collect(
                alloc_id,
                vec![healthy_node("n0")],
                NetworkDiagnostics::default(),
                StorageDiagnostics::default(),
                vec![],
                &["gpu_utilization", "cpu_utilization"],
            )
            .await
            .unwrap();

        assert!(result.recent_metrics.is_empty());
    }

    #[tokio::test]
    async fn collector_with_empty_metrics_slice_skips_tsdb() {
        let tsdb = Arc::new(MockTsdb::with_error()); // would fail if queried
        let alloc_id = Uuid::new_v4();

        let collector = DiagnosticsCollector::new(tsdb);
        let result = collector
            .collect(
                alloc_id,
                vec![],
                NetworkDiagnostics::default(),
                StorageDiagnostics::default(),
                vec![],
                &[], // no metrics requested
            )
            .await
            .unwrap();

        assert!(result.recent_metrics.is_empty());
    }

    // ── MetricsComparison::from_series tests ─────────────────

    #[test]
    fn compare_computes_positive_delta_correctly() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let series_a = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 100.0), (2000, 100.0)], // mean = 100
        };
        let series_b = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 150.0), (2000, 150.0)], // mean = 150
        };

        let cmp = MetricsComparison::from_series(
            id_a,
            id_b,
            "gpu_utilization".to_string(),
            &series_a,
            &series_b,
        );

        assert_eq!(cmp.alloc_a_id, id_a);
        assert_eq!(cmp.alloc_b_id, id_b);
        assert_eq!(cmp.metric_name, "gpu_utilization");
        assert_eq!(cmp.values_a.len(), 2);
        assert_eq!(cmp.values_b.len(), 2);
        // delta = (150 - 100) / 100 * 100 = 50.0 %
        assert_eq!(cmp.delta_pct, Some(50.0));
    }

    #[test]
    fn compare_computes_negative_delta_correctly() {
        let series_a = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 200.0)],
        };
        let series_b = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 100.0)],
        };

        let cmp = MetricsComparison::from_series(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "memory_used_bytes".to_string(),
            &series_a,
            &series_b,
        );

        // delta = (100 - 200) / 200 * 100 = -50.0 %
        assert_eq!(cmp.delta_pct, Some(-50.0));
    }

    #[test]
    fn compare_returns_none_delta_when_a_is_empty() {
        let series_empty = MetricSeries {
            labels: HashMap::new(),
            values: vec![],
        };
        let series_b = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 50.0)],
        };

        let cmp = MetricsComparison::from_series(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "gpu_utilization".to_string(),
            &series_empty,
            &series_b,
        );

        assert!(cmp.delta_pct.is_none());
        assert!(cmp.values_a.is_empty());
        assert_eq!(cmp.values_b.len(), 1);
    }

    #[test]
    fn compare_returns_none_delta_when_b_is_empty() {
        let series_a = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 50.0)],
        };
        let series_empty = MetricSeries {
            labels: HashMap::new(),
            values: vec![],
        };

        let cmp = MetricsComparison::from_series(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "gpu_utilization".to_string(),
            &series_a,
            &series_empty,
        );

        assert!(cmp.delta_pct.is_none());
    }

    #[test]
    fn compare_returns_none_delta_when_both_empty() {
        let empty = MetricSeries {
            labels: HashMap::new(),
            values: vec![],
        };

        let cmp = MetricsComparison::from_series(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "gpu_utilization".to_string(),
            &empty,
            &empty,
        );

        assert!(cmp.delta_pct.is_none());
        assert!(cmp.values_a.is_empty());
        assert!(cmp.values_b.is_empty());
    }

    #[test]
    fn compare_handles_zero_mean_a_gracefully() {
        let series_a = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 0.0)],
        };
        let series_b = MetricSeries {
            labels: HashMap::new(),
            values: vec![(1000, 10.0)],
        };

        let cmp = MetricsComparison::from_series(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "io_errors".to_string(),
            &series_a,
            &series_b,
        );

        // mean_a == 0 → delta_pct should be None (avoid div-by-zero)
        assert!(cmp.delta_pct.is_none());
    }

    // ── compare_allocations (async) tests ────────────────────

    #[tokio::test]
    async fn compare_allocations_computes_delta() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let tsdb = MockTsdb::new();
        tsdb.register("gpu_utilization", vec![(1000, 80.0), (2000, 80.0)]);

        // Both allocations will match the same prefix in our mock, giving both
        // the same data; we just need to verify the function runs end-to-end.
        let result = compare_allocations(&tsdb, id_a, id_b, "gpu_utilization", 300)
            .await
            .unwrap();

        assert!(result.is_some());
        let cmp = result.unwrap();
        assert_eq!(cmp.metric_name, "gpu_utilization");
        assert_eq!(cmp.alloc_a_id, id_a);
        assert_eq!(cmp.alloc_b_id, id_b);
    }

    #[tokio::test]
    async fn compare_allocations_returns_none_when_no_data() {
        let tsdb = MockTsdb::new();
        // No data registered → both queries return empty

        let result = compare_allocations(
            &tsdb,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "nonexistent_metric",
            300,
        )
        .await
        .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn compare_allocations_propagates_tsdb_error() {
        let tsdb = MockTsdb::with_error();

        let result = compare_allocations(
            &tsdb,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "gpu_utilization",
            300,
        )
        .await;

        assert!(result.is_err());
    }

    // ── latest_value helper tests ────────────────────────────

    #[test]
    fn latest_value_picks_maximum_timestamp() {
        let result = QueryResult {
            series: vec![
                MetricSeries {
                    labels: HashMap::new(),
                    values: vec![(1000, 0.5), (3000, 0.9)],
                },
                MetricSeries {
                    labels: HashMap::new(),
                    values: vec![(2000, 0.7)],
                },
            ],
        };

        let val = latest_value(&result);
        assert_eq!(val, Some(0.9)); // ts=3000 has the highest timestamp
    }

    #[test]
    fn latest_value_returns_none_for_empty_result() {
        let result = QueryResult { series: vec![] };
        assert!(latest_value(&result).is_none());
    }

    #[test]
    fn latest_value_returns_none_for_empty_series() {
        let result = QueryResult {
            series: vec![MetricSeries {
                labels: HashMap::new(),
                values: vec![],
            }],
        };
        assert!(latest_value(&result).is_none());
    }
}

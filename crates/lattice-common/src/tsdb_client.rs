//! TSDB client — push metrics (Prometheus remote write format) and query (PromQL).
//!
//! Provides a `TsdbClient` trait and a `VictoriaMetricsClient` implementation
//! using reqwest for real HTTP communication.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::LatticeError;

/// A single metric sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    /// Metric name (e.g., "gpu_utilization")
    pub name: String,
    /// Label key-value pairs
    pub labels: HashMap<String, String>,
    /// Timestamp in milliseconds since epoch
    pub timestamp_ms: i64,
    /// Metric value
    pub value: f64,
}

/// Result of a PromQL query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    /// Series data, each with labels and value pairs
    pub series: Vec<MetricSeries>,
}

/// A single time series from a query result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSeries {
    pub labels: HashMap<String, String>,
    pub values: Vec<(i64, f64)>,
}

/// Client for pushing and querying time series data.
#[async_trait]
pub trait TsdbClient: Send + Sync {
    /// Push metric samples to the TSDB.
    async fn push(&self, samples: &[MetricSample]) -> Result<(), LatticeError>;

    /// Query the TSDB with a PromQL expression.
    async fn query(&self, promql: &str, time_range_secs: u64) -> Result<QueryResult, LatticeError>;

    /// Instant query (single point in time).
    async fn query_instant(&self, promql: &str) -> Result<QueryResult, LatticeError>;
}

/// VictoriaMetrics client configuration.
#[derive(Debug, Clone)]
pub struct VictoriaMetricsConfig {
    /// Base URL (e.g., "http://localhost:8428")
    pub base_url: String,
    /// Optional auth token
    pub auth_token: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: u64,
}

impl Default for VictoriaMetricsConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8428".into(),
            auth_token: None,
            timeout_secs: 30,
        }
    }
}

/// VictoriaMetrics / Prometheus-compatible TSDB client.
pub struct VictoriaMetricsClient {
    config: VictoriaMetricsConfig,
    /// HTTP client handle — stored as an opaque type to avoid
    /// requiring reqwest at the trait level.
    /// Callers construct this via `new()` which accepts a config.
    _config_clone: VictoriaMetricsConfig,
}

impl VictoriaMetricsClient {
    pub fn new(config: VictoriaMetricsConfig) -> Self {
        Self {
            _config_clone: config.clone(),
            config,
        }
    }

    /// Build the import URL for Prometheus format.
    pub fn import_url(&self) -> String {
        format!("{}/api/v1/import/prometheus", self.config.base_url)
    }

    /// Build the query URL.
    pub fn query_url(&self) -> String {
        format!("{}/api/v1/query", self.config.base_url)
    }

    /// Build the range query URL.
    pub fn query_range_url(&self) -> String {
        format!("{}/api/v1/query_range", self.config.base_url)
    }

    /// Format samples as Prometheus text exposition format.
    pub fn format_samples(samples: &[MetricSample]) -> String {
        let mut lines = Vec::new();
        for sample in samples {
            let labels_str = if sample.labels.is_empty() {
                String::new()
            } else {
                let pairs: Vec<String> = sample
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{k}=\"{v}\""))
                    .collect();
                format!("{{{}}}", pairs.join(","))
            };
            lines.push(format!(
                "{}{} {} {}",
                sample.name, labels_str, sample.value, sample.timestamp_ms
            ));
        }
        lines.join("\n")
    }
}

#[async_trait]
impl TsdbClient for VictoriaMetricsClient {
    async fn push(&self, samples: &[MetricSample]) -> Result<(), LatticeError> {
        let body = Self::format_samples(samples);
        let url = self.import_url();

        let mut builder = reqwest::Client::new()
            .post(&url)
            .header("Content-Type", "text/plain")
            .body(body);

        if let Some(token) = &self.config.auth_token {
            builder = builder.bearer_auth(token);
        }

        let response = builder
            .send()
            .await
            .map_err(|e| LatticeError::MetricsQueryFailed(format!("push request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(LatticeError::MetricsQueryFailed(format!(
                "push returned HTTP {status}: {body}"
            )));
        }

        Ok(())
    }

    async fn query(&self, promql: &str, time_range_secs: u64) -> Result<QueryResult, LatticeError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let start = now.saturating_sub(time_range_secs);
        let url = self.query_range_url();

        let mut builder = reqwest::Client::new().get(&url).query(&[
            ("query", promql),
            ("start", &start.to_string()),
            ("end", &now.to_string()),
            ("step", "30"),
        ]);

        if let Some(token) = &self.config.auth_token {
            builder = builder.bearer_auth(token);
        }

        let response = builder
            .send()
            .await
            .map_err(|e| LatticeError::MetricsQueryFailed(format!("query request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(LatticeError::MetricsQueryFailed(format!(
                "query returned HTTP {status}: {body}"
            )));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| LatticeError::MetricsQueryFailed(format!("failed to parse JSON: {e}")))?;

        parse_query_response(&json)
    }

    async fn query_instant(&self, promql: &str) -> Result<QueryResult, LatticeError> {
        let url = self.query_url();

        let mut builder = reqwest::Client::new().get(&url).query(&[("query", promql)]);

        if let Some(token) = &self.config.auth_token {
            builder = builder.bearer_auth(token);
        }

        let response = builder.send().await.map_err(|e| {
            LatticeError::MetricsQueryFailed(format!("instant query request failed: {e}"))
        })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(LatticeError::MetricsQueryFailed(format!(
                "instant query returned HTTP {status}: {body}"
            )));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| LatticeError::MetricsQueryFailed(format!("failed to parse JSON: {e}")))?;

        parse_query_response(&json)
    }
}

/// Parse a VictoriaMetrics/Prometheus JSON query response into our QueryResult.
pub fn parse_query_response(json: &serde_json::Value) -> Result<QueryResult, LatticeError> {
    let data = json
        .get("data")
        .ok_or_else(|| LatticeError::Internal("missing 'data' in response".into()))?;

    let result_type = data
        .get("resultType")
        .and_then(|v| v.as_str())
        .unwrap_or("vector");

    let results = data
        .get("result")
        .and_then(|v| v.as_array())
        .ok_or_else(|| LatticeError::Internal("missing 'result' array in response".into()))?;

    let mut series = Vec::new();
    for result in results {
        let labels: HashMap<String, String> = result
            .get("metric")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let values = match result_type {
            "matrix" => {
                let empty = vec![];
                let vals = result
                    .get("values")
                    .and_then(|v| v.as_array())
                    .unwrap_or(&empty);
                vals.iter()
                    .filter_map(|pair| {
                        let arr = pair.as_array()?;
                        let ts = arr.first()?.as_f64()? as i64;
                        let val = arr.get(1)?.as_str()?.parse::<f64>().ok()?;
                        Some((ts, val))
                    })
                    .collect()
            }
            _ => {
                // vector / scalar
                if let Some(val_arr) = result.get("value").and_then(|v| v.as_array()) {
                    let ts = val_arr.first().and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
                    let val = val_arr
                        .get(1)
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    vec![(ts, val)]
                } else {
                    vec![]
                }
            }
        };

        series.push(MetricSeries { labels, values });
    }

    Ok(QueryResult { series })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── wiremock integration tests ────────────────────────────────────────────

    #[tokio::test]
    async fn push_sends_correct_request() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/import/prometheus"))
            .and(header("content-type", "text/plain"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        });

        let mut labels = HashMap::new();
        labels.insert("node".into(), "n1".into());

        let samples = vec![MetricSample {
            name: "gpu_utilization".into(),
            labels,
            timestamp_ms: 1_704_067_200_000,
            value: 0.85,
        }];

        let result = client.push(&samples).await;
        assert!(result.is_ok(), "push should succeed: {result:?}");

        // wiremock verifies expectation (expect(1)) on drop
        server.verify().await;
    }

    #[tokio::test]
    async fn push_body_contains_metric_line() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/import/prometheus"))
            .and(body_string_contains("gpu_utilization"))
            .and(body_string_contains("0.85"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        });

        let samples = vec![MetricSample {
            name: "gpu_utilization".into(),
            labels: HashMap::new(),
            timestamp_ms: 1_704_067_200_000,
            value: 0.85,
        }];

        let result = client.push(&samples).await;
        assert!(result.is_ok(), "push should succeed: {result:?}");
        server.verify().await;
    }

    #[tokio::test]
    async fn push_failure_propagates_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/import/prometheus"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
            .expect(1)
            .mount(&server)
            .await;

        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        });

        let samples = vec![MetricSample {
            name: "some_metric".into(),
            labels: HashMap::new(),
            timestamp_ms: 1_000_000,
            value: 1.0,
        }];

        let result = client.push(&samples).await;
        assert!(result.is_err(), "push should fail on HTTP 500");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("500"),
            "error should mention HTTP 500, got: {err_msg}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn query_returns_parsed_results() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let response_body = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [
                    {
                        "metric": {"__name__": "gpu_util", "node": "n1"},
                        "values": [
                            [1704067200, "0.5"],
                            [1704067260, "0.7"]
                        ]
                    }
                ]
            }
        });

        Mock::given(method("GET"))
            .and(path("/api/v1/query_range"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        });

        let result = client.query("gpu_util", 300).await;
        assert!(result.is_ok(), "query should succeed: {result:?}");

        let query_result = result.unwrap();
        assert_eq!(query_result.series.len(), 1);
        assert_eq!(query_result.series[0].labels["node"], "n1");
        assert_eq!(query_result.series[0].values.len(), 2);
        assert!((query_result.series[0].values[0].1 - 0.5).abs() < f64::EPSILON);
        assert!((query_result.series[0].values[1].1 - 0.7).abs() < f64::EPSILON);

        server.verify().await;
    }

    #[tokio::test]
    async fn query_instant_returns_parsed_result() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let response_body = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {"__name__": "cpu_usage", "job": "lattice"},
                        "value": [1704067200, "0.42"]
                    }
                ]
            }
        });

        Mock::given(method("GET"))
            .and(path("/api/v1/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        });

        let result = client.query_instant("cpu_usage").await;
        assert!(result.is_ok(), "instant query should succeed: {result:?}");

        let query_result = result.unwrap();
        assert_eq!(query_result.series.len(), 1);
        assert_eq!(query_result.series[0].labels["job"], "lattice");
        assert_eq!(query_result.series[0].values.len(), 1);
        assert!((query_result.series[0].values[0].1 - 0.42).abs() < f64::EPSILON);

        server.verify().await;
    }

    #[tokio::test]
    async fn query_with_empty_result() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let response_body = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": []
            }
        });

        Mock::given(method("GET"))
            .and(path("/api/v1/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        });

        let result = client.query_instant("nonexistent_metric").await;
        assert!(result.is_ok(), "empty result should succeed: {result:?}");

        let query_result = result.unwrap();
        assert!(
            query_result.series.is_empty(),
            "series should be empty for empty result"
        );

        server.verify().await;
    }

    #[test]
    fn format_samples_no_labels() {
        let samples = vec![MetricSample {
            name: "gpu_utilization".into(),
            labels: HashMap::new(),
            timestamp_ms: 1000000,
            value: 0.85,
        }];

        let formatted = VictoriaMetricsClient::format_samples(&samples);
        assert_eq!(formatted, "gpu_utilization 0.85 1000000");
    }

    #[test]
    fn format_samples_with_labels() {
        let mut labels = HashMap::new();
        labels.insert("node".into(), "n1".into());

        let samples = vec![MetricSample {
            name: "gpu_utilization".into(),
            labels,
            timestamp_ms: 1000000,
            value: 0.85,
        }];

        let formatted = VictoriaMetricsClient::format_samples(&samples);
        assert!(formatted.contains("gpu_utilization{"));
        assert!(formatted.contains("node=\"n1\""));
        assert!(formatted.contains("0.85 1000000"));
    }

    #[test]
    fn format_multiple_samples() {
        let samples = vec![
            MetricSample {
                name: "metric_a".into(),
                labels: HashMap::new(),
                timestamp_ms: 1000,
                value: 1.0,
            },
            MetricSample {
                name: "metric_b".into(),
                labels: HashMap::new(),
                timestamp_ms: 2000,
                value: 2.0,
            },
        ];

        let formatted = VictoriaMetricsClient::format_samples(&samples);
        let lines: Vec<&str> = formatted.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn parse_vector_response() {
        let json = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {"__name__": "gpu_util", "node": "n1"},
                        "value": [1704067200, "0.85"]
                    }
                ]
            }
        });

        let result = parse_query_response(&json).unwrap();
        assert_eq!(result.series.len(), 1);
        assert_eq!(result.series[0].labels["node"], "n1");
        assert_eq!(result.series[0].values.len(), 1);
        assert!((result.series[0].values[0].1 - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_matrix_response() {
        let json = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [
                    {
                        "metric": {"__name__": "gpu_util"},
                        "values": [
                            [1704067200, "0.5"],
                            [1704067260, "0.7"],
                            [1704067320, "0.9"]
                        ]
                    }
                ]
            }
        });

        let result = parse_query_response(&json).unwrap();
        assert_eq!(result.series.len(), 1);
        assert_eq!(result.series[0].values.len(), 3);
        assert!((result.series[0].values[2].1 - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_empty_result() {
        let json = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": []
            }
        });

        let result = parse_query_response(&json).unwrap();
        assert!(result.series.is_empty());
    }

    #[test]
    fn config_defaults() {
        let config = VictoriaMetricsConfig::default();
        assert_eq!(config.base_url, "http://localhost:8428");
        assert_eq!(config.timeout_secs, 30);
        assert!(config.auth_token.is_none());
    }

    #[test]
    fn client_urls() {
        let client = VictoriaMetricsClient::new(VictoriaMetricsConfig {
            base_url: "http://tsdb:8428".into(),
            ..Default::default()
        });

        assert_eq!(
            client.import_url(),
            "http://tsdb:8428/api/v1/import/prometheus"
        );
        assert_eq!(client.query_url(), "http://tsdb:8428/api/v1/query");
        assert_eq!(
            client.query_range_url(),
            "http://tsdb:8428/api/v1/query_range"
        );
    }
}

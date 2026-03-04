//! VAST storage service client.
//!
//! Implements the [`StorageService`] trait by communicating with the VAST
//! Management System REST API for data lifecycle operations.
//!
//! # VAST API Overview
//!
//! - **Catalog**: Namespace/path metadata, quota info, capacity
//!   - `GET /api/views/{path}` — view/quota details
//!   - `GET /api/capacity` — cluster capacity summary
//! - **Data Lifecycle**: Prefetch, tiering, QoS
//!   - `POST /api/nfs/prefetch` — prefetch data to hot tier (NVMe cache)
//!   - `PUT /api/qospolicies/{policy_id}` — set bandwidth floor for a path
//! - **Security**: Encrypted pools, secure wipe
//!   - `DELETE /api/views/{path}?wipe=true` — secure wipe of a path (zero-fill + crypto-erase)

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LatticeError;
use crate::traits::StorageService;

/// Configuration for connecting to a VAST storage cluster.
#[derive(Debug, Clone)]
pub struct VastConfig {
    /// Base URL for the VAST management API (e.g., "https://vast-mgmt.example.com")
    pub base_url: String,
    /// API username for authentication
    pub username: String,
    /// API password for authentication
    pub password: String,
    /// Request timeout in seconds
    pub timeout_secs: u64,
}

/// HTTP client for the VAST storage management system.
///
/// Provides concrete storage operations: data readiness checks (is data
/// on hot tier?), prefetch/staging, QoS floor bandwidth, and secure wipe
/// for sensitive workload teardown.
pub struct VastClient {
    http: Client,
    config: VastConfig,
}

impl VastClient {
    /// Create a new VAST client with the given configuration.
    pub fn new(config: VastConfig) -> Result<Self, LatticeError> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| LatticeError::Internal(format!("failed to build HTTP client: {e}")))?;

        Ok(Self { http, config })
    }

    /// Authenticate with VAST and return a session token.
    ///
    /// VAST uses basic auth for the initial request; the returned token
    /// is used for subsequent API calls.
    async fn authenticate(&self) -> Result<String, LatticeError> {
        let url = format!("{}/api/token", self.config.base_url);

        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .send()
            .await
            .map_err(|e| LatticeError::StorageError(format!("VAST auth request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::StorageError(format!(
                "VAST auth returned {status}: {body}"
            )));
        }

        let token_resp: VastTokenResponse = resp
            .json()
            .await
            .map_err(|e| LatticeError::StorageError(format!("failed to parse token: {e}")))?;

        Ok(token_resp.access)
    }

    /// Query the VAST view for a given path to determine capacity and tier info.
    async fn get_view(&self, path: &str, token: &str) -> Result<VastViewResponse, LatticeError> {
        let encoded_path = path.trim_start_matches('/');
        let url = format!("{}/api/views/{encoded_path}", self.config.base_url);

        let resp = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| LatticeError::StorageError(format!("VAST view request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::StorageError(format!(
                "VAST view returned {status}: {body}"
            )));
        }

        resp.json::<VastViewResponse>()
            .await
            .map_err(|e| LatticeError::StorageError(format!("failed to parse view: {e}")))
    }
}

#[async_trait]
impl StorageService for VastClient {
    async fn data_readiness(&self, source: &str) -> Result<f64, LatticeError> {
        let token = self.authenticate().await?;
        let view = self.get_view(source, &token).await?;

        // Readiness = fraction of data on hot tier (NVMe / SSD cache)
        if view.total_bytes == 0 {
            return Ok(1.0); // No data = fully ready
        }

        let readiness = view.hot_bytes as f64 / view.total_bytes as f64;
        Ok(readiness.clamp(0.0, 1.0))
    }

    async fn stage_data(&self, source: &str, target: &str) -> Result<(), LatticeError> {
        let token = self.authenticate().await?;

        let url = format!("{}/api/nfs/prefetch", self.config.base_url);

        let request = VastPrefetchRequest {
            path: source.to_string(),
            target_path: Some(target.to_string()),
            recursive: true,
        };

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&request)
            .send()
            .await
            .map_err(|e| LatticeError::StorageError(format!("VAST prefetch failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::StorageError(format!(
                "VAST prefetch returned {status}: {body}"
            )));
        }

        tracing::info!(source = %source, target = %target, "initiated data prefetch via VAST");
        Ok(())
    }

    async fn set_qos(&self, path: &str, floor_gbps: f64) -> Result<(), LatticeError> {
        let token = self.authenticate().await?;

        let url = format!("{}/api/qospolicies", self.config.base_url);

        let request = VastQosPolicyRequest {
            name: format!("lattice-qos-{}", path.replace('/', "-").trim_matches('-')),
            path: path.to_string(),
            min_bandwidth_mbps: (floor_gbps * 1000.0) as u64,
        };

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&request)
            .send()
            .await
            .map_err(|e| LatticeError::StorageError(format!("VAST QoS request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::StorageError(format!(
                "VAST QoS returned {status}: {body}"
            )));
        }

        tracing::info!(path = %path, floor_gbps = %floor_gbps, "set QoS floor via VAST");
        Ok(())
    }

    async fn wipe_data(&self, path: &str) -> Result<(), LatticeError> {
        let token = self.authenticate().await?;

        let encoded_path = path.trim_start_matches('/');
        let url = format!(
            "{}/api/views/{encoded_path}?wipe=true",
            self.config.base_url
        );

        let resp = self
            .http
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| LatticeError::StorageError(format!("VAST wipe request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::StorageError(format!(
                "VAST wipe returned {status}: {body}"
            )));
        }

        tracing::info!(path = %path, "initiated secure data wipe via VAST");
        Ok(())
    }
}

// ─── VAST API types ─────────────────────────────────────────

/// VAST token authentication response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VastTokenResponse {
    pub access: String,
    pub refresh: Option<String>,
}

/// VAST view (namespace) info response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VastViewResponse {
    pub path: String,
    /// Total bytes used in this view
    pub total_bytes: u64,
    /// Bytes on hot tier (NVMe/SSD cache)
    pub hot_bytes: u64,
    /// Bytes on warm/cold tier
    pub cold_bytes: u64,
    /// View quota in bytes (0 = unlimited)
    pub quota_bytes: u64,
}

/// VAST prefetch (staging) request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VastPrefetchRequest {
    pub path: String,
    pub target_path: Option<String>,
    pub recursive: bool,
}

/// VAST QoS policy request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VastQosPolicyRequest {
    pub name: String,
    pub path: String,
    pub min_bandwidth_mbps: u64,
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper to create a client pointing at a mock server and pre-mount auth.
    async fn mock_client_with_auth(server: &MockServer) -> VastClient {
        // Mount the token endpoint that all operations will call first
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access": "mock-token-abc",
                "refresh": "mock-refresh-xyz"
            })))
            .mount(server)
            .await;

        let config = VastConfig {
            base_url: server.uri(),
            username: "admin".to_string(),
            password: "secret".to_string(),
            timeout_secs: 5,
        };
        VastClient::new(config).unwrap()
    }

    #[tokio::test]
    async fn data_readiness_returns_ratio_of_hot_to_total() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("GET"))
            .and(path("/api/views/data/training"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "path": "/data/training",
                "total_bytes": 1000,
                "hot_bytes": 750,
                "cold_bytes": 250,
                "quota_bytes": 0
            })))
            .mount(&server)
            .await;

        let readiness = client.data_readiness("/data/training").await.unwrap();
        assert!((readiness - 0.75).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn data_readiness_returns_one_for_empty_view() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("GET"))
            .and(path("/api/views/empty"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "path": "/empty",
                "total_bytes": 0,
                "hot_bytes": 0,
                "cold_bytes": 0,
                "quota_bytes": 0
            })))
            .mount(&server)
            .await;

        let readiness = client.data_readiness("/empty").await.unwrap();
        assert!((readiness - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn data_readiness_clamps_to_zero_one() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("GET"))
            .and(path("/api/views/data/all-cold"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "path": "/data/all-cold",
                "total_bytes": 5000,
                "hot_bytes": 0,
                "cold_bytes": 5000,
                "quota_bytes": 0
            })))
            .mount(&server)
            .await;

        let readiness = client.data_readiness("/data/all-cold").await.unwrap();
        assert!((readiness - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn stage_data_sends_prefetch_request() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("POST"))
            .and(path("/api/nfs/prefetch"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let result = client.stage_data("/data/input", "/scratch/staging").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_qos_creates_policy() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("POST"))
            .and(path("/api/qospolicies"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let result = client.set_qos("/data/output", 10.0).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn wipe_data_sends_delete_with_wipe_param() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("DELETE"))
            .and(path_regex(r"/api/views/.*"))
            .and(query_param("wipe", "true"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let result = client.wipe_data("/sensitive/patient-123").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn auth_failure_propagates_error() {
        let server = MockServer::start().await;

        // Override the default auth mock with a failing one
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let config = VastConfig {
            base_url: server.uri(),
            username: "bad-user".to_string(),
            password: "bad-pass".to_string(),
            timeout_secs: 5,
        };
        let client = VastClient::new(config).unwrap();

        let result = client.data_readiness("/some/path").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("401"));
    }

    #[tokio::test]
    async fn stage_data_handles_server_error() {
        let server = MockServer::start().await;
        let client = mock_client_with_auth(&server).await;

        Mock::given(method("POST"))
            .and(path("/api/nfs/prefetch"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;

        let result = client.stage_data("/data/input", "/scratch/out").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("503"));
    }
}

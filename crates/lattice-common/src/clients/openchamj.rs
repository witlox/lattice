//! OpenCHAMI infrastructure service client.
//!
//! Implements the [`InfrastructureService`] trait by communicating with OpenCHAMI's
//! State Management Database (SMD) and Boot Script Service (BSS) REST APIs.
//!
//! # OpenCHAMI API Overview
//!
//! - **SMD (State Management Database)**: Hardware inventory, component state, Redfish endpoints
//!   - `GET /hsm/v2/State/Components/{xname}` — component state
//!   - `GET /hsm/v2/Inventory/Hardware/{xname}` — hardware inventory
//!   - `PATCH /hsm/v2/State/Components/{xname}` — update component state
//! - **BSS (Boot Script Service)**: Boot parameters and image configuration
//!   - `GET /boot/v1/bootparameters?name={xname}` — current boot params
//!   - `PUT /boot/v1/bootparameters` — set boot params (image, kernel, initrd)
//!   - `POST /boot/v1/bootparameters` — trigger reboot with new params

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LatticeError;
use crate::traits::{InfrastructureService, NodeHealthReport};
use crate::types::NodeId;

/// Configuration for connecting to an OpenCHAMI instance.
#[derive(Debug, Clone)]
pub struct OpenChamiConfig {
    /// Base URL for the OpenCHAMI SMD API (e.g., "https://smd.example.com")
    pub smd_base_url: String,
    /// Base URL for the OpenCHAMI BSS API (e.g., "https://bss.example.com")
    pub bss_base_url: String,
    /// Bearer token for authentication (optional, depends on deployment)
    pub auth_token: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: u64,
}

/// HTTP client for the OpenCHAMI infrastructure management system.
///
/// Wraps reqwest to communicate with SMD for inventory/state and BSS for
/// boot parameters. Used by the scheduler to boot nodes, query health,
/// and wipe nodes for medical workload teardown.
pub struct OpenChamiClient {
    http: Client,
    config: OpenChamiConfig,
}

impl OpenChamiClient {
    /// Create a new OpenCHAMI client with the given configuration.
    pub fn new(config: OpenChamiConfig) -> Result<Self, LatticeError> {
        let mut builder =
            Client::builder().timeout(std::time::Duration::from_secs(config.timeout_secs));

        if let Some(ref token) = config.auth_token {
            let mut headers = reqwest::header::HeaderMap::new();
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| LatticeError::ConfigError(format!("invalid auth token: {e}")))?;
            headers.insert(reqwest::header::AUTHORIZATION, value);
            builder = builder.default_headers(headers);
        }

        let http = builder
            .build()
            .map_err(|e| LatticeError::Internal(format!("failed to build HTTP client: {e}")))?;

        Ok(Self { http, config })
    }

    /// Query the SMD component state for a node.
    async fn get_component_state(&self, node_id: &NodeId) -> Result<SmdComponent, LatticeError> {
        let url = format!(
            "{}/hsm/v2/State/Components/{}",
            self.config.smd_base_url, node_id
        );

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| LatticeError::Internal(format!("SMD request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::Internal(format!(
                "SMD returned {status}: {body}"
            )));
        }

        resp.json::<SmdComponent>()
            .await
            .map_err(|e| LatticeError::Internal(format!("failed to parse SMD response: {e}")))
    }

    /// Set boot parameters for a node via BSS.
    async fn set_boot_params(
        &self,
        node_id: &NodeId,
        params: &BssBootParams,
    ) -> Result<(), LatticeError> {
        let url = format!("{}/boot/v1/bootparameters", self.config.bss_base_url);

        let request = BssBootParamsRequest {
            hosts: vec![node_id.clone()],
            params: params.clone(),
        };

        let resp = self
            .http
            .put(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| LatticeError::Internal(format!("BSS request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::Internal(format!(
                "BSS returned {status}: {body}"
            )));
        }

        Ok(())
    }

    /// Trigger a node power-cycle through SMD's Redfish integration.
    async fn power_cycle_node(&self, node_id: &NodeId) -> Result<(), LatticeError> {
        let url = format!(
            "{}/hsm/v2/State/Components/{}/Actions/PowerCycle",
            self.config.smd_base_url, node_id
        );

        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({"ResetType": "ForceRestart"}))
            .send()
            .await
            .map_err(|e| LatticeError::Internal(format!("power cycle request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LatticeError::Internal(format!(
                "power cycle returned {status}: {body}"
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl InfrastructureService for OpenChamiClient {
    async fn boot_node(&self, node_id: &NodeId, image: &str) -> Result<(), LatticeError> {
        // Step 1: Set boot parameters to use the specified OS image
        let params = BssBootParams {
            kernel: format!("s3://boot-images/{image}/vmlinuz"),
            initrd: format!("s3://boot-images/{image}/initrd.img"),
            params: format!("root=live:s3://boot-images/{image}/rootfs quiet"),
        };
        self.set_boot_params(node_id, &params).await?;

        // Step 2: Trigger a power-cycle so the node picks up new boot params
        self.power_cycle_node(node_id).await?;

        tracing::info!(node = %node_id, image = %image, "initiated node boot via OpenCHAMI");
        Ok(())
    }

    async fn wipe_node(&self, node_id: &NodeId) -> Result<(), LatticeError> {
        // For medical workload teardown: boot the node with a secure-wipe image
        // that zeroes memory and local storage before returning to the pool.
        let wipe_image = "secure-wipe-v1";
        self.boot_node(node_id, wipe_image).await?;
        tracing::info!(node = %node_id, "initiated secure wipe via OpenCHAMI");
        Ok(())
    }

    async fn query_node_health(&self, node_id: &NodeId) -> Result<NodeHealthReport, LatticeError> {
        let component = self.get_component_state(node_id).await?;

        let mut issues = Vec::new();
        let healthy = match component.state.as_str() {
            "Ready" | "On" => true,
            "Off" => {
                issues.push("node is powered off".to_string());
                false
            }
            "Standby" => {
                issues.push("node is in standby".to_string());
                false
            }
            other => {
                issues.push(format!("unexpected SMD state: {other}"));
                false
            }
        };

        if let Some(ref flag) = component.flag {
            if flag != "OK" {
                issues.push(format!("SMD flag: {flag}"));
            }
        }

        Ok(NodeHealthReport { healthy, issues })
    }
}

// ─── SMD/BSS API types ──────────────────────────────────────

/// SMD component state response (subset of fields we use).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SmdComponent {
    /// xname (e.g., "x1000c0s0b0n0")
    #[serde(rename = "ID")]
    pub id: String,
    /// Component state (e.g., "Ready", "On", "Off", "Standby")
    pub state: String,
    /// Flag (e.g., "OK", "Warning", "Alert")
    pub flag: Option<String>,
    /// Component role (e.g., "Compute", "Management")
    pub role: Option<String>,
    /// Whether the component is enabled
    pub enabled: Option<bool>,
}

/// BSS boot parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BssBootParams {
    pub kernel: String,
    pub initrd: String,
    pub params: String,
}

/// BSS boot parameters request (sets params for one or more hosts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BssBootParamsRequest {
    pub hosts: Vec<String>,
    pub params: BssBootParams,
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper to create a client pointing at the given mock server.
    fn mock_client(server: &MockServer) -> OpenChamiClient {
        let config = OpenChamiConfig {
            smd_base_url: server.uri(),
            bss_base_url: server.uri(),
            auth_token: None,
            timeout_secs: 5,
        };
        OpenChamiClient::new(config).unwrap()
    }

    #[tokio::test]
    async fn query_node_health_returns_healthy_for_ready_state() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hsm/v2/State/Components/x1000c0s0b0n0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ID": "x1000c0s0b0n0",
                "State": "Ready",
                "Flag": "OK",
                "Role": "Compute",
                "Enabled": true
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let report = client
            .query_node_health(&"x1000c0s0b0n0".to_string())
            .await
            .unwrap();

        assert!(report.healthy);
        assert!(report.issues.is_empty());
    }

    #[tokio::test]
    async fn query_node_health_returns_unhealthy_for_off_state() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hsm/v2/State/Components/x1000c0s0b0n0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ID": "x1000c0s0b0n0",
                "State": "Off",
                "Flag": "OK"
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let report = client
            .query_node_health(&"x1000c0s0b0n0".to_string())
            .await
            .unwrap();

        assert!(!report.healthy);
        assert!(report.issues.iter().any(|i| i.contains("powered off")));
    }

    #[tokio::test]
    async fn query_node_health_reports_warning_flag() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hsm/v2/State/Components/x1000c0s0b0n0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ID": "x1000c0s0b0n0",
                "State": "Ready",
                "Flag": "Warning"
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let report = client
            .query_node_health(&"x1000c0s0b0n0".to_string())
            .await
            .unwrap();

        // Node state is Ready so healthy=true, but the flag is non-OK
        assert!(report.healthy);
        assert!(report.issues.iter().any(|i| i.contains("Warning")));
    }

    #[tokio::test]
    async fn query_node_health_handles_smd_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hsm/v2/State/Components/x1000c0s0b0n0"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let result = client.query_node_health(&"x1000c0s0b0n0".to_string()).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("404"));
    }

    #[tokio::test]
    async fn boot_node_sets_params_and_power_cycles() {
        let server = MockServer::start().await;

        // Expect BSS PUT for boot params
        Mock::given(method("PUT"))
            .and(path("/boot/v1/bootparameters"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        // Expect SMD POST for power cycle
        Mock::given(method("POST"))
            .and(path_regex(
                r"/hsm/v2/State/Components/.*/Actions/PowerCycle",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let result = client
            .boot_node(&"x1000c0s0b0n0".to_string(), "ubuntu-22.04")
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn wipe_node_boots_with_secure_wipe_image() {
        let server = MockServer::start().await;

        // Expect BSS PUT for boot params (secure-wipe image)
        Mock::given(method("PUT"))
            .and(path("/boot/v1/bootparameters"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        // Expect SMD POST for power cycle
        Mock::given(method("POST"))
            .and(path_regex(
                r"/hsm/v2/State/Components/.*/Actions/PowerCycle",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let result = client.wipe_node(&"x1000c0s0b0n0".to_string()).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn client_with_auth_token_is_created_successfully() {
        let config = OpenChamiConfig {
            smd_base_url: "https://smd.example.com".to_string(),
            bss_base_url: "https://bss.example.com".to_string(),
            auth_token: Some("test-token-123".to_string()),
            timeout_secs: 10,
        };
        let result = OpenChamiClient::new(config);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn boot_node_fails_on_bss_error() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/boot/v1/bootparameters"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let result = client
            .boot_node(&"x1000c0s0b0n0".to_string(), "ubuntu-22.04")
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("500"));
    }
}

//! Sovra federation client — cryptographic trust for federated scheduling.
//!
//! Sovra provides sovereign key management for cross-site federation.
//! This client handles credential exchange and trust establishment
//! between federated Lattice instances.
//!
//! Feature-gated behind `federation`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Sovra credential representing a site's identity in the federation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SovraCredential {
    /// Site identifier.
    pub site_id: String,
    /// Cryptographic token for authentication.
    pub token: String,
    /// Token expiry (seconds since epoch).
    pub expires_at: u64,
    /// Scopes granted by this credential.
    pub scopes: Vec<String>,
}

/// Configuration for the Sovra client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SovraConfig {
    /// Sovra server URL.
    pub server_url: String,
    /// This site's identifier.
    pub site_id: String,
    /// Path to the site's private key.
    pub key_path: String,
    /// Token refresh interval in seconds.
    pub refresh_interval_secs: u64,
}

impl Default for SovraConfig {
    fn default() -> Self {
        Self {
            server_url: "https://sovra.example.com".to_string(),
            site_id: "local".to_string(),
            key_path: "/etc/lattice/sovra.key".to_string(),
            refresh_interval_secs: 3600,
        }
    }
}

/// Errors from Sovra operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SovraError {
    #[error("credential exchange failed: {0}")]
    ExchangeFailed(String),
    #[error("token expired")]
    TokenExpired,
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
}

/// Trait for Sovra credential operations.
#[async_trait]
pub trait SovraClient: Send + Sync {
    /// Exchange credentials with a remote site.
    async fn exchange_credentials(
        &self,
        remote_site_id: &str,
    ) -> Result<SovraCredential, SovraError>;

    /// Verify a credential received from a remote site.
    async fn verify_credential(&self, credential: &SovraCredential) -> Result<bool, SovraError>;

    /// Refresh this site's credential.
    async fn refresh(&self) -> Result<SovraCredential, SovraError>;
}

/// Stub Sovra client for testing and non-federated deployments.
pub struct StubSovraClient {
    site_id: String,
}

impl StubSovraClient {
    pub fn new(site_id: String) -> Self {
        Self { site_id }
    }
}

#[async_trait]
impl SovraClient for StubSovraClient {
    async fn exchange_credentials(
        &self,
        remote_site_id: &str,
    ) -> Result<SovraCredential, SovraError> {
        Ok(SovraCredential {
            site_id: self.site_id.clone(),
            token: format!("stub-token-for-{remote_site_id}"),
            expires_at: 0,
            scopes: vec!["read".to_string(), "schedule".to_string()],
        })
    }

    async fn verify_credential(&self, _credential: &SovraCredential) -> Result<bool, SovraError> {
        Ok(true)
    }

    async fn refresh(&self) -> Result<SovraCredential, SovraError> {
        Ok(SovraCredential {
            site_id: self.site_id.clone(),
            token: "refreshed-stub-token".to_string(),
            expires_at: 0,
            scopes: vec!["read".to_string(), "schedule".to_string()],
        })
    }
}

/// HTTP-based Sovra client for real federation deployments.
///
/// Communicates with a Sovra server via REST API for credential
/// exchange, verification, and refresh operations.
#[cfg(feature = "federation")]
pub struct HttpSovraClient {
    config: SovraConfig,
    client: reqwest::Client,
}

#[cfg(feature = "federation")]
impl HttpSovraClient {
    /// Create a new HTTP Sovra client with the given configuration.
    pub fn new(config: SovraConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "federation")]
#[async_trait]
impl SovraClient for HttpSovraClient {
    async fn exchange_credentials(
        &self,
        remote_site_id: &str,
    ) -> Result<SovraCredential, SovraError> {
        let url = format!("{}/api/v1/credentials/exchange", self.config.server_url);
        let body = serde_json::json!({
            "site_id": self.config.site_id,
            "remote_site_id": remote_site_id,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SovraError::ConnectionFailed(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SovraError::Unauthorized("exchange rejected".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SovraError::ExchangeFailed(format!("{status}: {text}")));
        }

        resp.json::<SovraCredential>()
            .await
            .map_err(|e| SovraError::ExchangeFailed(e.to_string()))
    }

    async fn verify_credential(&self, credential: &SovraCredential) -> Result<bool, SovraError> {
        let url = format!("{}/api/v1/credentials/verify", self.config.server_url);

        let resp = self
            .client
            .post(&url)
            .json(credential)
            .send()
            .await
            .map_err(|e| SovraError::ConnectionFailed(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SovraError::Unauthorized("verification rejected".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SovraError::ExchangeFailed(format!("{status}: {text}")));
        }

        #[derive(Deserialize)]
        struct VerifyResponse {
            valid: bool,
        }

        let result = resp
            .json::<VerifyResponse>()
            .await
            .map_err(|e| SovraError::ExchangeFailed(e.to_string()))?;

        Ok(result.valid)
    }

    async fn refresh(&self) -> Result<SovraCredential, SovraError> {
        let url = format!("{}/api/v1/credentials/refresh", self.config.server_url);
        let body = serde_json::json!({
            "site_id": self.config.site_id,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SovraError::ConnectionFailed(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SovraError::Unauthorized("refresh rejected".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SovraError::ExchangeFailed(format!("{status}: {text}")));
        }

        resp.json::<SovraCredential>()
            .await
            .map_err(|e| SovraError::ExchangeFailed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_exchange_credentials() {
        let client = StubSovraClient::new("site-a".to_string());
        let cred = client.exchange_credentials("site-b").await.unwrap();
        assert_eq!(cred.site_id, "site-a");
        assert!(cred.token.contains("site-b"));
        assert_eq!(cred.scopes.len(), 2);
    }

    #[tokio::test]
    async fn stub_verify_always_true() {
        let client = StubSovraClient::new("site-a".to_string());
        let cred = SovraCredential {
            site_id: "site-b".to_string(),
            token: "anything".to_string(),
            expires_at: 0,
            scopes: vec![],
        };
        assert!(client.verify_credential(&cred).await.unwrap());
    }

    #[tokio::test]
    async fn stub_refresh() {
        let client = StubSovraClient::new("site-a".to_string());
        let cred = client.refresh().await.unwrap();
        assert_eq!(cred.site_id, "site-a");
        assert!(cred.token.contains("refreshed"));
    }

    #[test]
    fn default_config() {
        let config = SovraConfig::default();
        assert!(config.server_url.contains("sovra"));
        assert_eq!(config.refresh_interval_secs, 3600);
    }

    #[test]
    fn sovra_error_display() {
        let err = SovraError::ExchangeFailed("timeout".to_string());
        assert!(err.to_string().contains("timeout"));
        assert!(SovraError::TokenExpired.to_string().contains("expired"));
    }

    // ─── HttpSovraClient tests ─────────────────────────────────────

    #[cfg(feature = "federation")]
    #[tokio::test]
    async fn http_client_exchange_credentials() {
        let server = wiremock::MockServer::start().await;

        let cred = SovraCredential {
            site_id: "site-a".to_string(),
            token: "real-token".to_string(),
            expires_at: 9999999999,
            scopes: vec!["read".to_string(), "schedule".to_string()],
        };

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v1/credentials/exchange"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&cred))
            .mount(&server)
            .await;

        let config = SovraConfig {
            server_url: server.uri(),
            site_id: "site-a".to_string(),
            ..Default::default()
        };
        let client = HttpSovraClient::new(config);
        let result = client.exchange_credentials("site-b").await.unwrap();
        assert_eq!(result.site_id, "site-a");
        assert_eq!(result.token, "real-token");
    }

    #[cfg(feature = "federation")]
    #[tokio::test]
    async fn http_client_verify_credential() {
        let server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v1/credentials/verify"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"valid": true})),
            )
            .mount(&server)
            .await;

        let config = SovraConfig {
            server_url: server.uri(),
            site_id: "site-a".to_string(),
            ..Default::default()
        };
        let client = HttpSovraClient::new(config);
        let cred = SovraCredential {
            site_id: "site-b".to_string(),
            token: "tok".to_string(),
            expires_at: 0,
            scopes: vec![],
        };
        assert!(client.verify_credential(&cred).await.unwrap());
    }

    #[cfg(feature = "federation")]
    #[tokio::test]
    async fn http_client_refresh() {
        let server = wiremock::MockServer::start().await;

        let cred = SovraCredential {
            site_id: "site-a".to_string(),
            token: "refreshed-token".to_string(),
            expires_at: 9999999999,
            scopes: vec!["read".to_string()],
        };

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v1/credentials/refresh"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&cred))
            .mount(&server)
            .await;

        let config = SovraConfig {
            server_url: server.uri(),
            site_id: "site-a".to_string(),
            ..Default::default()
        };
        let client = HttpSovraClient::new(config);
        let result = client.refresh().await.unwrap();
        assert_eq!(result.token, "refreshed-token");
    }

    #[cfg(feature = "federation")]
    #[tokio::test]
    async fn http_client_exchange_unauthorized() {
        let server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v1/credentials/exchange"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let config = SovraConfig {
            server_url: server.uri(),
            site_id: "site-a".to_string(),
            ..Default::default()
        };
        let client = HttpSovraClient::new(config);
        let err = client.exchange_credentials("site-b").await.unwrap_err();
        assert!(matches!(err, SovraError::Unauthorized(_)));
    }
}

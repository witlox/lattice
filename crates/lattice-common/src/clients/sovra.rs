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
}

//! Client configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Configuration for connecting to a Lattice API server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// gRPC endpoint (e.g., `"http://localhost:50051"`).
    pub endpoint: String,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
    /// Bearer token for authentication.
    #[serde(skip)]
    pub token: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:50051".to_string(),
            timeout_secs: 30,
            token: None,
        }
    }
}

impl ClientConfig {
    /// Returns the configured timeout as a [`Duration`].
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = ClientConfig::default();
        assert_eq!(config.endpoint, "http://localhost:50051");
        assert_eq!(config.timeout_secs, 30);
        assert!(config.token.is_none());
    }

    #[test]
    fn timeout_conversion() {
        let config = ClientConfig {
            timeout_secs: 60,
            ..Default::default()
        };
        assert_eq!(config.timeout(), Duration::from_secs(60));
    }
}

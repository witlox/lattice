//! gRPC client wrapper — connects to the lattice-api server.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Client configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub api_endpoint: String,
    pub timeout_secs: u64,
    pub user: String,
    pub tenant: Option<String>,
    pub vcluster: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            api_endpoint: "http://localhost:50051".to_string(),
            timeout_secs: 30,
            user: whoami().unwrap_or_else(|| "anonymous".to_string()),
            tenant: None,
            vcluster: None,
        }
    }
}

impl ClientConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

fn whoami() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = ClientConfig::default();
        assert_eq!(config.api_endpoint, "http://localhost:50051");
        assert_eq!(config.timeout_secs, 30);
        assert!(config.tenant.is_none());
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

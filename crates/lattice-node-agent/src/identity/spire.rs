//! SPIRE identity provider — obtains SVID from SPIRE agent.
//!
//! Connects to the SPIRE Workload API via unix socket.
//! Primary identity source when SPIRE is deployed.
//!
//! Feature-gated behind `spire` — when disabled, a stub is provided
//! that always reports unavailable.

use hpc_identity::{IdentityError, IdentityProvider, IdentitySource, WorkloadIdentity};

/// SPIRE identity provider.
#[allow(dead_code)] // agent_socket used only with `spire` feature
pub struct SpireProvider {
    /// Path to the SPIRE agent Workload API socket.
    agent_socket: String,
}

impl SpireProvider {
    #[must_use]
    pub fn new(agent_socket: &str) -> Self {
        Self {
            agent_socket: agent_socket.to_string(),
        }
    }
}

// Stub implementation — real SPIRE integration requires the `spiffe` crate.
// When `spire` feature is added, a cfg-gated real impl will replace this.
#[async_trait::async_trait]
impl IdentityProvider for SpireProvider {
    async fn get_identity(&self) -> Result<WorkloadIdentity, IdentityError> {
        Err(IdentityError::SpireUnavailable {
            reason: "SPIRE support not compiled in (enable 'spire' feature)".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        // Check if the socket exists on the filesystem.
        // Even without the `spire` feature, we report availability based on socket
        // existence so the cascade can detect SPIRE deployment for logging.
        std::path::Path::new(&self.agent_socket).exists()
    }

    fn source_type(&self) -> IdentitySource {
        IdentitySource::Spire
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spire_not_available_no_socket() {
        let provider = SpireProvider::new("/nonexistent/spire.sock");
        assert!(!provider.is_available().await);
    }

    #[tokio::test]
    async fn spire_get_identity_returns_unavailable() {
        let provider = SpireProvider::new("/nonexistent/spire.sock");
        let err = provider.get_identity().await.unwrap_err();
        assert!(matches!(err, IdentityError::SpireUnavailable { .. }));
    }

    #[test]
    fn spire_source_type() {
        let provider = SpireProvider::new("/tmp/spire.sock");
        assert_eq!(provider.source_type(), IdentitySource::Spire);
    }
}

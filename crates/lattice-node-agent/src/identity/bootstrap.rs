//! Bootstrap identity provider — reads cert from filesystem.
//!
//! Used for initial node authentication before SPIRE or self-signed
//! is available. The cert is provisioned by OpenCHAMI in the SquashFS image.
//!
//! Bootstrap identity is temporary, discarded after SVID/cert acquisition.

use hpc_identity::{IdentityError, IdentityProvider, IdentitySource, WorkloadIdentity};
use tracing::{debug, info};

/// Bootstrap provider — reads pre-provisioned cert from filesystem.
#[allow(clippy::struct_field_names)]
pub struct BootstrapProvider {
    cert_path: String,
    key_path: String,
    ca_path: String,
}

impl BootstrapProvider {
    #[must_use]
    pub fn new(cert_path: &str, key_path: &str, ca_path: &str) -> Self {
        Self {
            cert_path: cert_path.to_string(),
            key_path: key_path.to_string(),
            ca_path: ca_path.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl IdentityProvider for BootstrapProvider {
    async fn get_identity(&self) -> Result<WorkloadIdentity, IdentityError> {
        info!("loading bootstrap identity from filesystem");

        let cert = std::fs::read(&self.cert_path).map_err(|e| {
            IdentityError::BootstrapNotFound {
                path: format!("{}: {e}", self.cert_path),
            }
        })?;
        let key = std::fs::read(&self.key_path).map_err(|e| IdentityError::BootstrapNotFound {
            path: format!("{}: {e}", self.key_path),
        })?;
        let ca = std::fs::read(&self.ca_path).map_err(|e| IdentityError::BootstrapNotFound {
            path: format!("{}: {e}", self.ca_path),
        })?;

        // Bootstrap cert expiry: conservative 1-hour default.
        // Bootstrap identity is temporary — discarded after SVID acquisition.
        // Proper X.509 parsing would require x509-parser crate — not worth the
        // dependency for a cert that's replaced within seconds of boot.
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);
        debug!("bootstrap cert expiry set to 1h (conservative default, temporary)");

        debug!(
            cert_path = %self.cert_path,
            expires_at = %expires_at,
            "bootstrap identity loaded"
        );

        Ok(WorkloadIdentity {
            cert_chain_pem: cert,
            private_key_pem: key,
            trust_bundle_pem: ca,
            expires_at,
            source: IdentitySource::Bootstrap,
        })
    }

    async fn is_available(&self) -> bool {
        std::path::Path::new(&self.cert_path).exists()
            && std::path::Path::new(&self.key_path).exists()
            && std::path::Path::new(&self.ca_path).exists()
    }

    fn source_type(&self) -> IdentitySource {
        IdentitySource::Bootstrap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn bootstrap_loads_from_files() {
        let dir = TempDir::new().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        let ca = dir.path().join("ca.pem");

        std::fs::write(&cert, b"CERT DATA").unwrap();
        std::fs::write(&key, b"KEY DATA").unwrap();
        std::fs::write(&ca, b"CA DATA").unwrap();

        let provider = BootstrapProvider::new(
            cert.to_str().unwrap(),
            key.to_str().unwrap(),
            ca.to_str().unwrap(),
        );

        assert!(provider.is_available().await);
        assert_eq!(provider.source_type(), IdentitySource::Bootstrap);

        let id = provider.get_identity().await.unwrap();
        assert_eq!(id.cert_chain_pem, b"CERT DATA");
        assert_eq!(id.private_key_pem, b"KEY DATA");
        assert_eq!(id.trust_bundle_pem, b"CA DATA");
        assert_eq!(id.source, IdentitySource::Bootstrap);
    }

    #[tokio::test]
    async fn bootstrap_not_available_missing_files() {
        let provider =
            BootstrapProvider::new("/nonexistent/cert", "/nonexistent/key", "/nonexistent/ca");
        assert!(!provider.is_available().await);
    }

    #[tokio::test]
    async fn bootstrap_get_identity_fails_missing() {
        let provider =
            BootstrapProvider::new("/nonexistent/cert", "/nonexistent/key", "/nonexistent/ca");
        let err = provider.get_identity().await.unwrap_err();
        assert!(matches!(err, IdentityError::BootstrapNotFound { .. }));
    }

    #[tokio::test]
    async fn bootstrap_partial_files_not_available() {
        let dir = TempDir::new().unwrap();
        let cert = dir.path().join("cert.pem");
        // Only write cert, not key or ca
        std::fs::write(&cert, b"CERT DATA").unwrap();

        let provider = BootstrapProvider::new(
            cert.to_str().unwrap(),
            "/nonexistent/key",
            "/nonexistent/ca",
        );
        assert!(!provider.is_available().await);
    }
}

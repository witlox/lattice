//! Self-signed identity provider — CSR signing via quorum CA.
//!
//! Fallback identity source: agent generates keypair + CSR, submits to
//! lattice-quorum for signing. Used when SPIRE is not deployed.
//!
//! For now, this is a placeholder that stores a pre-set identity from
//! config or manual setup. The actual CSR-to-quorum flow will come later
//! when quorum CA signing is implemented.

use hpc_identity::{IdentityError, IdentityProvider, IdentitySource, WorkloadIdentity};
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Self-signed identity provider (quorum CA model).
///
/// Uses cached enrollment result from quorum CA signing.
/// The actual CSR submission flow is deferred — for now, identity
/// must be pre-set via [`set_cached_identity`].
pub struct SelfSignedProvider {
    /// Quorum endpoint for CSR signing.
    signing_endpoint: String,
    /// Cached identity (if available from prior enrollment).
    cached_identity: Mutex<Option<WorkloadIdentity>>,
}

impl SelfSignedProvider {
    #[must_use]
    pub fn new(signing_endpoint: &str) -> Self {
        Self {
            signing_endpoint: signing_endpoint.to_string(),
            cached_identity: Mutex::new(None),
        }
    }

    /// Provide a pre-computed identity (from prior enrollment).
    ///
    /// This avoids re-enrolling when the quorum CA has already
    /// signed a cert during the boot sequence.
    pub async fn set_cached_identity(&self, identity: WorkloadIdentity) {
        *self.cached_identity.lock().await = Some(identity);
    }
}

#[async_trait::async_trait]
impl IdentityProvider for SelfSignedProvider {
    async fn get_identity(&self) -> Result<WorkloadIdentity, IdentityError> {
        // Check for cached identity first
        let cached = self.cached_identity.lock().await;
        if let Some(ref identity) = *cached {
            info!("using cached enrollment result for identity");
            return Ok(identity.clone());
        }
        drop(cached);

        // No cached result — would need to run CSR enrollment with quorum
        // This is deferred until quorum CA signing is implemented.
        info!(
            endpoint = %self.signing_endpoint,
            "self-signed provider: no cached enrollment, quorum enrollment needed"
        );
        Err(IdentityError::CsrSigningFailed {
            reason: "no cached enrollment result and live enrollment not yet implemented"
                .to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        // Available if we have a cached result OR signing endpoint is configured
        let has_cached = self.cached_identity.lock().await.is_some();
        let has_endpoint = !self.signing_endpoint.is_empty();
        let available = has_cached || has_endpoint;
        debug!(
            endpoint = %self.signing_endpoint,
            has_cached = has_cached,
            available = available,
            "self-signed provider availability check"
        );
        available
    }

    fn source_type(&self) -> IdentitySource {
        IdentitySource::SelfSigned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn test_identity() -> WorkloadIdentity {
        WorkloadIdentity {
            cert_chain_pem: b"CERT".to_vec(),
            private_key_pem: b"KEY".to_vec(),
            trust_bundle_pem: b"CA".to_vec(),
            expires_at: Utc::now() + Duration::days(3),
            source: IdentitySource::SelfSigned,
        }
    }

    #[tokio::test]
    async fn self_signed_available_with_endpoint() {
        let provider = SelfSignedProvider::new("https://quorum:9443");
        assert!(provider.is_available().await);
    }

    #[tokio::test]
    async fn self_signed_not_available_empty_endpoint() {
        let provider = SelfSignedProvider::new("");
        assert!(!provider.is_available().await);
    }

    #[tokio::test]
    async fn self_signed_no_cached_result_fails() {
        let provider = SelfSignedProvider::new("https://quorum:9443");
        let err = provider.get_identity().await.unwrap_err();
        assert!(matches!(err, IdentityError::CsrSigningFailed { .. }));
    }

    #[tokio::test]
    async fn self_signed_with_cached_identity() {
        let provider = SelfSignedProvider::new("https://quorum:9443");
        provider.set_cached_identity(test_identity()).await;

        assert!(provider.is_available().await);
        let id = provider.get_identity().await.unwrap();
        assert_eq!(id.source, IdentitySource::SelfSigned);
        assert_eq!(id.cert_chain_pem, b"CERT");
        assert_eq!(id.private_key_pem, b"KEY");
        assert_eq!(id.trust_bundle_pem, b"CA");
    }

    #[test]
    fn self_signed_source_type() {
        let provider = SelfSignedProvider::new("https://quorum:9443");
        assert_eq!(provider.source_type(), IdentitySource::SelfSigned);
    }

    #[tokio::test]
    async fn self_signed_available_with_cached_and_empty_endpoint() {
        let provider = SelfSignedProvider::new("");
        assert!(!provider.is_available().await);

        provider.set_cached_identity(test_identity()).await;
        assert!(provider.is_available().await);
    }
}

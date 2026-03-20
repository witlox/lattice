//! Certificate rotation for lattice-node-agent.
//!
//! `LatticeRotator` implements `hpc_identity::CertRotator` to swap
//! the active workload identity when a cert renewal occurs.
//!
//! The actual dual-channel tonic swap (building passive channel,
//! health-checking, swapping) is deferred — for now this stores
//! the new identity and provides an accessor for code that needs
//! the active cert.

use std::sync::Arc;

use hpc_identity::{CertRotator, IdentityError, WorkloadIdentity};
use tokio::sync::RwLock;
use tracing::info;

/// Certificate rotator for lattice-node-agent.
///
/// Holds the currently active workload identity behind an `Arc<RwLock>`.
/// When `rotate()` is called, the new identity replaces the old one.
///
/// Future work: dual-channel tonic swap where a passive gRPC channel
/// is built with the new cert, health-checked, then atomically swapped
/// with the active channel.
pub struct LatticeRotator {
    /// Current active identity. `None` before first identity acquisition.
    current: Arc<RwLock<Option<WorkloadIdentity>>>,
}

impl LatticeRotator {
    /// Create a new rotator with no initial identity.
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a rotator pre-loaded with an initial identity.
    #[must_use]
    pub fn with_identity(identity: WorkloadIdentity) -> Self {
        Self {
            current: Arc::new(RwLock::new(Some(identity))),
        }
    }

    /// Get the current active identity, if any.
    pub async fn current_identity(&self) -> Option<WorkloadIdentity> {
        self.current.read().await.clone()
    }

    /// Get a cloneable handle to the inner identity store.
    ///
    /// Useful for sharing the identity state with other components
    /// (e.g., gRPC client constructors that need the current cert).
    #[must_use]
    pub fn identity_handle(&self) -> Arc<RwLock<Option<WorkloadIdentity>>> {
        Arc::clone(&self.current)
    }
}

impl Default for LatticeRotator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl CertRotator for LatticeRotator {
    async fn rotate(&self, new_identity: WorkloadIdentity) -> Result<(), IdentityError> {
        info!(
            source = ?new_identity.source,
            expires_at = %new_identity.expires_at,
            "rotating to new identity"
        );

        let mut guard = self.current.write().await;
        *guard = Some(new_identity);

        info!("identity rotation complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use hpc_identity::IdentitySource;

    fn test_identity(source: IdentitySource) -> WorkloadIdentity {
        WorkloadIdentity {
            cert_chain_pem: b"cert".to_vec(),
            private_key_pem: b"key".to_vec(),
            trust_bundle_pem: b"ca".to_vec(),
            expires_at: Utc::now() + Duration::hours(1),
            source,
        }
    }

    #[tokio::test]
    async fn rotator_starts_empty() {
        let rotator = LatticeRotator::new();
        assert!(rotator.current_identity().await.is_none());
    }

    #[tokio::test]
    async fn rotator_with_initial_identity() {
        let id = test_identity(IdentitySource::Bootstrap);
        let rotator = LatticeRotator::with_identity(id);
        let current = rotator.current_identity().await.unwrap();
        assert_eq!(current.source, IdentitySource::Bootstrap);
    }

    #[tokio::test]
    async fn rotator_stores_new_identity() {
        let rotator = LatticeRotator::new();
        assert!(rotator.current_identity().await.is_none());

        let id = test_identity(IdentitySource::SelfSigned);
        rotator.rotate(id).await.unwrap();

        let current = rotator.current_identity().await.unwrap();
        assert_eq!(current.source, IdentitySource::SelfSigned);
        assert_eq!(current.cert_chain_pem, b"cert");
    }

    #[tokio::test]
    async fn rotator_replaces_identity() {
        let rotator = LatticeRotator::with_identity(test_identity(IdentitySource::Bootstrap));

        let new_id = test_identity(IdentitySource::Spire);
        rotator.rotate(new_id).await.unwrap();

        let current = rotator.current_identity().await.unwrap();
        assert_eq!(current.source, IdentitySource::Spire);
    }

    #[tokio::test]
    async fn identity_handle_shares_state() {
        let rotator = LatticeRotator::new();
        let handle = rotator.identity_handle();

        let id = test_identity(IdentitySource::Bootstrap);
        rotator.rotate(id).await.unwrap();

        let read = handle.read().await;
        assert!(read.is_some());
        assert_eq!(read.as_ref().unwrap().source, IdentitySource::Bootstrap);
    }

    #[test]
    fn rotator_default_is_empty() {
        let rotator = LatticeRotator::default();
        // Can't await in sync test, but verify construction works
        let handle = rotator.identity_handle();
        assert!(Arc::strong_count(&handle) >= 1);
    }
}

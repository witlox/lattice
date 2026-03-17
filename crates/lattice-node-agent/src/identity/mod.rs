//! Identity cascade — SPIRE/SelfSigned/Bootstrap provider implementations.
//!
//! Implements `hpc_identity::IdentityProvider` for each identity source.
//! The cascade tries providers in priority order:
//!
//! 1. SpireProvider — connects to SPIRE agent socket, obtains SVID
//! 2. SelfSignedProvider — uses cached quorum-signed cert (CSR flow deferred)
//! 3. BootstrapProvider — reads cert from filesystem (initial boot, temporary)

mod bootstrap;
mod rotator;
mod self_signed;
mod spire;

pub use bootstrap::BootstrapProvider;
pub use rotator::LatticeRotator;
pub use self_signed::SelfSignedProvider;
pub use spire::SpireProvider;

use hpc_identity::{IdentityCascade, IdentityProvider, WorkloadIdentity};
use tonic::transport::{Certificate, ClientTlsConfig, Identity};

use lattice_common::config::IdentityConfig;

/// Build the identity cascade for lattice-node-agent.
///
/// Order: SPIRE -> SelfSigned -> Bootstrap.
/// Each provider that is configured is included; unconfigured ones are skipped.
#[must_use]
pub fn build_cascade(config: &IdentityConfig) -> IdentityCascade {
    let mut providers: Vec<Box<dyn IdentityProvider>> = Vec::new();

    // SPIRE provider (highest priority)
    providers.push(Box::new(SpireProvider::new(&config.spire_socket)));

    // Self-signed provider (quorum CA fallback)
    if let Some(ref endpoint) = config.signing_endpoint {
        providers.push(Box::new(SelfSignedProvider::new(endpoint)));
    }

    // Bootstrap provider (last resort)
    if let (Some(ref cert), Some(ref key), Some(ref ca)) = (
        &config.bootstrap_cert,
        &config.bootstrap_key,
        &config.bootstrap_ca,
    ) {
        providers.push(Box::new(BootstrapProvider::new(cert, key, ca)));
    }

    IdentityCascade::new(providers)
}

/// Build a tonic `ClientTlsConfig` from a `WorkloadIdentity`.
///
/// This configures mutual TLS for gRPC clients using the identity's
/// cert chain, private key, and trust bundle.
///
/// # Errors
///
/// Returns an error if the PEM data cannot be parsed by tonic.
pub fn tls_config_from_identity(
    identity: &WorkloadIdentity,
) -> Result<ClientTlsConfig, tonic::transport::Error> {
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(&identity.trust_bundle_pem))
        .identity(Identity::from_pem(
            &identity.cert_chain_pem,
            &identity.private_key_pem,
        ));
    Ok(tls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use hpc_identity::IdentitySource;

    fn default_config() -> IdentityConfig {
        IdentityConfig::default()
    }

    #[test]
    fn build_cascade_spire_only() {
        let config = default_config();
        let cascade = build_cascade(&config);
        // SPIRE is always included
        assert_eq!(cascade.provider_count(), 1);
    }

    #[test]
    fn build_cascade_with_signing_endpoint() {
        let config = IdentityConfig {
            signing_endpoint: Some("https://quorum:9443".into()),
            ..default_config()
        };
        let cascade = build_cascade(&config);
        assert_eq!(cascade.provider_count(), 2); // SPIRE + SelfSigned
    }

    #[test]
    fn build_cascade_with_bootstrap() {
        let config = IdentityConfig {
            bootstrap_cert: Some("/etc/lattice/cert.pem".into()),
            bootstrap_key: Some("/etc/lattice/key.pem".into()),
            bootstrap_ca: Some("/etc/lattice/ca.pem".into()),
            ..default_config()
        };
        let cascade = build_cascade(&config);
        assert_eq!(cascade.provider_count(), 2); // SPIRE + Bootstrap
    }

    #[test]
    fn build_cascade_all_providers() {
        let config = IdentityConfig {
            signing_endpoint: Some("https://quorum:9443".into()),
            bootstrap_cert: Some("/etc/lattice/cert.pem".into()),
            bootstrap_key: Some("/etc/lattice/key.pem".into()),
            bootstrap_ca: Some("/etc/lattice/ca.pem".into()),
            ..default_config()
        };
        let cascade = build_cascade(&config);
        assert_eq!(cascade.provider_count(), 3); // SPIRE + SelfSigned + Bootstrap
    }

    #[test]
    fn build_cascade_partial_bootstrap_excluded() {
        // Only cert and key, no CA — bootstrap should NOT be included
        let config = IdentityConfig {
            bootstrap_cert: Some("/etc/lattice/cert.pem".into()),
            bootstrap_key: Some("/etc/lattice/key.pem".into()),
            bootstrap_ca: None,
            ..default_config()
        };
        let cascade = build_cascade(&config);
        assert_eq!(cascade.provider_count(), 1); // Only SPIRE
    }

    #[test]
    fn tls_config_from_identity_builds_config() {
        let identity = WorkloadIdentity {
            cert_chain_pem: b"-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----\n"
                .to_vec(),
            private_key_pem: b"-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----\n"
                .to_vec(),
            trust_bundle_pem: b"-----BEGIN CERTIFICATE-----\nca\n-----END CERTIFICATE-----\n"
                .to_vec(),
            expires_at: Utc::now() + Duration::hours(1),
            source: IdentitySource::Bootstrap,
        };
        let result = tls_config_from_identity(&identity);
        assert!(result.is_ok());
    }
}

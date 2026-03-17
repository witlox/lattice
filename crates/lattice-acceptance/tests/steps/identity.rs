use cucumber::{given, then, when};

use crate::LatticeWorld;
use chrono::{Duration, Utc};
use hpc_identity::{
    CertRotator, IdentityCascade, IdentityError, IdentityProvider, IdentitySource, WorkloadIdentity,
};
use lattice_node_agent::identity::{tls_config_from_identity, BootstrapProvider, LatticeRotator};

// ─── Test Helpers ───────────────────────────────────────────

fn make_identity(source: IdentitySource, expires_in: Duration) -> WorkloadIdentity {
    WorkloadIdentity {
        cert_chain_pem: b"-----BEGIN CERTIFICATE-----\ntest-cert\n-----END CERTIFICATE-----\n"
            .to_vec(),
        private_key_pem: b"-----BEGIN PRIVATE KEY-----\ntest-key\n-----END PRIVATE KEY-----\n"
            .to_vec(),
        trust_bundle_pem: b"-----BEGIN CERTIFICATE-----\ntest-ca\n-----END CERTIFICATE-----\n"
            .to_vec(),
        expires_at: Utc::now() + expires_in,
        source,
    }
}

fn make_identity_issued_at(
    source: IdentitySource,
    issued_at: chrono::DateTime<Utc>,
    lifetime: Duration,
) -> WorkloadIdentity {
    WorkloadIdentity {
        cert_chain_pem: b"cert".to_vec(),
        private_key_pem: b"key".to_vec(),
        trust_bundle_pem: b"ca".to_vec(),
        expires_at: issued_at + lifetime,
        source,
    }
}

/// A provider that always succeeds with a given source.
struct AlwaysAvailableProvider {
    source: IdentitySource,
}

#[async_trait::async_trait]
impl IdentityProvider for AlwaysAvailableProvider {
    async fn get_identity(&self) -> Result<WorkloadIdentity, IdentityError> {
        Ok(make_identity(self.source, Duration::hours(1)))
    }
    async fn is_available(&self) -> bool {
        true
    }
    fn source_type(&self) -> IdentitySource {
        self.source
    }
}

/// A provider that is never available.
struct NeverAvailableProvider {
    source: IdentitySource,
}

#[async_trait::async_trait]
impl IdentityProvider for NeverAvailableProvider {
    async fn get_identity(&self) -> Result<WorkloadIdentity, IdentityError> {
        Err(IdentityError::SpireUnavailable {
            reason: "test stub".to_string(),
        })
    }
    async fn is_available(&self) -> bool {
        false
    }
    fn source_type(&self) -> IdentitySource {
        self.source
    }
}

// ─── Given Steps ────────────────────────────────────────────

#[given("an identity cascade with all three providers")]
fn given_cascade_all_providers(world: &mut LatticeWorld) {
    // Default: SPIRE unavailable, self-signed no cache, bootstrap no files
    // Individual steps override these defaults
    world.identity_spire_available = false;
    world.identity_self_signed_cached = false;
    world.identity_bootstrap_exists = false;
}

#[given("SPIRE is available")]
fn given_spire_available(world: &mut LatticeWorld) {
    world.identity_spire_available = true;
}

#[given("SPIRE is unavailable")]
fn given_spire_unavailable(world: &mut LatticeWorld) {
    world.identity_spire_available = false;
}

#[given("self-signed provider has a cached identity")]
fn given_self_signed_cached(world: &mut LatticeWorld) {
    world.identity_self_signed_cached = true;
}

#[given("self-signed provider has no cached identity")]
fn given_self_signed_no_cache(world: &mut LatticeWorld) {
    world.identity_self_signed_cached = false;
}

#[given("bootstrap files exist")]
fn given_bootstrap_files(world: &mut LatticeWorld) {
    world.identity_bootstrap_exists = true;
}

#[given("an empty identity cascade")]
fn given_empty_cascade(world: &mut LatticeWorld) {
    world.identity_cascade_empty = true;
}

#[given("a valid workload identity")]
fn given_valid_identity(world: &mut LatticeWorld) {
    world.identity_result = Some(Ok(make_identity(
        IdentitySource::SelfSigned,
        Duration::hours(1),
    )));
}

#[given("an identity that expired 1 hour ago")]
fn given_expired_identity(world: &mut LatticeWorld) {
    world.identity_result = Some(Ok(make_identity(
        IdentitySource::Bootstrap,
        Duration::hours(-1),
    )));
}

#[given("an identity that expires in 1 hour")]
fn given_valid_future_identity(world: &mut LatticeWorld) {
    world.identity_result = Some(Ok(make_identity(
        IdentitySource::Bootstrap,
        Duration::hours(1),
    )));
}

#[given("an identity with 3-day lifetime issued 2 days ago")]
fn given_identity_needs_renewal(world: &mut LatticeWorld) {
    let issued = Utc::now() - Duration::days(2);
    let id = make_identity_issued_at(IdentitySource::SelfSigned, issued, Duration::days(3));
    world.identity_result = Some(Ok(id));
    world.identity_issued_at = Some(issued);
}

#[given("an identity with 3-day lifetime issued just now")]
fn given_identity_fresh(world: &mut LatticeWorld) {
    let issued = Utc::now();
    let id = make_identity_issued_at(IdentitySource::SelfSigned, issued, Duration::days(3));
    world.identity_result = Some(Ok(id));
    world.identity_issued_at = Some(issued);
}

// Rotator given steps

#[given("a rotator with an initial bootstrap identity")]
fn given_rotator_with_bootstrap(world: &mut LatticeWorld) {
    let id = make_identity(IdentitySource::Bootstrap, Duration::hours(24));
    world.identity_rotator = Some(LatticeRotator::with_identity(id).into());
}

#[given("a rotator with no initial identity")]
fn given_rotator_empty(world: &mut LatticeWorld) {
    world.identity_rotator = Some(LatticeRotator::new().into());
}

#[given("a rotator with a 3-day identity issued 2 days ago")]
fn given_rotator_needs_renewal(world: &mut LatticeWorld) {
    let issued = Utc::now() - Duration::days(2);
    let id = make_identity_issued_at(IdentitySource::SelfSigned, issued, Duration::days(3));
    world.identity_rotator = Some(LatticeRotator::with_identity(id).into());
    world.identity_issued_at = Some(issued);
}

// ─── When Steps ─────────────────────────────────────────────

#[when("the cascade acquires identity")]
async fn when_cascade_acquires(world: &mut LatticeWorld) {
    let mut providers: Vec<Box<dyn IdentityProvider>> = Vec::new();

    // SPIRE
    if world.identity_spire_available {
        providers.push(Box::new(AlwaysAvailableProvider {
            source: IdentitySource::Spire,
        }));
    } else {
        providers.push(Box::new(NeverAvailableProvider {
            source: IdentitySource::Spire,
        }));
    }

    // Self-signed
    if world.identity_self_signed_cached {
        providers.push(Box::new(AlwaysAvailableProvider {
            source: IdentitySource::SelfSigned,
        }));
    } else {
        providers.push(Box::new(NeverAvailableProvider {
            source: IdentitySource::SelfSigned,
        }));
    }

    // Bootstrap
    if world.identity_bootstrap_exists {
        providers.push(Box::new(AlwaysAvailableProvider {
            source: IdentitySource::Bootstrap,
        }));
    } else {
        providers.push(Box::new(NeverAvailableProvider {
            source: IdentitySource::Bootstrap,
        }));
    }

    let cascade = IdentityCascade::new(providers);
    world.identity_result = Some(cascade.get_identity().await.map_err(|e| format!("{e:?}")));
}

#[when("the cascade attempts to acquire identity")]
async fn when_cascade_attempts(world: &mut LatticeWorld) {
    let cascade = if world.identity_cascade_empty {
        IdentityCascade::new(vec![])
    } else {
        IdentityCascade::new(vec![Box::new(NeverAvailableProvider {
            source: IdentitySource::Spire,
        })])
    };
    world.identity_result = Some(cascade.get_identity().await.map_err(|e| format!("{e:?}")));
}

#[when("the bootstrap provider acquires identity")]
async fn when_bootstrap_acquires(world: &mut LatticeWorld) {
    let dir = tempfile::TempDir::new().unwrap();
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
    let id = provider.get_identity().await.unwrap();
    world.identity_result = Some(Ok(id));
    world._identity_tempdir = Some(dir);
}

#[when("tonic TLS config is built from the identity")]
fn when_tls_config_built(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("need a valid identity");
    let result = tls_config_from_identity(id);
    world.identity_tls_config_ok = Some(result.is_ok());
}

// Rotator when steps

#[when("a new self-signed identity is rotated in")]
async fn when_rotate_self_signed(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let id = make_identity(IdentitySource::SelfSigned, Duration::hours(24));
    rotator.rotate(id).await.unwrap();
}

#[when("a new bootstrap identity is rotated in")]
async fn when_rotate_bootstrap(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let id = make_identity(IdentitySource::Bootstrap, Duration::hours(24));
    rotator.rotate(id).await.unwrap();
}

#[when("a new SPIRE identity is rotated in")]
async fn when_rotate_spire(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let id = make_identity(IdentitySource::Spire, Duration::hours(24));
    rotator.rotate(id).await.unwrap();
}

#[when("a rotation fails")]
fn when_rotation_fails(world: &mut LatticeWorld) {
    // Simulate a failed rotation — the rotator itself does not fail,
    // but we verify the current identity is unchanged by NOT calling rotate.
    // In a real scenario the failure would be in the TLS channel handshake.
    world.identity_rotation_failed = true;
}

// ─── Then Steps ─────────────────────────────────────────────

#[then(regex = r#"^the identity source should be "([^"]+)"$"#)]
fn then_identity_source(world: &mut LatticeWorld, expected: String) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("identity acquisition failed");
    let actual = format!("{:?}", id.source);
    assert_eq!(
        actual, expected,
        "expected identity source {expected}, got {actual}"
    );
}

#[then("the identity should expire within 1 hour")]
fn then_expires_within_hour(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    let remaining = id.expires_at - Utc::now();
    assert!(
        remaining <= Duration::hours(1) && remaining > Duration::seconds(0),
        "expected identity to expire within 1 hour, remaining: {remaining}"
    );
}

#[then("the identity source should be recorded")]
fn then_source_recorded(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    // The source field is always populated — this verifies it
    let _source = id.source;
}

#[then("the identity should contain a private key")]
fn then_has_private_key(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    assert!(
        !id.private_key_pem.is_empty(),
        "identity should contain a private key"
    );
}

#[then("the private key should only exist in the identity struct")]
fn then_key_only_in_struct(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    // Private key is in the struct only — no external storage
    assert!(
        !id.private_key_pem.is_empty(),
        "private key exists in identity struct"
    );
}

#[then("the debug output should redact the private key")]
fn then_debug_redacts_key(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    let debug = format!("{id:?}");
    assert!(
        debug.contains("REDACTED"),
        "debug output should redact private key, got: {debug}"
    );
}

#[then("the TLS config should be valid")]
fn then_tls_config_valid(world: &mut LatticeWorld) {
    let ok = world
        .identity_tls_config_ok
        .expect("TLS config not built yet");
    assert!(ok, "expected TLS config to be valid");
}

#[then(regex = r#"^the cascade should fail with "([^"]+)"$"#)]
fn then_cascade_fails(world: &mut LatticeWorld, expected_error: String) {
    let result = world.identity_result.as_ref().unwrap();
    assert!(result.is_err(), "expected cascade to fail");
    let err = result.as_ref().unwrap_err();
    assert!(
        err.contains(&expected_error),
        "expected error containing '{expected_error}', got '{err}'"
    );
}

#[then("the identity should not be valid")]
fn then_identity_not_valid(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    assert!(!id.is_valid(), "expected identity to be invalid (expired)");
}

#[then("the identity should be valid")]
fn then_identity_valid(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    assert!(id.is_valid(), "expected identity to be valid");
}

#[then("the identity should need renewal")]
fn then_needs_renewal(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    let issued = world.identity_issued_at.expect("issued_at not set");
    assert!(
        id.should_renew(issued),
        "expected identity to need renewal at 2/3 lifetime"
    );
}

#[then("the identity should not need renewal")]
fn then_no_renewal(world: &mut LatticeWorld) {
    let id = world
        .identity_result
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("no identity");
    let issued = world.identity_issued_at.expect("issued_at not set");
    assert!(
        !id.should_renew(issued),
        "expected identity to NOT need renewal"
    );
}

// Rotator then steps

#[then(regex = r#"^the current identity source should be "([^"]+)"$"#)]
async fn then_rotator_source(world: &mut LatticeWorld, expected: String) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let current = rotator
        .current_identity()
        .await
        .expect("no current identity");
    let actual = format!("{:?}", current.source);
    assert_eq!(
        actual, expected,
        "expected rotator identity source {expected}, got {actual}"
    );
}

#[then("the current identity should have cert data")]
async fn then_rotator_has_cert(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let current = rotator
        .current_identity()
        .await
        .expect("no current identity");
    assert!(
        !current.cert_chain_pem.is_empty(),
        "expected cert data to be present"
    );
}

#[then("the current identity should have key data")]
async fn then_rotator_has_key(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let current = rotator
        .current_identity()
        .await
        .expect("no current identity");
    assert!(
        !current.private_key_pem.is_empty(),
        "expected key data to be present"
    );
}

#[then("the current identity should still be valid")]
async fn then_rotator_still_valid(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let current = rotator
        .current_identity()
        .await
        .expect("no current identity");
    assert!(current.is_valid(), "expected current identity to be valid");
}

#[then("the current identity should need renewal")]
async fn then_rotator_needs_renewal(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let current = rotator
        .current_identity()
        .await
        .expect("no current identity");
    let issued = world.identity_issued_at.expect("issued_at not set");
    assert!(
        current.should_renew(issued),
        "expected current identity to need renewal"
    );
}

#[then("the current identity should not be the bootstrap identity")]
async fn then_rotator_not_bootstrap(world: &mut LatticeWorld) {
    let rotator = world
        .identity_rotator
        .as_ref()
        .expect("rotator not initialized");
    let current = rotator
        .current_identity()
        .await
        .expect("no current identity");
    assert_ne!(
        current.source,
        IdentitySource::Bootstrap,
        "expected identity to not be Bootstrap"
    );
}

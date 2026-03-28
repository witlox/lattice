//! Combined gRPC + REST server with TLS and middleware wiring.
//!
//! Builds a tonic gRPC server with all three services and an axum
//! REST gateway, sharing the same backing state.
//!
//! ## TLS
//!
//! When [`TlsConfig`] is provided, the gRPC server uses `tonic::transport::ServerTlsConfig`
//! with a server [`Identity`] (cert + key). If a CA certificate is also provided the server
//! enables mutual TLS (mTLS) by setting the client CA root.
//!
//! ## Middleware stack
//!
//! The logical ordering applied to every gRPC request is:
//!
//! 1. **Rate limiting** -- per-user token bucket (optional, if `ApiState::rate_limiter` is set).
//! 2. **OIDC authentication** -- validates the Bearer token (optional, if `ApiState::oidc` is set).
//! 3. **RBAC authorization** -- derives a [`Role`] from the validated token claims and checks
//!    the [`RbacPolicy`] for the target operation (follows OIDC; no-op if OIDC is disabled).
//!
//! Middleware is applied via tonic interceptors so it participates in the normal
//! gRPC request pipeline.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tonic::transport::server::ServerTlsConfig;
use tonic::transport::{Certificate, Identity, Server as TonicServer};

use lattice_common::config::ApiConfig;
use lattice_common::proto::lattice::v1::admin_service_server::AdminServiceServer;
use lattice_common::proto::lattice::v1::allocation_service_server::AllocationServiceServer;
use lattice_common::proto::lattice::v1::node_service_server::NodeServiceServer;

use crate::grpc::admin_service::LatticeAdminService;
use crate::grpc::allocation_service::LatticeAllocationService;
use crate::grpc::node_service::LatticeNodeService;
use crate::middleware::rbac::operation_from_grpc_method;
use crate::rest;
use crate::state::ApiState;

// ─── TLS configuration ──────────────────────────────────────────────────────

/// TLS configuration for the gRPC server.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to the server PEM certificate.
    pub cert_path: PathBuf,
    /// Path to the server PEM private key.
    pub key_path: PathBuf,
    /// Optional path to a CA certificate for mutual TLS.
    /// When present, clients must present a certificate signed by this CA.
    pub ca_path: Option<PathBuf>,
}

impl TlsConfig {
    /// Validate that the referenced files exist on disk.
    pub fn validate(&self) -> Result<(), TlsConfigError> {
        if !self.cert_path.exists() {
            return Err(TlsConfigError::CertNotFound(self.cert_path.clone()));
        }
        if !self.key_path.exists() {
            return Err(TlsConfigError::KeyNotFound(self.key_path.clone()));
        }
        if let Some(ref ca) = self.ca_path {
            if !ca.exists() {
                return Err(TlsConfigError::CaNotFound(ca.clone()));
            }
        }
        Ok(())
    }

    /// Build a `tonic::transport::server::ServerTlsConfig` from the on-disk
    /// PEM files.
    pub fn to_tonic_tls_config(&self) -> Result<ServerTlsConfig, TlsConfigError> {
        let cert_pem = std::fs::read(&self.cert_path)
            .map_err(|e| TlsConfigError::IoError(self.cert_path.clone(), e.to_string()))?;
        let key_pem = std::fs::read(&self.key_path)
            .map_err(|e| TlsConfigError::IoError(self.key_path.clone(), e.to_string()))?;

        let identity = Identity::from_pem(cert_pem, key_pem);
        let mut tls = ServerTlsConfig::new().identity(identity);

        if let Some(ref ca_path) = self.ca_path {
            let ca_pem = std::fs::read(ca_path)
                .map_err(|e| TlsConfigError::IoError(ca_path.clone(), e.to_string()))?;
            tls = tls.client_ca_root(Certificate::from_pem(ca_pem));
        }

        Ok(tls)
    }
}

/// Errors related to TLS configuration.
#[derive(Debug, thiserror::Error)]
pub enum TlsConfigError {
    #[error("TLS certificate file not found: {0}")]
    CertNotFound(PathBuf),
    #[error("TLS key file not found: {0}")]
    KeyNotFound(PathBuf),
    #[error("TLS CA certificate file not found: {0}")]
    CaNotFound(PathBuf),
    #[error("I/O error reading {0}: {1}")]
    IoError(PathBuf, String),
}

/// Try to extract a [`TlsConfig`] from the [`ApiConfig`].
///
/// Returns `Some(TlsConfig)` when both `tls_cert` and `tls_key` are set.
pub fn tls_config_from_api(api: &ApiConfig) -> Option<TlsConfig> {
    match (&api.tls_cert, &api.tls_key) {
        (Some(cert), Some(key)) => Some(TlsConfig {
            cert_path: cert.clone(),
            key_path: key.clone(),
            ca_path: api.tls_ca.clone(),
        }),
        _ => None,
    }
}

// ─── Server configuration ────────────────────────────────────────────────────

/// Server configuration.
pub struct ServerConfig {
    /// gRPC listen address.
    pub grpc_addr: SocketAddr,
    /// REST listen address.
    pub rest_addr: SocketAddr,
    /// Optional TLS configuration for the gRPC server.
    pub tls: Option<TlsConfig>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            grpc_addr: "0.0.0.0:50051".parse().unwrap(),
            rest_addr: "0.0.0.0:8080".parse().unwrap(),
            tls: None,
        }
    }
}

impl ServerConfig {
    /// Build a `ServerConfig` from a [`LatticeConfig`]'s `api` section.
    pub fn from_api_config(api: &ApiConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let grpc_addr: SocketAddr = api.grpc_address.parse()?;
        let rest_addr: SocketAddr = api
            .rest_address
            .as_deref()
            .unwrap_or("0.0.0.0:8080")
            .parse()?;
        let tls = tls_config_from_api(api);
        Ok(Self {
            grpc_addr,
            rest_addr,
            tls,
        })
    }
}

// ─── Middleware interceptor ──────────────────────────────────────────────────

/// Build the middleware interceptor function.
///
/// The interceptor applies the middleware stack **synchronously** on each
/// incoming gRPC request:
///
/// 1. Rate limit check (if configured).
/// 2. OIDC token extraction (if configured) -- note: because tonic interceptors
///    are synchronous, full async OIDC validation is deferred to the service
///    handlers. The interceptor extracts and stashes the bearer token.
/// 3. RBAC is evaluated **after** OIDC in the service handler since it needs
///    the validated `TokenClaims`.
///
/// The interceptor inserts metadata extensions so downstream handlers can
/// retrieve the information without re-parsing.
#[allow(clippy::result_large_err)]
pub fn build_interceptor(
    state: Arc<ApiState>,
) -> impl Fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Clone + Send + Sync
{
    let rate_limiter = state.rate_limiter.clone();
    let oidc_config = state.oidc_config.clone();
    let sync_validator = build_sync_validator(&state);

    move |mut req: tonic::Request<()>| -> Result<tonic::Request<()>, tonic::Status> {
        // ── Step 1: Rate limiting ────────────────────────────────────────
        if let Some(ref limiter) = rate_limiter {
            // Use peer address as fallback identity for rate limiting.
            let user_id = req
                .metadata()
                .get("x-user-id")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    req.remote_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                });
            if let Err(e) = limiter.check(&user_id) {
                return Err(tonic::Status::resource_exhausted(format!(
                    "rate limit exceeded; retry after {}s",
                    e.retry_after_secs
                )));
            }
        }

        // ── Step 2: Extract and validate bearer token ───────────────────
        let bearer_token = req
            .metadata()
            .get("authorization")
            .and_then(|auth| auth.to_str().ok())
            .and_then(|val| val.strip_prefix("Bearer "))
            .map(|token| BearerToken(token.to_string()));

        if oidc_config.is_some() {
            // Auth is required — reject if no token
            let token = bearer_token
                .ok_or_else(|| tonic::Status::unauthenticated("missing Bearer token"))?;

            // Synchronous token validation (HMAC or cached JWKS)
            if let Some(ref validator) = sync_validator {
                let claims = validator.validate_token_sync(&token.0).map_err(|e| {
                    tonic::Status::unauthenticated(format!("authentication failed: {e}"))
                })?;
                req.extensions_mut().insert(claims);
            }

            req.extensions_mut().insert(token);
        } else if let Some(token) = bearer_token {
            // No auth required but token provided — stash it
            req.extensions_mut().insert(token);
        }

        // ── Step 3: Extract gRPC method for RBAC (stash for handler) ─────
        let method_path = req
            .metadata()
            .get("grpc-method")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        if let Some(path) = method_path {
            if let Some(op) = operation_from_grpc_method(&path) {
                req.extensions_mut().insert(RequestOperation(op));
            }
        }

        Ok(req)
    }
}

/// Build a synchronous token validator for the gRPC interceptor.
///
/// Tries HMAC first (env var), then falls back to the JWKS validator
/// (if the `oidc` feature is enabled and JWKS keys are cached).
fn build_sync_validator(
    state: &ApiState,
) -> Option<Arc<dyn crate::middleware::oidc::SyncOidcValidator>> {
    // HMAC takes priority (fast, no network)
    if let Ok(secret) = std::env::var("LATTICE_OIDC_HMAC_SECRET") {
        return Some(Arc::new(crate::middleware::oidc::HmacOidcValidator::new(
            &secret,
            state.oidc_config.as_ref(),
        )));
    }

    // Fall back to JWKS sync validator (uses cached keys from prefetch)
    #[cfg(feature = "oidc")]
    if let Some(ref oidc_config) = state.oidc_config {
        if !oidc_config.issuer_url.is_empty() {
            let validator = crate::middleware::oidc::JwtOidcValidator::new(oidc_config.clone());
            return Some(Arc::new(validator));
        }
    }

    None
}

/// Extension type: the raw bearer token extracted from the `authorization` header.
#[derive(Debug, Clone)]
pub struct BearerToken(pub String);

/// Extension type: the RBAC operation derived from the gRPC method path.
#[derive(Debug, Clone)]
pub struct RequestOperation(pub crate::middleware::rbac::Operation);

/// Describes the middleware stack ordering for documentation and testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiddlewareLayer {
    RateLimit,
    Oidc,
    Rbac,
}

/// Returns the middleware layers in the order they are evaluated.
pub fn middleware_stack_order() -> Vec<MiddlewareLayer> {
    vec![
        MiddlewareLayer::RateLimit,
        MiddlewareLayer::Oidc,
        MiddlewareLayer::Rbac,
    ]
}

// ─── Server functions ────────────────────────────────────────────────────────

/// Build the gRPC server (does not start listening).
pub fn build_grpc_server(state: Arc<ApiState>) -> TonicServer {
    let _ = state; // Used in serve_grpc
    TonicServer::builder()
}

/// Start the gRPC server with optional TLS and middleware.
pub async fn serve_grpc(
    state: Arc<ApiState>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    serve_grpc_with_tls(state, addr, None).await
}

/// Start the gRPC server with explicit TLS configuration.
pub async fn serve_grpc_with_tls(
    state: Arc<ApiState>,
    addr: SocketAddr,
    tls: Option<&TlsConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    let alloc_svc = LatticeAllocationService::new(state.clone());
    let node_svc = LatticeNodeService::new(state.clone());
    let admin_svc = LatticeAdminService::new(state.clone());

    let interceptor = build_interceptor(state);

    let mut builder = TonicServer::builder();

    // Configure TLS if provided.
    if let Some(tls_cfg) = tls {
        let tonic_tls = tls_cfg.to_tonic_tls_config()?;
        tracing::info!(
            "gRPC server TLS enabled (mTLS={})",
            tls_cfg.ca_path.is_some()
        );
        builder = builder.tls_config(tonic_tls)?;
    }

    tracing::info!("gRPC server listening on {}", addr);

    builder
        .add_service(AllocationServiceServer::with_interceptor(
            alloc_svc,
            interceptor.clone(),
        ))
        .add_service(NodeServiceServer::with_interceptor(
            node_svc,
            interceptor.clone(),
        ))
        .add_service(AdminServiceServer::with_interceptor(admin_svc, interceptor))
        .serve(addr)
        .await?;

    Ok(())
}

/// Start the REST server.
pub async fn serve_rest(
    state: Arc<ApiState>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = rest::router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("REST server listening on {}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

/// Start both gRPC and REST servers concurrently.
pub async fn serve(
    state: Arc<ApiState>,
    config: ServerConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let grpc_state = state.clone();
    let rest_state = state;

    tokio::select! {
        result = serve_grpc_with_tls(grpc_state, config.grpc_addr, config.tls.as_ref()) => result,
        result = serve_rest(rest_state, config.rest_addr) => result,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use tempfile::NamedTempFile;

    // ── Helper: create a temporary PEM file ──────────────────────────────

    fn write_temp_file(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(content).expect("write temp file");
        f.flush().expect("flush temp file");
        f
    }

    // ── Helper: generate CA + server cert using rcgen ────────────────────

    struct TestPki {
        ca_cert_pem: String,
        server_cert_pem: String,
        server_key_pem: String,
    }

    fn generate_test_pki() -> TestPki {
        use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};

        // 1. Generate CA key pair and self-signed CA certificate.
        let ca_key = KeyPair::generate().expect("CA key generation");
        let mut ca_params =
            CertificateParams::new(vec!["Lattice Test CA".to_string()]).expect("CA params");
        ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("self-sign CA");

        // 2. Generate server key pair and certificate signed by CA.
        let server_key = KeyPair::generate().expect("server key generation");
        let server_params =
            CertificateParams::new(vec!["localhost".to_string()]).expect("server params");
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let server_cert = server_params
            .signed_by(&server_key, &issuer)
            .expect("sign server cert");

        TestPki {
            ca_cert_pem: ca_cert.pem(),
            server_cert_pem: server_cert.pem(),
            server_key_pem: server_key.serialize_pem(),
        }
    }

    // ── Test 1: TLS config from ApiConfig parses correctly ───────────────

    #[test]
    fn tls_config_from_api_config_parses() {
        let api = ApiConfig {
            grpc_address: "0.0.0.0:50051".to_string(),
            rest_address: Some("0.0.0.0:8080".to_string()),
            oidc_issuer: "https://auth.example.com".to_string(),
            tls_cert: Some(PathBuf::from("/etc/lattice/tls/server.crt")),
            tls_key: Some(PathBuf::from("/etc/lattice/tls/server.key")),
            tls_ca: Some(PathBuf::from("/etc/lattice/tls/ca.crt")),
            oidc_client_id: None,
            bind_network: lattice_common::config::BindNetwork::Any,
            oidc_hmac_secret: None,
        };

        let tls = tls_config_from_api(&api).expect("should produce TlsConfig");
        assert_eq!(tls.cert_path, Path::new("/etc/lattice/tls/server.crt"));
        assert_eq!(tls.key_path, Path::new("/etc/lattice/tls/server.key"));
        assert_eq!(
            tls.ca_path.as_deref(),
            Some(Path::new("/etc/lattice/tls/ca.crt"))
        );
    }

    // ── Test 2: ServerConfig with TLS paths validates ────────────────────

    #[test]
    fn tls_config_validates_existing_files() {
        let pki = generate_test_pki();

        let cert_file = write_temp_file(pki.server_cert_pem.as_bytes());
        let key_file = write_temp_file(pki.server_key_pem.as_bytes());
        let ca_file = write_temp_file(pki.ca_cert_pem.as_bytes());

        let tls = TlsConfig {
            cert_path: cert_file.path().to_path_buf(),
            key_path: key_file.path().to_path_buf(),
            ca_path: Some(ca_file.path().to_path_buf()),
        };

        assert!(tls.validate().is_ok());
    }

    // ── Test 3: ServerConfig without TLS (backward compat) ───────────────

    #[test]
    fn server_config_without_tls_backward_compat() {
        let api = ApiConfig {
            grpc_address: "0.0.0.0:50051".to_string(),
            rest_address: None,
            oidc_issuer: "https://auth.example.com".to_string(),
            oidc_client_id: None,
            tls_cert: None,
            tls_key: None,
            tls_ca: None,
            bind_network: lattice_common::config::BindNetwork::Any,
            oidc_hmac_secret: None,
        };

        let tls = tls_config_from_api(&api);
        assert!(tls.is_none(), "no TLS config when cert/key not set");

        let server_cfg = ServerConfig::from_api_config(&api).unwrap();
        assert!(server_cfg.tls.is_none());
        assert_eq!(
            server_cfg.rest_addr,
            "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
        );
    }

    // ── Test 4: Middleware stack order is correct ─────────────────────────

    #[test]
    fn middleware_stack_order_is_rate_limit_oidc_rbac() {
        let order = middleware_stack_order();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], MiddlewareLayer::RateLimit);
        assert_eq!(order[1], MiddlewareLayer::Oidc);
        assert_eq!(order[2], MiddlewareLayer::Rbac);
    }

    // ── Test 5: TLS config with rcgen-generated certs builds tonic cfg ──

    #[test]
    fn tls_config_builds_tonic_server_tls_config() {
        let pki = generate_test_pki();

        let cert_file = write_temp_file(pki.server_cert_pem.as_bytes());
        let key_file = write_temp_file(pki.server_key_pem.as_bytes());
        let ca_file = write_temp_file(pki.ca_cert_pem.as_bytes());

        let tls = TlsConfig {
            cert_path: cert_file.path().to_path_buf(),
            key_path: key_file.path().to_path_buf(),
            ca_path: Some(ca_file.path().to_path_buf()),
        };

        let tonic_tls = tls.to_tonic_tls_config();
        assert!(
            tonic_tls.is_ok(),
            "should build tonic TLS config from rcgen certs: {:?}",
            tonic_tls.err()
        );
    }

    // ── Test 6: mTLS config construction without CA ──────────────────────

    #[test]
    fn tls_config_without_ca_is_server_tls_only() {
        let pki = generate_test_pki();

        let cert_file = write_temp_file(pki.server_cert_pem.as_bytes());
        let key_file = write_temp_file(pki.server_key_pem.as_bytes());

        let tls = TlsConfig {
            cert_path: cert_file.path().to_path_buf(),
            key_path: key_file.path().to_path_buf(),
            ca_path: None,
        };

        assert!(tls.validate().is_ok());
        let tonic_tls = tls.to_tonic_tls_config();
        assert!(
            tonic_tls.is_ok(),
            "should build server-only TLS config without CA"
        );
    }

    // ── Test 7: TLS config validate rejects missing cert ─────────────────

    #[test]
    fn tls_config_validate_rejects_missing_cert() {
        let pki = generate_test_pki();
        let key_file = write_temp_file(pki.server_key_pem.as_bytes());

        let tls = TlsConfig {
            cert_path: PathBuf::from("/nonexistent/cert.pem"),
            key_path: key_file.path().to_path_buf(),
            ca_path: None,
        };

        let err = tls.validate().unwrap_err();
        assert!(
            matches!(err, TlsConfigError::CertNotFound(_)),
            "expected CertNotFound, got: {err}"
        );
    }

    // ── Test 8: TLS config validate rejects missing key ──────────────────

    #[test]
    fn tls_config_validate_rejects_missing_key() {
        let pki = generate_test_pki();
        let cert_file = write_temp_file(pki.server_cert_pem.as_bytes());

        let tls = TlsConfig {
            cert_path: cert_file.path().to_path_buf(),
            key_path: PathBuf::from("/nonexistent/key.pem"),
            ca_path: None,
        };

        let err = tls.validate().unwrap_err();
        assert!(
            matches!(err, TlsConfigError::KeyNotFound(_)),
            "expected KeyNotFound, got: {err}"
        );
    }

    // ── Test 9: TLS config validate rejects missing CA ───────────────────

    #[test]
    fn tls_config_validate_rejects_missing_ca() {
        let pki = generate_test_pki();
        let cert_file = write_temp_file(pki.server_cert_pem.as_bytes());
        let key_file = write_temp_file(pki.server_key_pem.as_bytes());

        let tls = TlsConfig {
            cert_path: cert_file.path().to_path_buf(),
            key_path: key_file.path().to_path_buf(),
            ca_path: Some(PathBuf::from("/nonexistent/ca.pem")),
        };

        let err = tls.validate().unwrap_err();
        assert!(
            matches!(err, TlsConfigError::CaNotFound(_)),
            "expected CaNotFound, got: {err}"
        );
    }

    // ── Test 10: ServerConfig from ApiConfig with TLS ────────────────────

    #[test]
    fn server_config_from_api_config_with_tls() {
        let api = ApiConfig {
            grpc_address: "127.0.0.1:9090".to_string(),
            rest_address: Some("127.0.0.1:8888".to_string()),
            oidc_issuer: "https://auth.test.com".to_string(),
            oidc_client_id: None,
            tls_cert: Some(PathBuf::from("/tls/cert.pem")),
            tls_key: Some(PathBuf::from("/tls/key.pem")),
            tls_ca: Some(PathBuf::from("/tls/ca.pem")),
            bind_network: lattice_common::config::BindNetwork::Any,
            oidc_hmac_secret: None,
        };

        let cfg = ServerConfig::from_api_config(&api).unwrap();
        assert_eq!(
            cfg.grpc_addr,
            "127.0.0.1:9090".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            cfg.rest_addr,
            "127.0.0.1:8888".parse::<SocketAddr>().unwrap()
        );
        let tls = cfg.tls.expect("TLS should be set");
        assert_eq!(tls.cert_path, Path::new("/tls/cert.pem"));
        assert_eq!(tls.key_path, Path::new("/tls/key.pem"));
        assert_eq!(tls.ca_path.as_deref(), Some(Path::new("/tls/ca.pem")));
    }

    // ── Test 11: ApiConfig YAML deserialization with tls_ca ──────────────

    #[test]
    fn api_config_yaml_with_tls_ca() {
        let yaml = r#"
grpc_address: "0.0.0.0:50051"
rest_address: "0.0.0.0:8080"
oidc_issuer: "https://auth.example.com"
tls_cert: "/etc/lattice/tls/server.crt"
tls_key: "/etc/lattice/tls/server.key"
tls_ca: "/etc/lattice/tls/ca.crt"
"#;
        let api: ApiConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            api.tls_ca.as_deref(),
            Some(Path::new("/etc/lattice/tls/ca.crt"))
        );
        assert_eq!(
            api.tls_cert.as_deref(),
            Some(Path::new("/etc/lattice/tls/server.crt"))
        );
    }

    // ── Test 12: ApiConfig YAML deserialization without tls_ca ───────────

    #[test]
    fn api_config_yaml_without_tls_ca() {
        let yaml = r#"
grpc_address: "0.0.0.0:50051"
oidc_issuer: "https://auth.example.com"
"#;
        let api: ApiConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(api.tls_ca.is_none());
        assert!(api.tls_cert.is_none());
        assert!(api.tls_key.is_none());
    }

    // ── Test 13: TlsConfigError Display messages ─────────────────────────

    #[test]
    fn tls_config_error_display() {
        let err = TlsConfigError::CertNotFound(PathBuf::from("/x/cert.pem"));
        assert!(err.to_string().contains("/x/cert.pem"));

        let err = TlsConfigError::KeyNotFound(PathBuf::from("/x/key.pem"));
        assert!(err.to_string().contains("/x/key.pem"));

        let err = TlsConfigError::CaNotFound(PathBuf::from("/x/ca.pem"));
        assert!(err.to_string().contains("/x/ca.pem"));

        let err = TlsConfigError::IoError(PathBuf::from("/x/f"), "permission denied".into());
        assert!(err.to_string().contains("permission denied"));
    }

    // ── Test 14: Default ServerConfig is backward compatible ─────────────

    #[test]
    fn default_server_config_has_no_tls() {
        let cfg = ServerConfig::default();
        assert!(cfg.tls.is_none());
        assert_eq!(
            cfg.grpc_addr,
            "0.0.0.0:50051".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(cfg.rest_addr, "0.0.0.0:8080".parse::<SocketAddr>().unwrap());
    }
}

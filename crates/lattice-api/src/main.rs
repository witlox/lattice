//! `lattice-server` — the Lattice control-plane process.
//!
//! Starts a Raft quorum member with gRPC + REST API services.
//! Configuration is loaded from a YAML file, environment variables,
//! or command-line arguments.

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use serde::Deserialize;
use tracing::info;

use lattice_api::{serve, ApiState, ServerConfig};

#[derive(Parser)]
#[command(
    name = "lattice-server",
    version,
    about = "Lattice control-plane server"
)]
struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "/etc/lattice/config.yaml")]
    config: String,

    /// gRPC listen address
    #[arg(long, default_value = "0.0.0.0:50051")]
    grpc_addr: String,

    /// REST listen address
    #[arg(long, default_value = "0.0.0.0:8080")]
    rest_addr: String,
}

/// Top-level configuration loaded from YAML.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LatticeConfig {
    /// OIDC authentication settings. When `issuer_url` is set, JWT
    /// validation is enabled; otherwise the stub validator is used.
    #[cfg(feature = "oidc")]
    oidc: Option<OidcSection>,

    /// Sovra federation settings. When `server_url` is set, the HTTP
    /// client is used; otherwise federation is disabled.
    #[cfg(feature = "federation")]
    sovra: Option<SovraSection>,

    /// Waldur accounting settings. When `api_url` is set, the HTTP
    /// client is used; otherwise accounting is disabled.
    #[cfg(feature = "accounting")]
    waldur: Option<WaldurSection>,
}

#[cfg(feature = "oidc")]
#[derive(Debug, Deserialize)]
struct OidcSection {
    issuer_url: String,
    audience: String,
    #[serde(default)]
    required_scopes: Vec<String>,
}

#[cfg(feature = "federation")]
#[derive(Debug, Deserialize)]
struct SovraSection {
    server_url: String,
    site_id: String,
    #[serde(default = "default_key_path")]
    key_path: String,
    #[serde(default = "default_refresh_interval")]
    refresh_interval_secs: u64,
}

#[cfg(feature = "federation")]
fn default_key_path() -> String {
    "/etc/lattice/sovra.key".to_string()
}
#[cfg(feature = "federation")]
fn default_refresh_interval() -> u64 {
    3600
}

#[cfg(feature = "accounting")]
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields used when adapter bridges AccountingClient → AccountingService.
struct WaldurSection {
    api_url: String,
    api_token: String,
    #[serde(default = "default_flush_interval")]
    flush_interval_secs: u64,
    #[serde(default = "default_max_buffer")]
    max_buffer_size: usize,
}

#[cfg(feature = "accounting")]
fn default_flush_interval() -> u64 {
    60
}
#[cfg(feature = "accounting")]
fn default_max_buffer() -> usize {
    1000
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!("Starting lattice-server");
    info!("gRPC address: {}", args.grpc_addr);
    info!("REST address: {}", args.rest_addr);

    // Load optional config file (missing file is OK — all defaults).
    let _config: LatticeConfig = match std::fs::read_to_string(&args.config) {
        Ok(contents) => serde_yaml::from_str(&contents)?,
        Err(_) => {
            info!("No config file at {}, using defaults", args.config);
            LatticeConfig::default()
        }
    };

    // ── OIDC ────────────────────────────────────────────────────────────
    #[cfg(feature = "oidc")]
    let oidc: Option<Arc<dyn lattice_api::middleware::oidc::OidcValidator>> =
        _config.oidc.as_ref().map(|c| {
            info!("OIDC enabled: issuer={}", c.issuer_url);
            let oidc_config = lattice_api::middleware::oidc::OidcConfig {
                issuer_url: c.issuer_url.clone(),
                audience: c.audience.clone(),
                required_scopes: c.required_scopes.clone(),
            };
            Arc::new(lattice_api::middleware::oidc::JwtOidcValidator::new(
                oidc_config,
            )) as Arc<dyn lattice_api::middleware::oidc::OidcValidator>
        });
    #[cfg(not(feature = "oidc"))]
    let oidc: Option<Arc<dyn lattice_api::middleware::oidc::OidcValidator>> = None;

    // ── Sovra ───────────────────────────────────────────────────────────
    #[cfg(feature = "federation")]
    let sovra: Option<Arc<dyn lattice_common::clients::SovraClient>> =
        _config.sovra.as_ref().map(|c| {
            info!("Sovra federation enabled: server={}", c.server_url);
            let sovra_config = lattice_common::clients::sovra::SovraConfig {
                server_url: c.server_url.clone(),
                site_id: c.site_id.clone(),
                key_path: c.key_path.clone(),
                refresh_interval_secs: c.refresh_interval_secs,
            };
            Arc::new(lattice_common::clients::HttpSovraClient::new(sovra_config))
                as Arc<dyn lattice_common::clients::SovraClient>
        });
    #[cfg(not(feature = "federation"))]
    let sovra: Option<Arc<dyn lattice_common::clients::SovraClient>> = None;

    // ── Waldur ──────────────────────────────────────────────────────────
    // HttpWaldurClient implements AccountingClient (low-level event API),
    // while ApiState.accounting expects AccountingService (allocation-level).
    // Config is parsed; a thin adapter can bridge them when needed.
    #[cfg(feature = "accounting")]
    if let Some(ref c) = _config.waldur {
        info!("Waldur accounting configured: url={}", c.api_url);
    }
    let accounting: Option<Arc<dyn lattice_common::traits::AccountingService>> = None;

    // For now, use in-memory test quorum. Real Raft wired via Raft transport.
    let quorum = lattice_quorum::create_test_quorum().await?;
    let quorum = Arc::new(quorum);

    let state = Arc::new(ApiState {
        allocations: quorum.clone(),
        nodes: quorum.clone(),
        audit: quorum.clone(),
        checkpoint: Arc::new(lattice_test_harness::mocks::MockCheckpointBroker::new()),
        quorum: Some(quorum),
        events: lattice_api::events::new_event_bus(),
        tsdb: None,
        storage: None,
        accounting,
        oidc,
        rate_limiter: None,
        sovra,
        pty: None,
    });

    let server_config = ServerConfig {
        grpc_addr: args.grpc_addr.parse()?,
        rest_addr: args.rest_addr.parse()?,
        tls: None,
    };

    serve(state, server_config)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(())
}

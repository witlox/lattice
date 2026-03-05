//! `lattice-server` — the Lattice control-plane process.
//!
//! Starts a Raft quorum member with gRPC + REST API services.
//! Configuration is loaded from a YAML file, environment variables,
//! or command-line arguments.

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing::info;

use lattice_api::{serve, ApiState, ServerConfig};
use lattice_common::config::LatticeConfig;
use lattice_common::tsdb_client::{VictoriaMetricsClient, VictoriaMetricsConfig};

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

    /// gRPC listen address (overrides config)
    #[arg(long)]
    grpc_addr: Option<String>,

    /// REST listen address (overrides config)
    #[arg(long)]
    rest_addr: Option<String>,
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

    // Load config file with fallback to defaults.
    let config: LatticeConfig = match std::fs::read_to_string(&args.config) {
        Ok(contents) => serde_yaml::from_str(&contents)?,
        Err(_) => {
            info!("No config file at {}, using defaults", args.config);
            LatticeConfig::default()
        }
    };

    let grpc_addr = args
        .grpc_addr
        .unwrap_or_else(|| config.api.grpc_address.clone());
    let rest_addr = args.rest_addr.unwrap_or_else(|| {
        config
            .api
            .rest_address
            .clone()
            .unwrap_or_else(|| "0.0.0.0:8080".to_string())
    });

    info!("Starting lattice-server");
    info!("gRPC address: {}", grpc_addr);
    info!("REST address: {}", rest_addr);

    // ── Raft Quorum ────────────────────────────────────────────────────────
    let (quorum, _raft_handle) = lattice_quorum::create_quorum_from_config(&config.quorum).await?;
    let quorum = Arc::new(quorum);

    if config.quorum.peers.is_empty() {
        info!("Single-node quorum (dev mode)");
    } else {
        info!(
            "Multi-node quorum: node_id={}, {} peers",
            config.quorum.node_id,
            config.quorum.peers.len()
        );
    }

    // ── OIDC ──────────────────────────────────────────────────────────────
    #[cfg(feature = "oidc")]
    let oidc: Option<Arc<dyn lattice_api::middleware::oidc::OidcValidator>> =
        if !config.api.oidc_issuer.is_empty() {
            info!("OIDC enabled: issuer={}", config.api.oidc_issuer);
            let oidc_config = lattice_api::middleware::oidc::OidcConfig {
                issuer_url: config.api.oidc_issuer.clone(),
                audience: String::new(),
                required_scopes: vec![],
            };
            Some(
                Arc::new(lattice_api::middleware::oidc::JwtOidcValidator::new(
                    oidc_config,
                )) as Arc<dyn lattice_api::middleware::oidc::OidcValidator>,
            )
        } else {
            None
        };
    #[cfg(not(feature = "oidc"))]
    let oidc: Option<Arc<dyn lattice_api::middleware::oidc::OidcValidator>> = None;

    // ── Sovra ─────────────────────────────────────────────────────────────
    #[cfg(feature = "federation")]
    let sovra: Option<Arc<dyn lattice_common::clients::SovraClient>> =
        config.federation.as_ref().map(|fed| {
            info!("Sovra federation enabled: endpoint={}", fed.sovra_endpoint);
            let sovra_config = lattice_common::clients::sovra::SovraConfig {
                server_url: fed.sovra_endpoint.clone(),
                site_id: fed.workspace_id.clone(),
                key_path: String::new(),
                refresh_interval_secs: 3600,
            };
            Arc::new(lattice_common::clients::HttpSovraClient::new(sovra_config))
                as Arc<dyn lattice_common::clients::SovraClient>
        });
    #[cfg(not(feature = "federation"))]
    let sovra: Option<Arc<dyn lattice_common::clients::SovraClient>> = None;

    // ── Waldur ────────────────────────────────────────────────────────────
    #[cfg(feature = "accounting")]
    let accounting: Option<Arc<dyn lattice_common::traits::AccountingService>> =
        if let Some(ref acct) = config.accounting {
            if acct.enabled && !acct.waldur_api_url.is_empty() {
                info!("Waldur accounting enabled: url={}", acct.waldur_api_url);
                let waldur_config = lattice_common::clients::waldur::WaldurConfig {
                    api_url: acct.waldur_api_url.clone(),
                    api_token: acct.waldur_token_secret_ref.clone(),
                    flush_interval_secs: acct.push_interval_seconds,
                    max_buffer_size: acct.buffer_size as usize,
                };
                Some(Arc::new(lattice_common::clients::HttpWaldurClient::new(
                    waldur_config,
                ))
                    as Arc<dyn lattice_common::traits::AccountingService>)
            } else {
                None
            }
        } else {
            None
        };
    #[cfg(not(feature = "accounting"))]
    let accounting: Option<Arc<dyn lattice_common::traits::AccountingService>> = None;

    // ── TSDB ──────────────────────────────────────────────────────────────
    let tsdb: Option<Arc<dyn lattice_common::tsdb_client::TsdbClient>> =
        if config.telemetry.tsdb_endpoint.is_empty() {
            None
        } else {
            info!("TSDB endpoint: {}", config.telemetry.tsdb_endpoint);
            Some(Arc::new(VictoriaMetricsClient::new(
                VictoriaMetricsConfig {
                    base_url: config.telemetry.tsdb_endpoint.clone(),
                    ..Default::default()
                },
            )))
        };

    // ── VAST Storage ──────────────────────────────────────────────────────
    let storage: Option<Arc<dyn lattice_common::traits::StorageService>> =
        if let Some(ref url) = config.storage.vast_api_url {
            if !url.is_empty() {
                info!("VAST storage enabled: url={}", url);
                let vast_config = lattice_common::clients::vast::VastConfig {
                    base_url: url.clone(),
                    username: config.storage.vast_username.clone().unwrap_or_default(),
                    password: config.storage.vast_password.clone().unwrap_or_default(),
                    timeout_secs: config.storage.vast_timeout_secs,
                };
                match lattice_common::clients::VastClient::new(vast_config) {
                    Ok(client) => {
                        Some(Arc::new(client) as Arc<dyn lattice_common::traits::StorageService>)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to create VAST client: {e}; storage disabled");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

    // ── Rate Limiter ────────────────────────────────────────────────────
    let rate_limiter: Option<Arc<lattice_api::middleware::rate_limit::RateLimiter>> =
        if config.rate_limit.is_some() {
            info!("API rate limiting enabled");
            Some(Arc::new(
                lattice_api::middleware::rate_limit::RateLimiter::new(
                    lattice_api::middleware::rate_limit::RateLimitConfig::default(),
                ),
            ))
        } else {
            None
        };

    // ── PTY Backend ─────────────────────────────────────────────────────
    let pty: Option<Arc<dyn lattice_node_agent::pty::PtyBackend>> =
        Some(Arc::new(lattice_node_agent::pty::MockPtyBackend::new()));

    // ── TLS ───────────────────────────────────────────────────────────────
    let tls = lattice_api::server::tls_config_from_api(&config.api);

    let state = Arc::new(ApiState {
        allocations: quorum.clone(),
        nodes: quorum.clone(),
        audit: quorum.clone(),
        checkpoint: Arc::new(lattice_test_harness::mocks::MockCheckpointBroker::new()),
        quorum: Some(quorum),
        events: lattice_api::events::new_event_bus(),
        tsdb,
        storage,
        accounting,
        oidc,
        rate_limiter,
        sovra,
        pty,
        data_dir: config.quorum.data_dir.clone(),
    });

    let server_config = ServerConfig {
        grpc_addr: grpc_addr.parse()?,
        rest_addr: rest_addr.parse()?,
        tls,
    };

    serve(state, server_config)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(())
}

//! `lattice-server` — the Lattice control-plane process.
//!
//! Starts a Raft quorum member with gRPC + REST API services.
//! Configuration is loaded from a YAML file, environment variables,
//! or command-line arguments.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::info;

use lattice_api::{serve, ApiState, ServerConfig};
use lattice_common::config::LatticeConfig;
use lattice_common::error::LatticeError;
use lattice_common::tsdb_client::{VictoriaMetricsClient, VictoriaMetricsConfig};
use lattice_common::types::{
    AllocId, Allocation, AllocationState, Node, NodeId, NodeState, Tenant, TopologyModel,
};
use lattice_quorum::QuorumClient;
use lattice_scheduler::{SchedulerCommandSink, SchedulerLoopConfig, SchedulerStateReader};

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

    /// Initialize a new Raft cluster. Use only on the very first startup of
    /// node 1. All subsequent restarts must omit this flag.
    #[arg(long, default_value_t = false)]
    bootstrap: bool,
}

// ─── Scheduler ↔ Quorum adapters ─────────────────────────────────────────────

/// Reads cluster state from the Raft quorum for the scheduler loop.
struct QuorumStateReader {
    quorum: Arc<QuorumClient>,
}

#[async_trait::async_trait]
impl SchedulerStateReader for QuorumStateReader {
    async fn pending_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
        let state = self.quorum.state().read().await;
        Ok(state
            .allocations
            .values()
            .filter(|a| a.state == AllocationState::Pending)
            .cloned()
            .collect())
    }

    async fn running_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
        let state = self.quorum.state().read().await;
        Ok(state
            .allocations
            .values()
            .filter(|a| a.state == AllocationState::Running)
            .cloned()
            .collect())
    }

    async fn available_nodes(&self) -> Result<Vec<Node>, LatticeError> {
        let state = self.quorum.state().read().await;
        Ok(state
            .nodes
            .values()
            .filter(|n| n.state == NodeState::Ready)
            .cloned()
            .collect())
    }

    async fn draining_nodes(&self) -> Result<Vec<Node>, LatticeError> {
        let state = self.quorum.state().read().await;
        Ok(state
            .nodes
            .values()
            .filter(|n| matches!(n.state, NodeState::Draining))
            .cloned()
            .collect())
    }

    async fn tenants(&self) -> Result<Vec<Tenant>, LatticeError> {
        let state = self.quorum.state().read().await;
        Ok(state.tenants.values().cloned().collect())
    }

    async fn topology(&self) -> TopologyModel {
        let state = self.quorum.state().read().await;
        state.topology.clone()
    }
}

/// Applies scheduling decisions back to the Raft quorum.
struct QuorumCommandSink {
    quorum: Arc<QuorumClient>,
}

#[async_trait::async_trait]
impl SchedulerCommandSink for QuorumCommandSink {
    async fn assign_nodes(
        &self,
        alloc_id: AllocId,
        nodes: Vec<NodeId>,
    ) -> Result<(), LatticeError> {
        let resp = self
            .quorum
            .propose(lattice_quorum::QuorumCommand::AssignNodes {
                id: alloc_id,
                nodes,
                expected_version: None,
            })
            .await?;
        match resp {
            lattice_quorum::QuorumResponse::Ok => Ok(()),
            lattice_quorum::QuorumResponse::Error(e) => Err(LatticeError::Internal(e)),
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }

    async fn set_running(&self, alloc_id: AllocId) -> Result<(), LatticeError> {
        use lattice_common::traits::AllocationStore;
        self.quorum
            .update_state(&alloc_id, AllocationState::Running)
            .await
    }

    async fn suspend(&self, alloc_id: AllocId) -> Result<(), LatticeError> {
        use lattice_common::traits::AllocationStore;
        self.quorum
            .update_state(&alloc_id, AllocationState::Suspended)
            .await
    }

    async fn complete_drain(&self, node_id: String) -> Result<(), LatticeError> {
        use lattice_common::traits::NodeRegistry;
        self.quorum
            .update_node_state(&node_id, NodeState::Drained)
            .await
    }
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

    // ── Secret Resolution (INV-SEC1: before serving) ─────────────────────
    // Uses reqwest::blocking internally — must run outside tokio's async
    // context to avoid "cannot start runtime from within runtime" panic.
    let config_clone = config.clone();
    let secrets = tokio::task::spawn_blocking(move || {
        let resolver = lattice_common::secrets::SecretResolver::new(&config_clone)?;
        resolver.resolve_all(&config_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("secret resolution task failed: {e}"))?
    .map_err(|e| anyhow::anyhow!("secret resolution failed: {e}"))?;

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
    let mut quorum_config = config.quorum.clone();
    if args.bootstrap {
        quorum_config.bootstrap = true;
        info!("Bootstrap mode: will initialize new Raft cluster");
    }
    let (quorum, _raft_handle) = lattice_quorum::create_quorum_from_config(&quorum_config).await?;

    // Load audit signing key: resolved secret > file path > random (dev mode).
    // When Vault is active, missing key already caused a fatal error above.
    if let Some(ref key_bytes) = secrets.audit_signing_key {
        let mut state = quorum.state().write().await;
        state
            .load_signing_key_from_bytes(key_bytes)
            .map_err(|e| anyhow::anyhow!("audit signing key: {e}"))?;
        info!("Audit signing key loaded from secret resolver");
    } else if config.vault.is_none() {
        if let Some(ref key_path) = config.quorum.audit_signing_key_path {
            let mut state = quorum.state().write().await;
            state
                .load_signing_key_from_file(key_path)
                .map_err(|e| anyhow::anyhow!("audit signing key: {e}"))?;
            info!("Audit signing key loaded from {}", key_path.display());
        } else {
            tracing::warn!("No audit_signing_key configured — using random key (dev mode only)");
        }
    }

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
    let hmac_secret = config
        .api
        .oidc_hmac_secret
        .clone()
        .or_else(|| std::env::var("LATTICE_OIDC_HMAC_SECRET").ok());

    #[cfg(feature = "oidc")]
    let oidc: Option<Arc<dyn lattice_api::middleware::oidc::OidcValidator>> =
        if !config.api.oidc_issuer.is_empty() {
            info!("OIDC enabled: issuer={}", config.api.oidc_issuer);
            let oidc_config = lattice_api::middleware::oidc::OidcConfig {
                issuer_url: config.api.oidc_issuer.clone(),
                audience: config.api.oidc_client_id.clone().unwrap_or_default(),
                required_scopes: vec![],
            };
            let validator = lattice_api::middleware::oidc::JwtOidcValidator::new(oidc_config);
            // Pre-fetch JWKS so the gRPC sync interceptor can validate immediately
            if let Err(e) = validator.prefetch_jwks().await {
                tracing::warn!("JWKS prefetch failed (will retry on first request): {e}");
            }
            Some(Arc::new(validator) as Arc<dyn lattice_api::middleware::oidc::OidcValidator>)
        } else if let Some(ref secret) = hmac_secret {
            info!("HMAC token validation enabled (dev/internal mode)");
            Some(
                Arc::new(lattice_api::middleware::oidc::HmacOidcValidator::new(
                    secret, None,
                )) as Arc<dyn lattice_api::middleware::oidc::OidcValidator>,
            )
        } else {
            tracing::warn!(
                "No OIDC authentication configured -- API is unauthenticated (dev mode only)"
            );
            None
        };
    #[cfg(not(feature = "oidc"))]
    let oidc: Option<Arc<dyn lattice_api::middleware::oidc::OidcValidator>> =
        if let Some(ref secret) = hmac_secret {
            info!("HMAC token validation enabled (dev/internal mode)");
            Some(
                Arc::new(lattice_api::middleware::oidc::HmacOidcValidator::new(
                    secret, None,
                )) as Arc<dyn lattice_api::middleware::oidc::OidcValidator>,
            )
        } else {
            tracing::warn!(
                "No OIDC authentication configured -- API is unauthenticated (dev mode only)"
            );
            None
        };

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
                    api_token: secrets
                        .waldur_token
                        .as_ref()
                        .map(|s| s.expose().to_string())
                        .unwrap_or_default(),
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
                    username: secrets
                        .vast_username
                        .as_ref()
                        .map(|s| s.expose().to_string())
                        .unwrap_or_default(),
                    password: secrets
                        .vast_password
                        .as_ref()
                        .map(|s| s.expose().to_string())
                        .unwrap_or_default(),
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

    // ── Checkpoint Broker ──────────────────────────────────────────────
    let checkpoint_broker = lattice_checkpoint::LatticeCheckpointBroker::new(
        lattice_checkpoint::CheckpointParams::default(),
    )
    .with_allocation_store(quorum.clone() as Arc<dyn lattice_common::traits::AllocationStore>);
    let checkpoint: Arc<dyn lattice_common::traits::CheckpointBroker> = Arc::new(checkpoint_broker);
    info!("Checkpoint broker initialized");

    // ── PTY Backend ─────────────────────────────────────────────────────
    let pty: Option<Arc<dyn lattice_node_agent::pty::PtyBackend>> =
        Some(Arc::new(lattice_node_agent::pty::ProcessPtyBackend::new()));

    // ── TLS ───────────────────────────────────────────────────────────────
    let tls = lattice_api::server::tls_config_from_api(&config.api);

    let state = Arc::new(ApiState {
        allocations: quorum.clone(),
        nodes: quorum.clone(),
        audit: quorum.clone(),
        checkpoint,
        quorum: Some(quorum.clone()),
        events: lattice_api::events::new_event_bus(),
        tsdb,
        storage,
        accounting,
        oidc,
        rate_limiter,
        sovra,
        pty,
        agent_pool: None,
        data_dir: config.quorum.data_dir.clone(),
        oidc_config: if !config.api.oidc_issuer.is_empty() {
            Some(lattice_api::middleware::oidc::OidcConfig {
                issuer_url: config.api.oidc_issuer.clone(),
                audience: config.api.oidc_client_id.clone().unwrap_or_default(),
                required_scopes: vec![],
            })
        } else if hmac_secret.is_some() {
            // HMAC mode: create a minimal OidcConfig so auth enforcement is enabled
            Some(lattice_api::middleware::oidc::OidcConfig {
                issuer_url: String::new(),
                audience: String::new(),
                required_scopes: vec![],
            })
        } else {
            None
        },
    });

    let server_config = ServerConfig {
        grpc_addr: grpc_addr.parse()?,
        rest_addr: rest_addr.parse()?,
        tls,
    };

    // ── Scheduler Loop ────────────────────────────────────────────────────
    let sched_config = config.scheduling.unwrap_or_default();
    let scheduler_loop = lattice_scheduler::SchedulerLoop::new(
        Arc::new(QuorumStateReader {
            quorum: quorum.clone(),
        }),
        Arc::new(QuorumCommandSink {
            quorum: quorum.clone(),
        }),
        SchedulerLoopConfig {
            tick_interval: Duration::from_secs(sched_config.cycle_interval_seconds),
            ..Default::default()
        },
    );

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    // Graceful shutdown on Ctrl+C
    let cancel_tx_clone = cancel_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received SIGINT, shutting down");
        let _ = cancel_tx_clone.send(true);
    });

    info!(
        "Scheduler loop starting (interval={}s)",
        sched_config.cycle_interval_seconds
    );

    // Run scheduler loop and API servers concurrently
    tokio::select! {
        result = serve(state, server_config) => {
            result.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        _ = scheduler_loop.run(cancel_rx) => {
            info!("Scheduler loop stopped");
        }
    }

    drop(cancel_tx);
    Ok(())
}

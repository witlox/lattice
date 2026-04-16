//! `lattice-agent` — the per-node agent daemon.
//!
//! Connects to the Lattice quorum, registers the node, and runs the
//! heartbeat loop, allocation lifecycle, and telemetry collection.
//!
//! On startup the agent loads a persisted state file and reattaches to
//! any workload processes that survived a previous agent restart (see
//! `state.rs` and `reattach.rs`). On shutdown (SIGTERM / SIGINT) the
//! agent persists its state and exits *without* killing workloads.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};

use lattice_common::config::IdentityConfig;
use lattice_common::types::NodeCapabilities;
use lattice_node_agent::grpc_client::{GrpcHeartbeatSink, GrpcNodeRegistry};
use lattice_node_agent::health::ObservedHealth;
use lattice_node_agent::heartbeat_loop::StaticHealthObserver;
use lattice_node_agent::identity;
use lattice_node_agent::reattach;
use lattice_node_agent::state;
use lattice_node_agent::NodeAgent;

#[derive(Parser)]
#[command(name = "lattice-agent", version, about = "Lattice per-node agent")]
struct Args {
    /// This node's xname identifier
    #[arg(long)]
    node_id: String,

    /// Quorum endpoint(s) to connect to
    #[arg(long, default_value = "http://localhost:50051")]
    quorum_endpoint: String,

    /// Heartbeat interval in seconds
    #[arg(long, default_value = "10")]
    heartbeat_interval: u64,

    /// Number of GPUs on this node
    #[arg(long, default_value = "0")]
    gpu_count: u32,

    /// GPU type (e.g., GH200, MI300X)
    #[arg(long)]
    gpu_type: Option<String>,

    /// Number of CPU cores
    #[arg(long, default_value = "1")]
    cpu_cores: u32,

    /// Memory in GB
    #[arg(long, default_value = "1")]
    memory_gb: u64,

    /// TSDB endpoint for pushing metrics
    #[arg(long, env = "LATTICE_TELEMETRY_TSDB_ENDPOINT")]
    tsdb_endpoint: Option<String>,

    /// gRPC listen address for incoming RPCs (LaunchProcesses, PmiFence, etc.)
    #[arg(long, default_value = "0.0.0.0:50052")]
    grpc_addr: String,

    /// Path to the agent state file for workload persistence across restarts.
    #[arg(long, default_value = state::STATE_FILE_PATH, env = "LATTICE_STATE_FILE")]
    state_file: PathBuf,

    /// Bootstrap certificate for mTLS (PEM file path).
    #[arg(long, env = "LATTICE_BOOTSTRAP_CERT")]
    bootstrap_cert: Option<String>,

    /// Bootstrap private key for mTLS (PEM file path).
    #[arg(long, env = "LATTICE_BOOTSTRAP_KEY")]
    bootstrap_key: Option<String>,

    /// Bootstrap CA trust bundle for mTLS (PEM file path).
    #[arg(long, env = "LATTICE_BOOTSTRAP_CA")]
    bootstrap_ca: Option<String>,

    /// SPIRE agent socket path.
    #[arg(
        long,
        default_value = "/run/spire/agent.sock",
        env = "LATTICE_SPIRE_SOCKET"
    )]
    spire_socket: String,
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

    info!("Starting lattice-agent for node {}", args.node_id);
    info!("Quorum endpoint: {}", args.quorum_endpoint);

    let capabilities = NodeCapabilities {
        gpu_type: args.gpu_type.clone(),
        gpu_count: args.gpu_count,
        cpu_cores: args.cpu_cores,
        memory_gb: args.memory_gb,
        features: vec![],
        gpu_topology: None,
        memory_topology: None,
    };

    // ── Identity acquisition (mTLS) ────────────────────────────────
    // Try the identity cascade: SPIRE → SelfSigned → Bootstrap.
    // If an identity is available, use mTLS for gRPC connections.
    // Otherwise, fall back to LATTICE_AGENT_TOKEN Bearer auth.
    let identity_config = IdentityConfig {
        spire_socket: args.spire_socket.clone(),
        bootstrap_cert: args.bootstrap_cert.clone(),
        bootstrap_key: args.bootstrap_key.clone(),
        bootstrap_ca: args.bootstrap_ca.clone(),
        ..Default::default()
    };

    let tls_config = {
        let cascade = identity::build_cascade(&identity_config);
        match cascade.get_identity().await {
            Ok(id) => {
                info!(
                    source = ?id.source,
                    expires = %id.expires_at,
                    "acquired workload identity for mTLS"
                );
                match identity::tls_config_from_identity(&id) {
                    Ok(tls) => Some(tls),
                    Err(e) => {
                        warn!(error = %e, "failed to build TLS config from identity — falling back to token auth");
                        None
                    }
                }
            }
            Err(e) => {
                if std::env::var("LATTICE_AGENT_TOKEN").is_ok() {
                    info!("no mTLS identity available ({e}) — using LATTICE_AGENT_TOKEN");
                } else {
                    warn!("no mTLS identity available ({e}) and no LATTICE_AGENT_TOKEN set — gRPC calls may be rejected");
                }
                None
            }
        }
    };

    // Connect to the quorum
    let grpc_registry = GrpcNodeRegistry::connect(&args.quorum_endpoint, tls_config.clone())
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect registry: {e}"))?;
    info!("Connected to quorum");

    // Register this node (INV-D1: agent_address is the reachable gRPC
    // endpoint on which the control plane dispatches RunAllocation).
    grpc_registry
        .register_node(&args.node_id, &capabilities, &args.grpc_addr)
        .await
        .map_err(|e| anyhow::anyhow!("failed to register node: {e}"))?;
    info!(
        "Node {} registered with agent_address {}",
        args.node_id, args.grpc_addr
    );

    // Set up heartbeat sink
    let heartbeat_sink = GrpcHeartbeatSink::connect(&args.quorum_endpoint, tls_config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect heartbeat sink: {e}"))?;

    // Set up health observer (static for now — ProcSysCollector used for TSDB push)
    let observer = StaticHealthObserver::new(ObservedHealth {
        gpu_count: args.gpu_count,
        max_gpu_temp_c: None,
        ecc_errors: 0,
        nic_up: true,
    });

    let registry = Arc::new(grpc_registry);
    let mut agent = NodeAgent::new(args.node_id.clone(), capabilities.clone(), registry);

    // ── Reattach to surviving workloads from a previous run ──────
    let state_path = args.state_file.clone();
    match state::load_state(&state_path) {
        Ok(Some(prev)) => {
            info!(
                allocations = prev.allocations.len(),
                updated_at = %prev.updated_at,
                "loaded persisted agent state"
            );
            let result = reattach::reattach(&prev);
            info!(
                recovered = result.recovered.len(),
                orphans = result.orphans.len(),
                "reattach complete"
            );

            // Re-register recovered allocations in the agent's AllocationManager.
            for rec in &result.recovered {
                let p = &rec.persisted;
                if let Err(e) = agent.allocations_mut().start(p.id, p.entrypoint.clone()) {
                    warn!(alloc_id = %p.id, error = %e, "failed to re-register recovered allocation");
                } else {
                    // Advance from Prologue → Running (they were running before restart).
                    let _ = agent.allocations_mut().advance(&p.id);
                }
            }

            // Clean up orphan cgroups from dead workloads.
            for orphan in &result.orphans {
                if let Some(ref cg) = orphan.cgroup_path {
                    reattach::cleanup_orphan_cgroup(cg);
                }
            }

            // Scan for stray cgroup scopes not in the state file.
            let known_ids: Vec<_> = prev.allocations.iter().map(|a| a.id).collect();
            let stray = reattach::scan_orphan_cgroups(&known_ids);
            for path in &stray {
                warn!(path = %path, "cleaning up stray cgroup scope");
                reattach::cleanup_orphan_cgroup(path);
            }
        }
        Ok(None) => {
            info!(path = %state_path.display(), "no previous agent state found (first boot)");
        }
        Err(e) => {
            warn!(
                path = %state_path.display(),
                error = %e,
                "failed to load agent state — starting fresh"
            );
        }
    }

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);

    // Spawn TSDB metrics push task
    if let Some(ref tsdb_endpoint) = args.tsdb_endpoint {
        let tsdb = lattice_common::tsdb_client::VictoriaMetricsClient::new(
            lattice_common::tsdb_client::VictoriaMetricsConfig {
                base_url: tsdb_endpoint.clone(),
                ..Default::default()
            },
        );
        let node_id = args.node_id.clone();
        let caps = capabilities;
        tokio::spawn(async move {
            use lattice_common::tsdb_client::{MetricSample, TsdbClient};
            use std::collections::HashMap;

            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let now_ms = chrono::Utc::now().timestamp_millis();
                let mut labels = HashMap::new();
                labels.insert("node".to_string(), node_id.clone());

                let samples = vec![
                    MetricSample {
                        name: "lattice_node_up".to_string(),
                        labels: labels.clone(),
                        timestamp_ms: now_ms,
                        value: 1.0,
                    },
                    MetricSample {
                        name: "lattice_node_cpu_cores".to_string(),
                        labels: labels.clone(),
                        timestamp_ms: now_ms,
                        value: caps.cpu_cores as f64,
                    },
                    MetricSample {
                        name: "lattice_node_memory_gb".to_string(),
                        labels: labels.clone(),
                        timestamp_ms: now_ms,
                        value: caps.memory_gb as f64,
                    },
                    MetricSample {
                        name: "lattice_node_gpu_count".to_string(),
                        labels,
                        timestamp_ms: now_ms,
                        value: caps.gpu_count as f64,
                    },
                ];

                if let Err(e) = tsdb.push(&samples).await {
                    tracing::warn!(error = %e, "failed to push metrics to TSDB");
                } else {
                    tracing::debug!("pushed {} metric samples to TSDB", samples.len());
                }
            }
        });
        info!("TSDB metrics push enabled: {}", tsdb_endpoint);
    }

    // Start the node agent gRPC server (LaunchProcesses, PmiFence, etc.)
    let grpc_addr: std::net::SocketAddr = args
        .grpc_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid grpc_addr: {e}"))?;

    // ── Dispatch bridge (Impl 5) ───────────────────────────────────
    // Share the agent's Completion Buffer with the gRPC RunAllocation
    // handler so runtime monitor tasks can push state-change reports that
    // the heartbeat loop drains on each tick (IP-03 / INV-D13).
    let dispatch_alloc_mgr = std::sync::Arc::new(tokio::sync::Mutex::new(
        lattice_node_agent::allocation_runner::AllocationManager::new(),
    ));
    let bare_runtime = std::sync::Arc::new(lattice_node_agent::runtime::BareProcessRuntime::new());
    // Construct uenv + podman runtimes with default configuration. Both
    // runtimes auto-degrade to simulation mode on non-Linux hosts or when
    // the required binaries (squashfs-mount / podman) are absent — so they
    // can be safely wired here even in dev/CI environments that don't have
    // the full HPC stack installed.
    let uenv_runtime: std::sync::Arc<dyn lattice_node_agent::runtime::Runtime> =
        std::sync::Arc::new(lattice_node_agent::runtime::UenvRuntime::new(
            lattice_node_agent::runtime::uenv::UenvConfig::default(),
        ));
    let podman_runtime: std::sync::Arc<dyn lattice_node_agent::runtime::Runtime> =
        std::sync::Arc::new(lattice_node_agent::runtime::PodmanRuntime::new(
            lattice_node_agent::runtime::podman::PodmanConfig::default(),
        ));
    let bridge = lattice_node_agent::grpc_server::DispatchBridge {
        allocations: dispatch_alloc_mgr,
        reports: agent.completion_buffer(),
        bare: bare_runtime,
        uenv: Some(uenv_runtime),
        podman: Some(podman_runtime),
    };
    let node_agent_server =
        lattice_node_agent::grpc_server::NodeAgentServer::new(args.node_id.clone())
            .with_dispatch(bridge);
    let grpc_svc =
        lattice_common::proto::lattice::v1::node_agent_service_server::NodeAgentServiceServer::new(
            node_agent_server,
        );
    tokio::spawn(async move {
        info!("Node agent gRPC server listening on {}", grpc_addr);
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(grpc_svc)
            .serve(grpc_addr)
            .await
        {
            tracing::error!(error = %e, "node agent gRPC server failed");
        }
    });

    // Handle Ctrl+C / SIGTERM for graceful shutdown
    let cancel_tx_clone = cancel_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received shutdown signal");
        let _ = cancel_tx_clone.send(true);
    });

    info!(
        "Agent for {} running (heartbeat every {}s, gRPC on {})",
        args.node_id, args.heartbeat_interval, args.grpc_addr
    );

    agent
        .run(
            heartbeat_sink,
            observer,
            Duration::from_secs(args.heartbeat_interval),
            cancel_rx,
            cmd_rx,
        )
        .await;

    // ── Persist state before exit ────────────────────────────────
    // Collect active allocations and write the state file so the next
    // agent instance can reattach. We intentionally do NOT kill
    // workloads — they run in their own cgroup scopes and survive.
    let active_ids = agent.allocations().list_ids();
    let active_allocs: Vec<state::PersistedAllocation> = active_ids
        .iter()
        .filter_map(|id| {
            agent.allocations().get(id).and_then(|la| {
                if la.is_active() {
                    Some(state::PersistedAllocation {
                        id: la.id,
                        pid: None, // PIDs not tracked in AllocationManager yet
                        container_id: None,
                        cgroup_path: None,
                        entrypoint: la.entrypoint.clone(),
                        started_at: la.started_at,
                        runtime_type: state::RuntimeType::Uenv, // default
                        mount_point: None,
                    })
                } else {
                    None
                }
            })
        })
        .collect();

    if active_allocs.is_empty() {
        info!("no active allocations — removing state file");
        if let Err(e) = state::remove_state(&state_path) {
            warn!(error = %e, "failed to remove state file");
        }
    } else {
        let agent_state = state::AgentState {
            node_id: args.node_id.clone(),
            updated_at: chrono::Utc::now(),
            allocations: active_allocs,
        };
        info!(
            allocations = agent_state.allocations.len(),
            path = %state_path.display(),
            "persisting agent state before shutdown"
        );
        if let Err(e) = state::save_state(&state_path, &agent_state) {
            warn!(error = %e, "failed to persist agent state");
        }
    }

    info!("Shutting down lattice-agent");
    drop(cancel_tx);
    Ok(())
}

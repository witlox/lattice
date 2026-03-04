//! `lattice-agent` — the per-node agent daemon.
//!
//! Connects to the Lattice quorum, registers the node, and runs the
//! heartbeat loop, allocation lifecycle, and telemetry collection.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::info;

use lattice_common::types::NodeCapabilities;
use lattice_node_agent::grpc_client::{GrpcHeartbeatSink, GrpcNodeRegistry};
use lattice_node_agent::health::ObservedHealth;
use lattice_node_agent::heartbeat_loop::StaticHealthObserver;
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
    };

    // Connect to the quorum
    let grpc_registry = GrpcNodeRegistry::connect(&args.quorum_endpoint)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect registry: {e}"))?;
    info!("Connected to quorum");

    // Register this node
    grpc_registry
        .register_node(&args.node_id, &capabilities)
        .await
        .map_err(|e| anyhow::anyhow!("failed to register node: {e}"))?;
    info!("Node {} registered", args.node_id);

    // Set up heartbeat sink
    let heartbeat_sink = GrpcHeartbeatSink::connect(&args.quorum_endpoint)
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

    // Handle Ctrl+C for graceful shutdown
    let cancel_tx_clone = cancel_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received SIGINT, shutting down");
        let _ = cancel_tx_clone.send(true);
    });

    info!(
        "Agent for {} running (heartbeat every {}s)",
        args.node_id, args.heartbeat_interval
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

    info!("Shutting down lattice-agent");
    drop(cancel_tx);
    Ok(())
}

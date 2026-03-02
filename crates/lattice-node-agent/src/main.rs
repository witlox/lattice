//! `lattice-agent` — the per-node agent daemon.
//!
//! Connects to the Lattice quorum, registers the node, and runs the
//! heartbeat loop, allocation lifecycle, and telemetry collection.

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing::info;

use lattice_common::types::NodeCapabilities;
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
        gpu_type: args.gpu_type,
        gpu_count: args.gpu_count,
        cpu_cores: args.cpu_cores,
        memory_gb: args.memory_gb,
        features: vec![],
        gpu_topology: None,
    };

    // For now, use a mock registry. S06 will wire this to a real quorum client.
    let registry = Arc::new(lattice_test_harness::mocks::MockNodeRegistry::new());

    let _agent = NodeAgent::new(args.node_id.clone(), capabilities, registry);

    info!(
        "Agent for {} initialized, waiting for commands",
        args.node_id
    );

    // Keep the process alive until terminated.
    // S06 will add the heartbeat loop here.
    tokio::signal::ctrl_c().await?;
    info!("Shutting down lattice-agent");

    Ok(())
}

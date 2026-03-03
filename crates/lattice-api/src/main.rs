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

    // For now, use in-memory test quorum. S03 will wire this to real Raft with gRPC transport.
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
        accounting: None,
        oidc: None,
        rate_limiter: None,
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

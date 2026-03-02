//! Combined gRPC + REST server.
//!
//! Builds a tonic gRPC server with all three services and an axum
//! REST gateway, sharing the same backing state.

use std::net::SocketAddr;
use std::sync::Arc;

use tonic::transport::Server as TonicServer;

use lattice_common::proto::lattice::v1::admin_service_server::AdminServiceServer;
use lattice_common::proto::lattice::v1::allocation_service_server::AllocationServiceServer;
use lattice_common::proto::lattice::v1::node_service_server::NodeServiceServer;

use crate::grpc::admin_service::LatticeAdminService;
use crate::grpc::allocation_service::LatticeAllocationService;
use crate::grpc::node_service::LatticeNodeService;
use crate::rest;
use crate::state::ApiState;

/// Server configuration.
pub struct ServerConfig {
    /// gRPC listen address
    pub grpc_addr: SocketAddr,
    /// REST listen address
    pub rest_addr: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            grpc_addr: "0.0.0.0:50051".parse().unwrap(),
            rest_addr: "0.0.0.0:8080".parse().unwrap(),
        }
    }
}

/// Build the gRPC server (does not start listening).
pub fn build_grpc_server(state: Arc<ApiState>) -> TonicServer {
    let _ = state; // Used in serve_grpc
    TonicServer::builder()
}

/// Start the gRPC server.
pub async fn serve_grpc(
    state: Arc<ApiState>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let alloc_svc = LatticeAllocationService::new(state.clone());
    let node_svc = LatticeNodeService::new(state.clone());
    let admin_svc = LatticeAdminService::new(state);

    tracing::info!("gRPC server listening on {}", addr);

    TonicServer::builder()
        .add_service(AllocationServiceServer::new(alloc_svc))
        .add_service(NodeServiceServer::new(node_svc))
        .add_service(AdminServiceServer::new(admin_svc))
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
        result = serve_grpc(grpc_state, config.grpc_addr) => result,
        result = serve_rest(rest_state, config.rest_addr) => result,
    }
}

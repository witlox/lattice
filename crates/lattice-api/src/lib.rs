//! # lattice-api
//!
//! gRPC + REST gateway for the Lattice scheduler.
//!
//! Implements three gRPC services from the proto definitions:
//! - `AllocationService` (18 RPCs): submit, get, list, cancel, watch, checkpoint, etc.
//! - `NodeService` (5 RPCs): list, get, drain, undrain, disable
//! - `AdminService` (6 RPCs): tenant/vCluster CRUD, Raft status, backup verify
//!
//! Also provides a JSON REST gateway via axum for CLI and browser clients.

pub mod convert;
pub mod diagnostics;
pub mod events;
pub mod grpc;
pub mod middleware;
pub mod mpi;
pub mod rest;
pub mod server;
pub mod state;

pub use events::{AllocationEvent, EventBus};
pub use grpc::admin_service::LatticeAdminService;
pub use grpc::allocation_service::LatticeAllocationService;
pub use grpc::node_service::LatticeNodeService;
pub use server::{
    serve, serve_grpc, serve_grpc_with_tls, serve_rest, BearerToken, MiddlewareLayer,
    RequestOperation, ServerConfig, TlsConfig, TlsConfigError,
};
pub use state::ApiState;

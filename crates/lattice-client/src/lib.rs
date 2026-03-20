//! Rust gRPC client SDK for the Lattice distributed workload scheduler.
//!
//! This crate provides a type-safe client for all Lattice API operations,
//! covering allocations, DAGs, sessions, nodes, tenants, vClusters, audit,
//! accounting, and cluster administration.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), lattice_client::LatticeClientError> {
//! use lattice_client::{LatticeClient, ClientConfig};
//!
//! let config = ClientConfig {
//!     endpoint: "http://lattice-api:50051".to_string(),
//!     token: Some("my-token".to_string()),
//!     ..Default::default()
//! };
//! let mut client = LatticeClient::connect(config).await?;
//!
//! // List nodes
//! let nodes = client.list_nodes(Default::default()).await?;
//! println!("Nodes: {}", nodes.nodes.len());
//!
//! // Enable a disabled node
//! client.enable_node("x1000c0s0b0n0").await?;
//! # Ok(())
//! # }
//! ```

mod auth;
mod client;
mod config;
mod error;

pub use client::LatticeClient;
pub use config::ClientConfig;
pub use error::LatticeClientError;

// Re-export proto types for convenience — callers shouldn't need to depend on
// lattice-common directly.
pub use lattice_common::proto::lattice::v1 as proto;

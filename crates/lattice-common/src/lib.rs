//! # lattice-common
//!
//! Shared types, configuration, and error handling for the Lattice scheduler.
//!
//! This crate defines the core domain model used across all Lattice components:
//! - **Allocation**: The universal work unit (replaces Slurm job + K8s pod)
//! - **Node**: Physical compute node with capabilities and ownership
//! - **Tenant**: Organizational boundary with quotas
//! - **VCluster**: Logical cluster with its own scheduling policy
//! - **TopologyModel**: Slingshot/UE dragonfly group structure

pub mod clients;
pub mod config;
pub mod error;
pub mod proto;
pub mod registry;
#[cfg(feature = "scheduler-core")]
pub mod scheduler_core_impls;
pub mod secrets;
pub mod traits;
pub mod tsdb_client;
pub mod types;
pub mod uenv_metadata;

pub use error::LatticeError;
pub use types::*;

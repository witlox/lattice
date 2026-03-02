//! # lattice-node-agent
//!
//! Per-node daemon that manages allocation lifecycle, heartbeats,
//! health monitoring, conformance fingerprinting, and checkpoint signaling.
//!
//! See docs/architecture/node-lifecycle.md for the state machine design.

pub mod agent;
pub mod allocation_runner;
pub mod checkpoint_handler;
pub mod conformance;
pub mod health;
pub mod heartbeat;
pub mod network;
pub mod telemetry;

pub use agent::NodeAgent;

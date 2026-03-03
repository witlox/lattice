//! # lattice-checkpoint
//!
//! Checkpoint broker: cost-function-driven checkpoint coordination.
//!
//! Implements the checkpoint decision engine from
//! `docs/architecture/checkpoint-broker.md`:
//! - Cost model: `Should_checkpoint = Value > Cost`
//! - Three communication protocols (signal, shmem, gRPC callback)
//! - Per-allocation checkpoint policy evaluation
//! - Storage destination path generation (S3/NFS)

pub mod broker;
pub mod cost_model;
pub mod loop_runner;
pub mod policy;
pub mod protocol;

pub use broker::{LatticeCheckpointBroker, NodeAgentPool};
pub use cost_model::{evaluate_checkpoint, CheckpointEvaluation, CheckpointParams};
pub use loop_runner::CheckpointLoop;
pub use policy::{evaluate_policy, CheckpointPolicy};
pub use protocol::{
    checkpoint_destination, CheckpointDestination, CheckpointProtocol, CheckpointRequest,
    CheckpointResponse,
};

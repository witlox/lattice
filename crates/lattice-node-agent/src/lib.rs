//! # lattice-node-agent
//!
//! Per-node daemon that manages allocation lifecycle, heartbeats,
//! health monitoring, conformance fingerprinting, and checkpoint signaling.
//!
//! See docs/architecture/node-lifecycle.md for the state machine design.

pub mod agent;
pub mod allocation_runner;
pub mod attach;
pub mod checkpoint_handler;
pub mod conformance;
pub mod epilogue;
pub mod grpc_client;
pub mod health;
pub mod heartbeat;
pub mod heartbeat_loop;
pub mod image_cache;
pub mod network;
pub mod prologue;
pub mod pty;
pub mod runtime;
pub mod signal;
pub mod telemetry;

pub use agent::NodeAgent;
pub use attach::{AttachManager, AttachSession};
pub use heartbeat_loop::{HealthObserver, HeartbeatLoop, HeartbeatSink};
pub use pty::{MockPtyBackend, PtyBackend, PtyError, TerminalSize};
pub use runtime::{
    ExitStatus, MockRuntime, PrepareConfig, ProcessHandle, Runtime, RuntimeError, SarusRuntime,
    UenvRuntime,
};
pub use telemetry::ebpf_stubs::{CollectorState, EbpfCollector, EbpfEvent, StubEbpfCollector};
pub use telemetry::log_buffer::{LogRingBuffer, S3Sink};

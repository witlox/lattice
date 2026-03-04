//! Runtime abstraction for process execution.
//!
//! The [`Runtime`] trait defines the lifecycle for executing an allocation's
//! workload on a node: prepare the environment, spawn the entrypoint process,
//! send signals, and stop the process.
//!
//! Implementations:
//! - [`UenvRuntime`]: SquashFS mount namespace execution via `squashfs-mount` + `nsenter`.
//! - [`SarusRuntime`]: OCI container execution via the Sarus container runtime.
//! - [`MockRuntime`]: In-memory mock for testing.

pub mod dmtcp;
pub mod mock;
pub mod sarus;
pub mod uenv;

pub use dmtcp::DmtcpRuntime;
pub use mock::{MockConfig, MockRuntime};
pub use sarus::SarusRuntime;
pub use uenv::UenvRuntime;

use async_trait::async_trait;
use lattice_common::types::AllocId;

/// Exit status from a runtime process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    /// Process exited with the given code (0 = success).
    Code(i32),
    /// Process was killed by a signal.
    Signal(i32),
    /// Process state is unknown (e.g., lost contact).
    Unknown,
}

impl ExitStatus {
    /// Whether the process exited successfully (code 0).
    pub fn success(&self) -> bool {
        matches!(self, ExitStatus::Code(0))
    }
}

/// Configuration for preparing a runtime environment.
#[derive(Debug, Clone)]
pub struct PrepareConfig {
    /// Allocation ID.
    pub alloc_id: AllocId,
    /// uenv image reference (e.g., "prgenv-gnu/24.11:v1").
    pub uenv: Option<String>,
    /// uenv view to activate.
    pub view: Option<String>,
    /// OCI image reference (for Sarus).
    pub image: Option<String>,
    /// Working directory inside the environment.
    pub workdir: Option<String>,
    /// Environment variables to set.
    pub env_vars: Vec<(String, String)>,
    /// Memory binding policy (numactl).
    pub memory_policy: Option<lattice_common::types::MemoryPolicy>,
    /// Whether the node has unified memory (skips numactl).
    pub is_unified_memory: bool,
}

/// Handle to a running process managed by a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessHandle {
    /// Allocation this process belongs to.
    pub alloc_id: AllocId,
    /// OS process ID (if available).
    pub pid: Option<u32>,
    /// Container ID (Sarus) or namespace ID (uenv).
    pub container_id: Option<String>,
}

/// Errors from runtime operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeError {
    #[error("prepare failed for {alloc_id}: {reason}")]
    PrepareFailed { alloc_id: AllocId, reason: String },

    #[error("spawn failed for {alloc_id}: {reason}")]
    SpawnFailed { alloc_id: AllocId, reason: String },

    #[error("stop failed for {alloc_id}: {reason}")]
    StopFailed { alloc_id: AllocId, reason: String },

    #[error("signal failed for {alloc_id}: {reason}")]
    SignalFailed { alloc_id: AllocId, reason: String },

    #[error("allocation {alloc_id} not found")]
    NotFound { alloc_id: AllocId },
}

/// Trait for a container/namespace runtime that manages process execution.
///
/// The lifecycle is: `prepare()` → `spawn()` → (optional `signal()`) → `stop()`.
/// After `stop()` or when the process exits naturally, `wait()` returns the exit status.
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Prepare the execution environment (pull image, mount SquashFS, create namespaces).
    /// This corresponds to the allocation's Prologue phase.
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError>;

    /// Spawn the entrypoint command inside the prepared environment.
    /// Returns a handle that can be used to signal or stop the process.
    async fn spawn(
        &self,
        alloc_id: AllocId,
        entrypoint: &str,
        args: &[String],
    ) -> Result<ProcessHandle, RuntimeError>;

    /// Send a signal to a running process (e.g., SIGUSR1 for checkpoint).
    async fn signal(&self, handle: &ProcessHandle, signal: i32) -> Result<(), RuntimeError>;

    /// Stop a running process. Sends SIGTERM, waits up to `grace_secs`, then SIGKILL.
    async fn stop(
        &self,
        handle: &ProcessHandle,
        grace_secs: u32,
    ) -> Result<ExitStatus, RuntimeError>;

    /// Wait for a process to exit and return its exit status.
    async fn wait(&self, handle: &ProcessHandle) -> Result<ExitStatus, RuntimeError>;

    /// Clean up the environment after the process has exited (unmount, remove dirs).
    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_status_success() {
        assert!(ExitStatus::Code(0).success());
        assert!(!ExitStatus::Code(1).success());
        assert!(!ExitStatus::Code(-1).success());
        assert!(!ExitStatus::Signal(9).success());
        assert!(!ExitStatus::Unknown.success());
    }

    #[test]
    fn prepare_config_builder() {
        let config = PrepareConfig {
            alloc_id: uuid::Uuid::new_v4(),
            uenv: Some("prgenv-gnu/24.11:v1".to_string()),
            view: Some("default".to_string()),
            image: None,
            workdir: Some("/workspace".to_string()),
            env_vars: vec![("CUDA_VISIBLE_DEVICES".to_string(), "0,1".to_string())],
            memory_policy: None,
            is_unified_memory: false,
        };
        assert!(config.uenv.is_some());
        assert!(config.image.is_none());
        assert_eq!(config.env_vars.len(), 1);
    }

    #[test]
    fn process_handle_fields() {
        let handle = ProcessHandle {
            alloc_id: uuid::Uuid::new_v4(),
            pid: Some(12345),
            container_id: None,
        };
        assert_eq!(handle.pid, Some(12345));
        assert!(handle.container_id.is_none());
    }

    #[test]
    fn runtime_error_display() {
        let id = uuid::Uuid::new_v4();
        let err = RuntimeError::PrepareFailed {
            alloc_id: id,
            reason: "image not found".to_string(),
        };
        assert!(err.to_string().contains("prepare failed"));
        assert!(err.to_string().contains("image not found"));

        let err = RuntimeError::SpawnFailed {
            alloc_id: id,
            reason: "exec failed".to_string(),
        };
        assert!(err.to_string().contains("spawn failed"));

        let err = RuntimeError::NotFound { alloc_id: id };
        assert!(err.to_string().contains("not found"));
    }
}

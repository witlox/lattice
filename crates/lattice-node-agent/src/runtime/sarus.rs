//! Sarus runtime — OCI container execution.
//!
//! Uses the Sarus container runtime for workloads that need OCI isolation,
//! third-party images, or when uenv is not suitable. Commands:
//! - Prepare: `sarus pull <image>`
//! - Spawn:   `sarus run [--mount=...] <image> <entrypoint> [args...]`
//! - Stop:    `sarus stop <container-id>` or `kill -TERM <pid>`
//! - Cleanup: `sarus rmi <image>` (optional, image cache managed separately)

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{ExitStatus, PrepareConfig, ProcessHandle, Runtime, RuntimeError};
use lattice_common::types::AllocId;

/// State tracked per Sarus container.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used when real OS commands are wired in
struct SarusState {
    /// OCI image reference.
    image_ref: String,
    /// Container ID assigned by Sarus.
    container_id: Option<String>,
    /// OS PID of the container's init process.
    pid: Option<u32>,
    /// Whether the container has been stopped.
    stopped: bool,
}

/// Configuration for the Sarus runtime.
#[derive(Debug, Clone)]
pub struct SarusConfig {
    /// Path to the `sarus` binary.
    pub sarus_bin: String,
    /// Additional Sarus flags (e.g., "--mpi", "--mount=type=bind,...").
    pub extra_flags: Vec<String>,
}

impl Default for SarusConfig {
    fn default() -> Self {
        Self {
            sarus_bin: "/usr/bin/sarus".to_string(),
            extra_flags: Vec::new(),
        }
    }
}

/// Sarus OCI container runtime implementation.
pub struct SarusRuntime {
    config: SarusConfig,
    states: Arc<RwLock<HashMap<AllocId, SarusState>>>,
}

impl SarusRuntime {
    pub fn new(config: SarusConfig) -> Self {
        Self {
            config,
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Build the `sarus pull` command args.
    pub fn pull_args(&self, image_ref: &str) -> Vec<String> {
        vec!["pull".to_string(), image_ref.to_string()]
    }

    /// Build the `sarus run` command args.
    pub fn run_args(
        &self,
        image_ref: &str,
        entrypoint: &str,
        args: &[String],
        env_vars: &[(String, String)],
    ) -> Vec<String> {
        let mut cmd_args = vec!["run".to_string()];

        // Add extra flags (e.g., --mpi)
        cmd_args.extend(self.config.extra_flags.iter().cloned());

        // Add environment variables
        for (key, value) in env_vars {
            cmd_args.push("--env".to_string());
            cmd_args.push(format!("{key}={value}"));
        }

        cmd_args.push(image_ref.to_string());
        cmd_args.push(entrypoint.to_string());
        cmd_args.extend(args.iter().cloned());
        cmd_args
    }

    /// Generate a deterministic container ID for an allocation (for simulation).
    fn generate_container_id(alloc_id: &AllocId) -> String {
        format!("sarus-{}", &alloc_id.to_string()[..12])
    }
}

#[async_trait]
impl Runtime for SarusRuntime {
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError> {
        let image_ref = config
            .image
            .as_deref()
            .ok_or_else(|| RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "OCI image reference required for SarusRuntime".to_string(),
            })?;

        let mut states = self.states.write().await;
        if states.contains_key(&config.alloc_id) {
            return Err(RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "allocation already prepared".to_string(),
            });
        }

        // In production: sarus pull <image>
        let state = SarusState {
            image_ref: image_ref.to_string(),
            container_id: None,
            pid: None,
            stopped: false,
        };
        states.insert(config.alloc_id, state);

        Ok(())
    }

    async fn spawn(
        &self,
        alloc_id: AllocId,
        _entrypoint: &str,
        _args: &[String],
    ) -> Result<ProcessHandle, RuntimeError> {
        let mut states = self.states.write().await;
        let state = states
            .get_mut(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;

        if state.pid.is_some() {
            return Err(RuntimeError::SpawnFailed {
                alloc_id,
                reason: "container already running".to_string(),
            });
        }

        // In production: sarus run <image> <entrypoint> [args]
        let container_id = Self::generate_container_id(&alloc_id);
        let pid = (alloc_id.as_u128() % 65535 + 1) as u32;

        state.container_id = Some(container_id.clone());
        state.pid = Some(pid);

        Ok(ProcessHandle {
            alloc_id,
            pid: Some(pid),
            container_id: Some(container_id),
        })
    }

    async fn signal(&self, handle: &ProcessHandle, signal: i32) -> Result<(), RuntimeError> {
        let states = self.states.read().await;
        let state = states.get(&handle.alloc_id).ok_or(RuntimeError::NotFound {
            alloc_id: handle.alloc_id,
        })?;

        if state.pid.is_none() {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "no container running".to_string(),
            });
        }

        if state.stopped {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "container already stopped".to_string(),
            });
        }

        tracing::debug!(
            alloc_id = %handle.alloc_id,
            container_id = ?state.container_id,
            signal,
            "sending signal to sarus container"
        );

        Ok(())
    }

    async fn stop(
        &self,
        handle: &ProcessHandle,
        _grace_secs: u32,
    ) -> Result<ExitStatus, RuntimeError> {
        let mut states = self.states.write().await;
        let state = states
            .get_mut(&handle.alloc_id)
            .ok_or(RuntimeError::NotFound {
                alloc_id: handle.alloc_id,
            })?;

        if state.stopped {
            return Ok(ExitStatus::Signal(15));
        }

        // In production: sarus kill <container-id> or kill -TERM <pid>
        state.stopped = true;
        Ok(ExitStatus::Signal(15))
    }

    async fn wait(&self, handle: &ProcessHandle) -> Result<ExitStatus, RuntimeError> {
        let states = self.states.read().await;
        let state = states.get(&handle.alloc_id).ok_or(RuntimeError::NotFound {
            alloc_id: handle.alloc_id,
        })?;

        if state.stopped {
            return Ok(ExitStatus::Signal(15));
        }

        Ok(ExitStatus::Code(0))
    }

    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError> {
        let mut states = self.states.write().await;
        let _state = states
            .remove(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;

        // In production: optionally `sarus rmi <image>` if not cached
        tracing::debug!(alloc_id = %alloc_id, "cleaned up sarus container");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SarusConfig {
        SarusConfig {
            sarus_bin: "/usr/bin/sarus".to_string(),
            extra_flags: vec!["--mpi".to_string()],
        }
    }

    fn prepare_config(alloc_id: AllocId) -> PrepareConfig {
        PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: Some("nvcr.io/nvidia/pytorch:24.01-py3".to_string()),
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        }
    }

    #[test]
    fn pull_args_format() {
        let rt = SarusRuntime::new(test_config());
        let args = rt.pull_args("nvcr.io/nvidia/pytorch:24.01-py3");
        assert_eq!(args, vec!["pull", "nvcr.io/nvidia/pytorch:24.01-py3"]);
    }

    #[test]
    fn run_args_with_env_and_flags() {
        let rt = SarusRuntime::new(test_config());
        let args = rt.run_args(
            "pytorch:latest",
            "python",
            &["train.py".to_string()],
            &[("CUDA_VISIBLE_DEVICES".to_string(), "0,1".to_string())],
        );

        assert_eq!(args[0], "run");
        assert_eq!(args[1], "--mpi"); // extra flag
        assert_eq!(args[2], "--env");
        assert_eq!(args[3], "CUDA_VISIBLE_DEVICES=0,1");
        assert_eq!(args[4], "pytorch:latest");
        assert_eq!(args[5], "python");
        assert_eq!(args[6], "train.py");
    }

    #[test]
    fn default_config() {
        let config = SarusConfig::default();
        assert!(config.sarus_bin.contains("sarus"));
        assert!(config.extra_flags.is_empty());
    }

    #[tokio::test]
    async fn prepare_creates_state() {
        let rt = SarusRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        let states = rt.states.read().await;
        let state = states.get(&alloc_id).unwrap();
        assert_eq!(state.image_ref, "nvcr.io/nvidia/pytorch:24.01-py3");
        assert!(state.container_id.is_none());
    }

    #[tokio::test]
    async fn prepare_requires_image() {
        let rt = SarusRuntime::new(test_config());
        let config = PrepareConfig {
            alloc_id: uuid::Uuid::new_v4(),
            uenv: Some("prgenv-gnu/24.11:v1".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        let result = rt.prepare(&config).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("OCI image reference required"));
    }

    #[tokio::test]
    async fn prepare_rejects_duplicate() {
        let rt = SarusRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        let config = prepare_config(alloc_id);

        rt.prepare(&config).await.unwrap();
        let result = rt.prepare(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn spawn_returns_handle_with_container_id() {
        let rt = SarusRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        let handle = rt
            .spawn(alloc_id, "python", &["train.py".to_string()])
            .await
            .unwrap();

        assert_eq!(handle.alloc_id, alloc_id);
        assert!(handle.pid.is_some());
        assert!(handle.container_id.is_some());
        assert!(handle.container_id.unwrap().starts_with("sarus-"));
    }

    #[tokio::test]
    async fn spawn_rejects_double_spawn() {
        let rt = SarusRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        rt.spawn(alloc_id, "python", &[]).await.unwrap();
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn full_lifecycle() {
        let rt = SarusRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        rt.signal(&handle, 10).await.unwrap();
        let status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));
        rt.cleanup(alloc_id).await.unwrap();

        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn signal_stopped_container_fails() {
        let rt = SarusRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        rt.stop(&handle, 5).await.unwrap();
        let result = rt.signal(&handle, 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cleanup_unknown_fails() {
        let rt = SarusRuntime::new(test_config());
        let result = rt.cleanup(uuid::Uuid::new_v4()).await;
        assert!(result.is_err());
    }
}

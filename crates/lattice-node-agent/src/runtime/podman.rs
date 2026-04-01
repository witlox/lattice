//! Podman runtime — OCI container execution via Podman + Parallax.
//!
//! Uses Podman for workloads that need OCI isolation with HPC module support.
//! Lifecycle (INV-SD10):
//! - Prepare: check Parallax shared store, `podman pull`, `parallax --migrate`,
//!   `podman run -d --module hpc` with paused init process.
//! - Spawn: `nsenter --target <pid> --mount --user -- <entrypoint> [args...]`
//!   (namespace joining delegated to nsenter to avoid corrupting tokio runtime)
//! - Stop: signal workload process (SIGTERM -> grace -> SIGKILL), `podman stop`
//! - Cleanup: `podman rm <container_id>`
//!
//! See specs/architecture/interfaces/software-delivery.md for full lifecycle spec.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{ExitStatus, PrepareConfig, ProcessHandle, Runtime, RuntimeError};
use lattice_common::types::AllocId;

/// State tracked per Podman container.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used when real OS commands are wired in
struct PodmanState {
    /// OCI image reference.
    image_ref: String,
    /// Container ID assigned by Podman.
    container_id: Option<String>,
    /// OS PID of the container's init process.
    container_pid: Option<u32>,
    /// OS PID of the spawned workload process (via nsenter).
    workload_pid: Option<u32>,
    /// Whether the container has been stopped.
    stopped: bool,
}

/// Configuration for the Podman runtime.
#[derive(Debug, Clone)]
pub struct PodmanConfig {
    /// Path to the `podman` binary.
    pub podman_bin: String,
    /// Podman module (e.g., "hpc").
    pub podman_module: String,
    /// Podman tmp path (e.g., "/dev/shm").
    pub podman_tmp_path: String,
    /// Parallax shared image store path.
    pub parallax_imagestore: Option<String>,
    /// Parallax binary path.
    pub parallax_bin: Option<String>,
    /// Parallax mount program path.
    pub parallax_mount_program: Option<String>,
    /// Path to the `nsenter` binary.
    pub nsenter_bin: String,
    /// EDF system search paths.
    pub edf_search_paths: Vec<String>,
}

impl Default for PodmanConfig {
    fn default() -> Self {
        Self {
            podman_bin: "podman".to_string(),
            podman_module: "hpc".to_string(),
            podman_tmp_path: "/dev/shm".to_string(),
            parallax_imagestore: None,
            parallax_bin: None,
            parallax_mount_program: None,
            nsenter_bin: "/usr/bin/nsenter".to_string(),
            edf_search_paths: Vec::new(),
        }
    }
}

/// Podman OCI container runtime implementation (INV-SD10).
///
/// Uses nsenter for namespace joining instead of setns() to avoid corrupting
/// the tokio async runtime's work-stealing threads. This matches the UenvRuntime
/// pattern where the agent never calls setns() in its own process.
pub struct PodmanRuntime {
    config: PodmanConfig,
    states: Arc<RwLock<HashMap<AllocId, PodmanState>>>,
}

impl PodmanRuntime {
    pub fn new(config: PodmanConfig) -> Self {
        Self {
            config,
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Build the `podman run` command args for creating a paused container.
    pub fn run_args(&self, alloc_id: &AllocId, image_ref: &str) -> Vec<String> {
        vec![
            "run".to_string(),
            "-d".to_string(),
            format!("--module={}", self.config.podman_module),
            format!("--tmpdir={}", self.config.podman_tmp_path),
            "--label".to_string(),
            "managed-by=lattice".to_string(),
            "--label".to_string(),
            format!("alloc-id={alloc_id}"),
            image_ref.to_string(),
            "sh".to_string(),
            "-c".to_string(),
            "kill -STOP $$ ; exit 0".to_string(),
        ]
    }

    /// Build the `nsenter` command args for spawning inside the container.
    pub fn nsenter_args(
        &self,
        container_pid: u32,
        entrypoint: &str,
        args: &[String],
    ) -> Vec<String> {
        let mut cmd_args = vec![
            format!("--target={container_pid}"),
            "--mount".to_string(),
            "--user".to_string(),
            "--".to_string(),
            entrypoint.to_string(),
        ];
        cmd_args.extend(args.iter().cloned());
        cmd_args
    }

    /// Generate a deterministic container ID for an allocation (for simulation).
    fn generate_container_id(alloc_id: &AllocId) -> String {
        format!("podman-{}", &alloc_id.to_string()[..12])
    }
}

#[async_trait]
impl Runtime for PodmanRuntime {
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError> {
        let image_ref = config
            .image
            .as_deref()
            .ok_or_else(|| RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "OCI image reference required for PodmanRuntime".to_string(),
            })?;

        let mut states = self.states.write().await;
        if states.contains_key(&config.alloc_id) {
            return Err(RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "allocation already prepared".to_string(),
            });
        }

        // In production:
        // 1. Check Parallax shared store for image
        // 2. If missing: podman pull -> parallax --migrate
        // 3. podman run -d --module hpc --label managed-by=lattice
        //    --label alloc-id=<id> [devices] [mounts] sh -c "kill -STOP $$ ; exit 0"
        // 4. Read container PID from podman inspect
        // 5. Persist container_id in agent state file
        let container_id = Self::generate_container_id(&config.alloc_id);
        let container_pid = (config.alloc_id.as_u128() % 65535 + 1000) as u32;

        let state = PodmanState {
            image_ref: image_ref.to_string(),
            container_id: Some(container_id),
            container_pid: Some(container_pid),
            workload_pid: None,
            stopped: false,
        };
        states.insert(config.alloc_id, state);

        let _ = &self.config; // suppress unused warning until real commands are wired in

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

        if state.workload_pid.is_some() {
            return Err(RuntimeError::SpawnFailed {
                alloc_id,
                reason: "workload already running".to_string(),
            });
        }

        // In production:
        // 1. Build environment from env_patches via apply_env_patches()
        // 2. Command::new(nsenter_bin) --target <container_pid>
        //    --mount --user -- <entrypoint> [args...]
        //    with env vars set on the Command
        // 3. Agent retains PID for signal delivery and exit status collection
        let workload_pid = (alloc_id.as_u128() % 65535 + 1) as u32;
        state.workload_pid = Some(workload_pid);

        Ok(ProcessHandle {
            alloc_id,
            pid: Some(workload_pid),
            container_id: state.container_id.clone(),
        })
    }

    async fn signal(&self, handle: &ProcessHandle, signal: i32) -> Result<(), RuntimeError> {
        let states = self.states.read().await;
        let state = states.get(&handle.alloc_id).ok_or(RuntimeError::NotFound {
            alloc_id: handle.alloc_id,
        })?;

        if state.workload_pid.is_none() {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "no workload process running".to_string(),
            });
        }

        if state.stopped {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "container already stopped".to_string(),
            });
        }

        // In production: kill -<signal> <workload_pid>
        tracing::debug!(
            alloc_id = %handle.alloc_id,
            container_id = ?state.container_id,
            workload_pid = ?state.workload_pid,
            signal,
            "sending signal to podman workload process"
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

        // In production:
        // 1. Signal spawned workload process (SIGTERM -> grace -> SIGKILL)
        // 2. podman stop <container_id>
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

        // In production: waitpid() on the workload child process
        Ok(ExitStatus::Code(0))
    }

    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError> {
        let mut states = self.states.write().await;
        let _state = states
            .remove(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;

        // In production:
        // 1. podman rm <container_id>
        // 2. Clean local artifacts
        tracing::debug!(alloc_id = %alloc_id, "cleaned up podman container");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PodmanConfig {
        PodmanConfig {
            podman_bin: "podman".to_string(),
            podman_module: "hpc".to_string(),
            nsenter_bin: "/usr/bin/nsenter".to_string(),
            ..Default::default()
        }
    }

    fn prepare_config(alloc_id: AllocId) -> PrepareConfig {
        PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: Some("nvcr.io/nvidia/pytorch:24.01-py3".to_string()),
            workdir: Some("/workspace".to_string()),
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
            resource_limits: None,
            images: vec![],
            env_patches: vec![],
        }
    }

    #[test]
    fn default_config() {
        let config = PodmanConfig::default();
        assert_eq!(config.podman_bin, "podman");
        assert_eq!(config.podman_module, "hpc");
        assert_eq!(config.podman_tmp_path, "/dev/shm");
        assert_eq!(config.nsenter_bin, "/usr/bin/nsenter");
        assert!(config.parallax_imagestore.is_none());
        assert!(config.edf_search_paths.is_empty());
    }

    #[test]
    fn run_args_format() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        let args = rt.run_args(&alloc_id, "pytorch:latest");

        assert_eq!(args[0], "run");
        assert_eq!(args[1], "-d");
        assert!(args[2].starts_with("--module="));
        assert!(args.iter().any(|a| a == "managed-by=lattice"));
        assert!(args.iter().any(|a| a.contains(&alloc_id.to_string())));
    }

    #[test]
    fn nsenter_args_format() {
        let rt = PodmanRuntime::new(test_config());
        let args = rt.nsenter_args(
            12345,
            "python",
            &["train.py".to_string(), "--epochs=10".to_string()],
        );
        assert_eq!(args[0], "--target=12345");
        assert_eq!(args[1], "--mount");
        assert_eq!(args[2], "--user");
        assert_eq!(args[3], "--");
        assert_eq!(args[4], "python");
        assert_eq!(args[5], "train.py");
        assert_eq!(args[6], "--epochs=10");
    }

    #[tokio::test]
    async fn podman_prepare_and_spawn() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        let handle = rt
            .spawn(alloc_id, "python", &["train.py".to_string()])
            .await
            .unwrap();

        assert_eq!(handle.alloc_id, alloc_id);
        assert!(handle.pid.is_some());
        assert!(handle.container_id.is_some());
        assert!(handle.container_id.unwrap().starts_with("podman-"));
    }

    #[tokio::test]
    async fn podman_prepare_requires_image() {
        let rt = PodmanRuntime::new(test_config());
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
            resource_limits: None,
            images: vec![],
            env_patches: vec![],
        };

        let result = rt.prepare(&config).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("OCI image reference required"));
    }

    #[tokio::test]
    async fn podman_prepare_rejects_duplicate() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        let config = prepare_config(alloc_id);

        rt.prepare(&config).await.unwrap();
        let result = rt.prepare(&config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already prepared"));
    }

    #[tokio::test]
    async fn podman_spawn_rejects_double_spawn() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        rt.spawn(alloc_id, "python", &[]).await.unwrap();
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));
    }

    #[tokio::test]
    async fn podman_stop_returns_signal() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        let status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));
    }

    #[tokio::test]
    async fn podman_cleanup_removes_state() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        rt.stop(&handle, 5).await.unwrap();

        rt.cleanup(alloc_id).await.unwrap();

        // After cleanup, operations should fail with NotFound
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(matches!(result, Err(RuntimeError::NotFound { .. })));
    }

    #[tokio::test]
    async fn podman_not_found() {
        let rt = PodmanRuntime::new(test_config());
        let unknown = uuid::Uuid::new_v4();

        let result = rt.spawn(unknown, "/bin/bash", &[]).await;
        assert!(matches!(result, Err(RuntimeError::NotFound { .. })));

        let handle = ProcessHandle {
            alloc_id: unknown,
            pid: Some(1234),
            container_id: None,
        };
        let result = rt.signal(&handle, 15).await;
        assert!(matches!(result, Err(RuntimeError::NotFound { .. })));

        let result = rt.stop(&handle, 5).await;
        assert!(matches!(result, Err(RuntimeError::NotFound { .. })));

        let result = rt.cleanup(unknown).await;
        assert!(matches!(result, Err(RuntimeError::NotFound { .. })));
    }

    #[tokio::test]
    async fn podman_signal_stopped_container_fails() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        rt.stop(&handle, 5).await.unwrap();
        let result = rt.signal(&handle, 10).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already stopped"));
    }

    #[tokio::test]
    async fn podman_full_lifecycle() {
        let rt = PodmanRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        // Prepare
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        // Spawn
        let handle = rt
            .spawn(alloc_id, "python", &["train.py".to_string()])
            .await
            .unwrap();
        assert!(handle.pid.is_some());
        assert!(handle.container_id.is_some());

        // Signal
        rt.signal(&handle, 10).await.unwrap();

        // Wait (running)
        let status = rt.wait(&handle).await.unwrap();
        assert_eq!(status, ExitStatus::Code(0));

        // Stop
        let status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));

        // Wait (stopped)
        let status = rt.wait(&handle).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));

        // Cleanup
        rt.cleanup(alloc_id).await.unwrap();

        // After cleanup, spawn should fail
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
    }
}

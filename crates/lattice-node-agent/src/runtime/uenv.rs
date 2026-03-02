//! uenv runtime — SquashFS mount namespace execution.
//!
//! Uses `squashfs-mount` to mount uenv images and `nsenter`/`unshare` to
//! execute the entrypoint in an isolated mount namespace. This is the default
//! and preferred runtime for Lattice (near-zero overhead, no container daemon).
//!
//! Commands generated:
//! - Prepare: `squashfs-mount <image-path> <mount-point>`
//! - Spawn:   `unshare --mount nsenter --mount=<ns-path> -- <entrypoint> [args...]`
//! - Stop:    `kill -TERM <pid>` → grace period → `kill -KILL <pid>`
//! - Cleanup: `umount <mount-point> && rm -rf <workdir>`

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{ExitStatus, PrepareConfig, ProcessHandle, Runtime, RuntimeError};
use lattice_common::types::AllocId;

/// State tracked per prepared allocation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used when real OS commands are wired in
struct UenvState {
    /// Path where the SquashFS image is mounted.
    mount_point: String,
    /// The uenv image reference.
    image_ref: String,
    /// Working directory created for the allocation.
    workdir: String,
    /// PID of the spawned process (set after spawn).
    pid: Option<u32>,
    /// Whether the process has been stopped.
    stopped: bool,
}

/// Configuration for the uenv runtime.
#[derive(Debug, Clone)]
pub struct UenvConfig {
    /// Base directory for SquashFS mounts (e.g., "/var/lib/lattice/mounts").
    pub mount_base: String,
    /// Base directory for allocation working directories.
    pub workdir_base: String,
    /// Path to the `squashfs-mount` binary.
    pub squashfs_mount_bin: String,
    /// Path to the `nsenter` binary.
    pub nsenter_bin: String,
    /// Path to the `unshare` binary.
    pub unshare_bin: String,
}

impl Default for UenvConfig {
    fn default() -> Self {
        Self {
            mount_base: "/var/lib/lattice/mounts".to_string(),
            workdir_base: "/var/lib/lattice/workdirs".to_string(),
            squashfs_mount_bin: "/usr/bin/squashfs-mount".to_string(),
            nsenter_bin: "/usr/bin/nsenter".to_string(),
            unshare_bin: "/usr/bin/unshare".to_string(),
        }
    }
}

/// uenv runtime implementation.
///
/// Uses SquashFS mount namespaces for near-zero-overhead execution.
/// This is the recommended runtime for HPC workloads on Lattice.
pub struct UenvRuntime {
    config: UenvConfig,
    states: Arc<RwLock<HashMap<AllocId, UenvState>>>,
}

impl UenvRuntime {
    pub fn new(config: UenvConfig) -> Self {
        Self {
            config,
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Build the squashfs-mount command args for preparing an image.
    pub fn mount_args(&self, image_ref: &str, mount_point: &str) -> Vec<String> {
        vec![image_ref.to_string(), mount_point.to_string()]
    }

    /// Build the spawn command args for running inside the mount namespace.
    pub fn spawn_args(&self, mount_point: &str, entrypoint: &str, args: &[String]) -> Vec<String> {
        let mut cmd_args = vec![
            "--mount".to_string(),
            format!("--mount={mount_point}/ns/mnt"),
            "--".to_string(),
            entrypoint.to_string(),
        ];
        cmd_args.extend(args.iter().cloned());
        cmd_args
    }

    /// Derive the mount point path for an allocation.
    fn mount_point(&self, alloc_id: &AllocId) -> String {
        format!("{}/{}", self.config.mount_base, alloc_id)
    }

    /// Derive the working directory for an allocation.
    fn workdir(&self, alloc_id: &AllocId) -> String {
        format!("{}/{}", self.config.workdir_base, alloc_id)
    }
}

#[async_trait]
impl Runtime for UenvRuntime {
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError> {
        let image_ref = config
            .uenv
            .as_deref()
            .ok_or_else(|| RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "uenv image reference required for UenvRuntime".to_string(),
            })?;

        let mount_point = self.mount_point(&config.alloc_id);
        let workdir = config
            .workdir
            .clone()
            .unwrap_or_else(|| self.workdir(&config.alloc_id));

        let state = UenvState {
            mount_point,
            image_ref: image_ref.to_string(),
            workdir,
            pid: None,
            stopped: false,
        };

        let mut states = self.states.write().await;
        if states.contains_key(&config.alloc_id) {
            return Err(RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "allocation already prepared".to_string(),
            });
        }
        states.insert(config.alloc_id, state);

        // In production, this would run:
        //   squashfs-mount <image-ref> <mount-point>
        //   mkdir -p <workdir>
        // For now, we record state. Actual execution is in the OS-level integration.

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
                reason: "process already spawned".to_string(),
            });
        }

        // In production, this would run:
        //   unshare --mount nsenter --mount=<mount>/ns/mnt -- <entrypoint> [args]
        // and capture the child PID.
        // For now, we simulate a PID assignment.
        let pid = (alloc_id.as_u128() % 65535 + 1) as u32;
        state.pid = Some(pid);

        Ok(ProcessHandle {
            alloc_id,
            pid: Some(pid),
            container_id: None,
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
                reason: "no process running".to_string(),
            });
        }

        if state.stopped {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "process already stopped".to_string(),
            });
        }

        // In production: kill -<signal> <pid>
        tracing::debug!(
            alloc_id = %handle.alloc_id,
            pid = ?state.pid,
            signal,
            "sending signal to uenv process"
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
            return Ok(ExitStatus::Signal(15)); // already stopped
        }

        // In production: kill -TERM <pid>, sleep grace_secs, kill -KILL <pid>
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

        // In production: waitpid() on the child process
        Ok(ExitStatus::Code(0))
    }

    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError> {
        let mut states = self.states.write().await;
        let _state = states
            .remove(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;

        // In production:
        //   umount <mount-point>
        //   rm -rf <workdir>
        tracing::debug!(alloc_id = %alloc_id, "cleaned up uenv environment");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> UenvConfig {
        UenvConfig {
            mount_base: "/tmp/lattice-test/mounts".to_string(),
            workdir_base: "/tmp/lattice-test/workdirs".to_string(),
            ..Default::default()
        }
    }

    fn prepare_config(alloc_id: AllocId) -> PrepareConfig {
        PrepareConfig {
            alloc_id,
            uenv: Some("prgenv-gnu/24.11:v1".to_string()),
            view: Some("default".to_string()),
            image: None,
            workdir: None,
            env_vars: vec![],
        }
    }

    #[test]
    fn mount_args_format() {
        let rt = UenvRuntime::new(test_config());
        let args = rt.mount_args("prgenv-gnu/24.11:v1", "/mnt/alloc1");
        assert_eq!(args, vec!["prgenv-gnu/24.11:v1", "/mnt/alloc1"]);
    }

    #[test]
    fn spawn_args_format() {
        let rt = UenvRuntime::new(test_config());
        let args = rt.spawn_args(
            "/mnt/alloc1",
            "python",
            &["train.py".to_string(), "--epochs=10".to_string()],
        );
        assert_eq!(args[0], "--mount");
        assert!(args[1].contains("/mnt/alloc1"));
        assert_eq!(args[2], "--");
        assert_eq!(args[3], "python");
        assert_eq!(args[4], "train.py");
        assert_eq!(args[5], "--epochs=10");
    }

    #[test]
    fn default_config_paths() {
        let config = UenvConfig::default();
        assert_eq!(config.mount_base, "/var/lib/lattice/mounts");
        assert_eq!(config.workdir_base, "/var/lib/lattice/workdirs");
        assert!(config.squashfs_mount_bin.contains("squashfs-mount"));
    }

    #[tokio::test]
    async fn prepare_creates_state() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        let config = prepare_config(alloc_id);

        rt.prepare(&config).await.unwrap();

        let states = rt.states.read().await;
        let state = states.get(&alloc_id).unwrap();
        assert_eq!(state.image_ref, "prgenv-gnu/24.11:v1");
        assert!(state.pid.is_none());
    }

    #[tokio::test]
    async fn prepare_requires_uenv_image() {
        let rt = UenvRuntime::new(test_config());
        let config = PrepareConfig {
            alloc_id: uuid::Uuid::new_v4(),
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        let result = rt.prepare(&config).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("uenv image reference required"));
    }

    #[tokio::test]
    async fn prepare_rejects_duplicate() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        let config = prepare_config(alloc_id);

        rt.prepare(&config).await.unwrap();
        let result = rt.prepare(&config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already prepared"));
    }

    #[tokio::test]
    async fn spawn_returns_handle_with_pid() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        let handle = rt
            .spawn(alloc_id, "python", &["train.py".to_string()])
            .await
            .unwrap();

        assert_eq!(handle.alloc_id, alloc_id);
        assert!(handle.pid.is_some());
        assert!(handle.container_id.is_none());
    }

    #[tokio::test]
    async fn spawn_rejects_double_spawn() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();

        rt.spawn(alloc_id, "python", &[]).await.unwrap();
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already spawned"));
    }

    #[tokio::test]
    async fn spawn_unknown_alloc_fails() {
        let rt = UenvRuntime::new(test_config());
        let result = rt.spawn(uuid::Uuid::new_v4(), "python", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn signal_running_process() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        rt.signal(&handle, 10).await.unwrap(); // SIGUSR1
    }

    #[tokio::test]
    async fn signal_stopped_process_fails() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        rt.stop(&handle, 5).await.unwrap();
        let result = rt.signal(&handle, 10).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already stopped"));
    }

    #[tokio::test]
    async fn stop_returns_signal_exit() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        let status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));
    }

    #[tokio::test]
    async fn full_lifecycle_prepare_spawn_stop_cleanup() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt
            .spawn(alloc_id, "python", &["train.py".to_string()])
            .await
            .unwrap();
        rt.signal(&handle, 10).await.unwrap();
        let status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));
        rt.cleanup(alloc_id).await.unwrap();

        // After cleanup, spawn should fail (not found)
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cleanup_unknown_alloc_fails() {
        let rt = UenvRuntime::new(test_config());
        let result = rt.cleanup(uuid::Uuid::new_v4()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn wait_returns_success_for_running() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        let status = rt.wait(&handle).await.unwrap();
        assert_eq!(status, ExitStatus::Code(0));
    }

    #[tokio::test]
    async fn wait_returns_signal_for_stopped() {
        let rt = UenvRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();
        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();

        rt.stop(&handle, 5).await.unwrap();
        let status = rt.wait(&handle).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));
    }
}

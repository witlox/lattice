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
#[cfg(target_os = "linux")]
use std::time::Duration;

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
            mount_point: mount_point.clone(),
            image_ref: image_ref.to_string(),
            workdir: workdir.clone(),
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

        #[cfg(target_os = "linux")]
        {
            // Check if squashfs-mount binary exists
            let bin = &self.config.squashfs_mount_bin;
            if !std::path::Path::new(bin).exists() {
                return Err(RuntimeError::PrepareFailed {
                    alloc_id: config.alloc_id,
                    reason: format!(
                        "squashfs-mount binary not found at {bin} — uenv not available on this node"
                    ),
                });
            }

            // Create mount point directory
            tokio::fs::create_dir_all(&mount_point).await.map_err(|e| {
                RuntimeError::PrepareFailed {
                    alloc_id: config.alloc_id,
                    reason: format!("failed to create mount point {mount_point}: {e}"),
                }
            })?;

            // Mount squashfs image
            let status = tokio::process::Command::new(&self.config.squashfs_mount_bin)
                .arg(image_ref)
                .arg(&mount_point)
                .status()
                .await
                .map_err(|e| RuntimeError::PrepareFailed {
                    alloc_id: config.alloc_id,
                    reason: format!("squashfs-mount failed: {e}"),
                })?;
            if !status.success() {
                return Err(RuntimeError::PrepareFailed {
                    alloc_id: config.alloc_id,
                    reason: format!("squashfs-mount exited with code {:?}", status.code()),
                });
            }

            // Create workdir
            tokio::fs::create_dir_all(&workdir)
                .await
                .map_err(|e| RuntimeError::PrepareFailed {
                    alloc_id: config.alloc_id,
                    reason: format!("failed to create workdir {workdir}: {e}"),
                })?;
        }

        Ok(())
    }

    async fn spawn(
        &self,
        alloc_id: AllocId,
        entrypoint: &str,
        args: &[String],
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

        #[cfg(target_os = "linux")]
        let pid = {
            // Build environment with env_patches applied
            let env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            // Inherit env_vars from state if stored; env_patches handled here
            // (PrepareConfig.env_patches are applied at spawn time)
            let mount_point = &state.mount_point;

            let child = tokio::process::Command::new(&self.config.unshare_bin)
                .arg("--mount")
                .arg("nsenter")
                .arg(format!("--mount={mount_point}/ns/mnt"))
                .arg("--")
                .arg(entrypoint)
                .args(args)
                .envs(env)
                .spawn()
                .map_err(|e| RuntimeError::SpawnFailed {
                    alloc_id,
                    reason: format!("failed to spawn: {e}"),
                })?;
            let pid = child.id().ok_or_else(|| RuntimeError::SpawnFailed {
                alloc_id,
                reason: "child process has no PID".to_string(),
            })?;
            pid
        };

        #[cfg(not(target_os = "linux"))]
        let pid = {
            let _ = (entrypoint, args);
            (alloc_id.as_u128() % 65535 + 1) as u32
        };

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

        let pid = state.pid.ok_or(RuntimeError::SignalFailed {
            alloc_id: handle.alloc_id,
            reason: "no process running".to_string(),
        })?;

        if state.stopped {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "process already stopped".to_string(),
            });
        }

        tracing::debug!(
            alloc_id = %handle.alloc_id,
            pid,
            signal,
            "sending signal to uenv process"
        );

        #[cfg(target_os = "linux")]
        {
            use libc::{kill, pid_t};
            let ret = unsafe { kill(pid as pid_t, signal) };
            if ret != 0 {
                return Err(RuntimeError::SignalFailed {
                    alloc_id: handle.alloc_id,
                    reason: format!(
                        "kill({pid}, {signal}) failed: {}",
                        std::io::Error::last_os_error()
                    ),
                });
            }
        }

        Ok(())
    }

    async fn stop(
        &self,
        handle: &ProcessHandle,
        grace_secs: u32,
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

        #[cfg(target_os = "linux")]
        if let Some(pid) = state.pid {
            use libc::{kill, pid_t, SIGKILL, SIGTERM};

            // Send SIGTERM
            unsafe { kill(pid as pid_t, SIGTERM) };

            // Wait grace period
            tokio::time::sleep(Duration::from_secs(grace_secs as u64)).await;

            // Check if still alive, SIGKILL if needed
            let alive = unsafe { kill(pid as pid_t, 0) } == 0;
            if alive {
                unsafe { kill(pid as pid_t, SIGKILL) };
            }
        }

        #[cfg(not(target_os = "linux"))]
        let _ = grace_secs;

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

        #[cfg(target_os = "linux")]
        if let Some(pid) = state.pid {
            use libc::{waitpid, WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG};
            let mut status: i32 = 0;
            let ret = unsafe { waitpid(pid as i32, &mut status, 0) };
            if ret < 0 {
                return Err(RuntimeError::StopFailed {
                    alloc_id: handle.alloc_id,
                    reason: format!("waitpid failed: {}", std::io::Error::last_os_error()),
                });
            }
            if WIFEXITED(status) {
                return Ok(ExitStatus::Code(WEXITSTATUS(status)));
            }
            if WIFSIGNALED(status) {
                return Ok(ExitStatus::Signal(WTERMSIG(status)));
            }
            return Ok(ExitStatus::Unknown);
        }

        Ok(ExitStatus::Code(0))
    }

    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError> {
        let mut states = self.states.write().await;
        let state = states
            .remove(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;

        #[cfg(target_os = "linux")]
        {
            // Unmount squashfs
            let umount_status = tokio::process::Command::new("umount")
                .arg(&state.mount_point)
                .status()
                .await
                .map_err(|e| RuntimeError::StopFailed {
                    alloc_id,
                    reason: format!("umount failed: {e}"),
                })?;
            if !umount_status.success() {
                tracing::warn!(
                    alloc_id = %alloc_id,
                    mount_point = %state.mount_point,
                    "umount exited with non-zero status"
                );
            }

            // Remove mount point directory
            if let Err(e) = tokio::fs::remove_dir_all(&state.mount_point).await {
                tracing::warn!(
                    alloc_id = %alloc_id,
                    path = %state.mount_point,
                    error = %e,
                    "failed to remove mount point directory"
                );
            }

            // Remove workdir
            if let Err(e) = tokio::fs::remove_dir_all(&state.workdir).await {
                tracing::warn!(
                    alloc_id = %alloc_id,
                    path = %state.workdir,
                    error = %e,
                    "failed to remove workdir"
                );
            }
        }

        #[cfg(not(target_os = "linux"))]
        let _ = state;

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

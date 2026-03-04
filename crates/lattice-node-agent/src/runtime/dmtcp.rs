//! DMTCP runtime — transparent checkpointing via DMTCP.
//!
//! DMTCP (Distributed MultiThreaded CheckPointing) provides transparent
//! checkpointing for applications that don't implement their own checkpoint
//! protocol. This runtime wraps the standard execution with DMTCP's
//! `dmtcp_launch` and `dmtcp_restart` commands.
//!
//! Feature-gated behind `dmtcp`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{ExitStatus, PrepareConfig, ProcessHandle, Runtime, RuntimeError};
use lattice_common::types::AllocId;

/// DMTCP state per allocation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used when real DMTCP commands are wired in
struct DmtcpState {
    /// DMTCP coordinator port (unique per allocation).
    coordinator_port: u16,
    /// Checkpoint directory.
    checkpoint_dir: String,
    /// Whether this is a restart from checkpoint.
    is_restart: bool,
    /// PID of the DMTCP-wrapped process.
    pid: Option<u32>,
    /// Whether stopped.
    stopped: bool,
}

/// Configuration for the DMTCP runtime.
#[derive(Debug, Clone)]
pub struct DmtcpConfig {
    /// Path to `dmtcp_launch` binary.
    pub launch_bin: String,
    /// Path to `dmtcp_restart` binary.
    pub restart_bin: String,
    /// Path to `dmtcp_command` binary.
    pub command_bin: String,
    /// Base directory for checkpoint images.
    pub checkpoint_base: String,
    /// Base port for DMTCP coordinators (each allocation gets port + offset).
    pub base_port: u16,
}

impl Default for DmtcpConfig {
    fn default() -> Self {
        Self {
            launch_bin: "/usr/bin/dmtcp_launch".to_string(),
            restart_bin: "/usr/bin/dmtcp_restart".to_string(),
            command_bin: "/usr/bin/dmtcp_command".to_string(),
            checkpoint_base: "/var/lib/lattice/checkpoints".to_string(),
            base_port: 7779,
        }
    }
}

/// DMTCP transparent checkpointing runtime.
pub struct DmtcpRuntime {
    config: DmtcpConfig,
    states: Arc<RwLock<HashMap<AllocId, DmtcpState>>>,
    port_counter: Arc<RwLock<u16>>,
}

impl DmtcpRuntime {
    pub fn new(config: DmtcpConfig) -> Self {
        let base = config.base_port;
        Self {
            config,
            states: Arc::new(RwLock::new(HashMap::new())),
            port_counter: Arc::new(RwLock::new(base)),
        }
    }

    /// Build the `dmtcp_launch` command args.
    pub fn launch_args(
        &self,
        coordinator_port: u16,
        entrypoint: &str,
        args: &[String],
    ) -> Vec<String> {
        let mut cmd_args = vec![
            "--port".to_string(),
            coordinator_port.to_string(),
            "--".to_string(),
            entrypoint.to_string(),
        ];
        cmd_args.extend(args.iter().cloned());
        cmd_args
    }

    /// Build the `dmtcp_restart` command args for resuming from checkpoint.
    pub fn restart_args(&self, coordinator_port: u16, checkpoint_dir: &str) -> Vec<String> {
        vec![
            "--port".to_string(),
            coordinator_port.to_string(),
            "--ckptdir".to_string(),
            checkpoint_dir.to_string(),
        ]
    }

    /// Build the `dmtcp_command` args for triggering a checkpoint.
    pub fn checkpoint_args(&self, coordinator_port: u16) -> Vec<String> {
        vec![
            "--port".to_string(),
            coordinator_port.to_string(),
            "--checkpoint".to_string(),
        ]
    }

    async fn next_port(&self) -> u16 {
        let mut port = self.port_counter.write().await;
        let current = *port;
        *port += 1;
        current
    }
}

#[async_trait]
impl Runtime for DmtcpRuntime {
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError> {
        let port = self.next_port().await;
        let checkpoint_dir = format!("{}/{}", self.config.checkpoint_base, config.alloc_id);

        let mut states = self.states.write().await;
        if states.contains_key(&config.alloc_id) {
            return Err(RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: "allocation already prepared".to_string(),
            });
        }

        states.insert(
            config.alloc_id,
            DmtcpState {
                coordinator_port: port,
                checkpoint_dir,
                is_restart: false,
                pid: None,
                stopped: false,
            },
        );

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

        // In production: dmtcp_launch or dmtcp_restart depending on is_restart
        let pid = (alloc_id.as_u128() % 65535 + 1) as u32;
        state.pid = Some(pid);

        Ok(ProcessHandle {
            alloc_id,
            pid: Some(pid),
            container_id: Some(format!("dmtcp-{}", state.coordinator_port)),
        })
    }

    async fn signal(&self, handle: &ProcessHandle, signal: i32) -> Result<(), RuntimeError> {
        let states = self.states.read().await;
        let state = states.get(&handle.alloc_id).ok_or(RuntimeError::NotFound {
            alloc_id: handle.alloc_id,
        })?;

        if state.stopped {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "process already stopped".to_string(),
            });
        }

        // In production: kill -<signal> <pid> or dmtcp_command --checkpoint
        tracing::debug!(
            alloc_id = %handle.alloc_id,
            signal,
            port = state.coordinator_port,
            "sending signal to DMTCP-wrapped process"
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
        states
            .remove(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DmtcpConfig {
        DmtcpConfig {
            checkpoint_base: "/tmp/lattice-test/checkpoints".to_string(),
            base_port: 10000,
            ..Default::default()
        }
    }

    fn prepare_config(alloc_id: AllocId) -> PrepareConfig {
        PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
        }
    }

    #[test]
    fn launch_args_format() {
        let rt = DmtcpRuntime::new(test_config());
        let args = rt.launch_args(7779, "python", &["train.py".to_string()]);
        assert_eq!(args[0], "--port");
        assert_eq!(args[1], "7779");
        assert_eq!(args[2], "--");
        assert_eq!(args[3], "python");
        assert_eq!(args[4], "train.py");
    }

    #[test]
    fn restart_args_format() {
        let rt = DmtcpRuntime::new(test_config());
        let args = rt.restart_args(7780, "/ckpt/alloc-1");
        assert_eq!(args[0], "--port");
        assert_eq!(args[1], "7780");
        assert_eq!(args[2], "--ckptdir");
        assert_eq!(args[3], "/ckpt/alloc-1");
    }

    #[test]
    fn checkpoint_args_format() {
        let rt = DmtcpRuntime::new(test_config());
        let args = rt.checkpoint_args(7781);
        assert_eq!(args[0], "--port");
        assert_eq!(args[1], "7781");
        assert_eq!(args[2], "--checkpoint");
    }

    #[test]
    fn default_config_paths() {
        let config = DmtcpConfig::default();
        assert!(config.launch_bin.contains("dmtcp_launch"));
        assert!(config.restart_bin.contains("dmtcp_restart"));
        assert_eq!(config.base_port, 7779);
    }

    #[tokio::test]
    async fn full_lifecycle() {
        let rt = DmtcpRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        assert!(handle.pid.is_some());
        assert!(handle.container_id.as_ref().unwrap().starts_with("dmtcp-"));

        rt.signal(&handle, 10).await.unwrap();
        let status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(status, ExitStatus::Signal(15));
        rt.cleanup(alloc_id).await.unwrap();
    }

    #[tokio::test]
    async fn prepare_rejects_duplicate() {
        let rt = DmtcpRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let result = rt.prepare(&prepare_config(alloc_id)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ports_increment() {
        let rt = DmtcpRuntime::new(test_config());
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(id1)).await.unwrap();
        rt.prepare(&prepare_config(id2)).await.unwrap();

        let states = rt.states.read().await;
        let port1 = states.get(&id1).unwrap().coordinator_port;
        let port2 = states.get(&id2).unwrap().coordinator_port;
        assert_eq!(port2, port1 + 1);
    }

    #[tokio::test]
    async fn signal_stopped_fails() {
        let rt = DmtcpRuntime::new(test_config());
        let alloc_id = uuid::Uuid::new_v4();

        rt.prepare(&prepare_config(alloc_id)).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        rt.stop(&handle, 5).await.unwrap();

        let result = rt.signal(&handle, 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cleanup_unknown_fails() {
        let rt = DmtcpRuntime::new(test_config());
        let result = rt.cleanup(uuid::Uuid::new_v4()).await;
        assert!(result.is_err());
    }
}

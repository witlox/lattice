//! Mock runtime for testing.
//!
//! Records all calls and allows pre-configuring results for prepare, spawn,
//! signal, stop, wait, and cleanup operations. Used in unit and integration
//! tests to verify allocation lifecycle without real process execution.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{ExitStatus, PrepareConfig, ProcessHandle, Runtime, RuntimeError};
use lattice_common::types::AllocId;

/// Records of calls made to the mock runtime.
#[derive(Debug, Clone)]
pub enum MockCall {
    Prepare {
        alloc_id: AllocId,
    },
    Spawn {
        alloc_id: AllocId,
        entrypoint: String,
        args: Vec<String>,
    },
    Signal {
        alloc_id: AllocId,
        signal: i32,
    },
    Stop {
        alloc_id: AllocId,
        grace_secs: u32,
    },
    Wait {
        alloc_id: AllocId,
    },
    Cleanup {
        alloc_id: AllocId,
    },
}

/// Per-allocation mock state.
#[derive(Debug, Clone)]
struct MockState {
    prepared: bool,
    spawned: bool,
    stopped: bool,
    exit_status: ExitStatus,
    pid: u32,
}

/// Configuration for pre-setting mock behavior.
#[derive(Debug, Clone, Default)]
pub struct MockConfig {
    /// If set, `prepare()` will fail with this error message.
    pub prepare_error: Option<String>,
    /// If set, `spawn()` will fail with this error message.
    pub spawn_error: Option<String>,
    /// If set, `signal()` will fail with this error message.
    pub signal_error: Option<String>,
    /// If set, `stop()` will fail with this error message.
    pub stop_error: Option<String>,
    /// Exit status to return from `wait()` and `stop()`.
    pub exit_status: Option<ExitStatus>,
}

/// Mock runtime that records calls and returns configurable results.
pub struct MockRuntime {
    config: MockConfig,
    calls: Arc<RwLock<Vec<MockCall>>>,
    states: Arc<RwLock<HashMap<AllocId, MockState>>>,
    next_pid: Arc<RwLock<u32>>,
}

impl MockRuntime {
    /// Create a mock runtime with default (success) behavior.
    pub fn new() -> Self {
        Self {
            config: MockConfig::default(),
            calls: Arc::new(RwLock::new(Vec::new())),
            states: Arc::new(RwLock::new(HashMap::new())),
            next_pid: Arc::new(RwLock::new(1000)),
        }
    }

    /// Create a mock runtime with custom configuration.
    pub fn with_config(config: MockConfig) -> Self {
        Self {
            config,
            calls: Arc::new(RwLock::new(Vec::new())),
            states: Arc::new(RwLock::new(HashMap::new())),
            next_pid: Arc::new(RwLock::new(1000)),
        }
    }

    /// Get all calls that have been made to this runtime.
    pub async fn calls(&self) -> Vec<MockCall> {
        self.calls.read().await.clone()
    }

    /// Get the number of calls that have been made.
    pub async fn call_count(&self) -> usize {
        self.calls.read().await.len()
    }

    /// Get only calls of a specific type for an allocation.
    pub async fn calls_for(&self, alloc_id: AllocId) -> Vec<MockCall> {
        self.calls
            .read()
            .await
            .iter()
            .filter(|c| match c {
                MockCall::Prepare { alloc_id: id } => *id == alloc_id,
                MockCall::Spawn { alloc_id: id, .. } => *id == alloc_id,
                MockCall::Signal { alloc_id: id, .. } => *id == alloc_id,
                MockCall::Stop { alloc_id: id, .. } => *id == alloc_id,
                MockCall::Wait { alloc_id: id } => *id == alloc_id,
                MockCall::Cleanup { alloc_id: id } => *id == alloc_id,
            })
            .cloned()
            .collect()
    }

    async fn record(&self, call: MockCall) {
        self.calls.write().await.push(call);
    }

    async fn alloc_pid(&self) -> u32 {
        let mut pid = self.next_pid.write().await;
        let current = *pid;
        *pid += 1;
        current
    }
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Runtime for MockRuntime {
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError> {
        self.record(MockCall::Prepare {
            alloc_id: config.alloc_id,
        })
        .await;

        if let Some(ref err) = self.config.prepare_error {
            return Err(RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason: err.clone(),
            });
        }

        let pid = self.alloc_pid().await;
        let exit_status = self
            .config
            .exit_status
            .clone()
            .unwrap_or(ExitStatus::Code(0));

        let mut states = self.states.write().await;
        states.insert(
            config.alloc_id,
            MockState {
                prepared: true,
                spawned: false,
                stopped: false,
                exit_status,
                pid,
            },
        );

        Ok(())
    }

    async fn spawn(
        &self,
        alloc_id: AllocId,
        entrypoint: &str,
        args: &[String],
    ) -> Result<ProcessHandle, RuntimeError> {
        self.record(MockCall::Spawn {
            alloc_id,
            entrypoint: entrypoint.to_string(),
            args: args.to_vec(),
        })
        .await;

        if let Some(ref err) = self.config.spawn_error {
            return Err(RuntimeError::SpawnFailed {
                alloc_id,
                reason: err.clone(),
            });
        }

        let mut states = self.states.write().await;
        let state = states
            .get_mut(&alloc_id)
            .ok_or(RuntimeError::NotFound { alloc_id })?;

        if !state.prepared {
            return Err(RuntimeError::SpawnFailed {
                alloc_id,
                reason: "not prepared".to_string(),
            });
        }

        state.spawned = true;

        Ok(ProcessHandle {
            alloc_id,
            pid: Some(state.pid),
            container_id: Some(format!("mock-{alloc_id}")),
        })
    }

    async fn signal(&self, handle: &ProcessHandle, signal: i32) -> Result<(), RuntimeError> {
        self.record(MockCall::Signal {
            alloc_id: handle.alloc_id,
            signal,
        })
        .await;

        if let Some(ref err) = self.config.signal_error {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: err.clone(),
            });
        }

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

        Ok(())
    }

    async fn stop(
        &self,
        handle: &ProcessHandle,
        grace_secs: u32,
    ) -> Result<ExitStatus, RuntimeError> {
        self.record(MockCall::Stop {
            alloc_id: handle.alloc_id,
            grace_secs,
        })
        .await;

        if let Some(ref err) = self.config.stop_error {
            return Err(RuntimeError::StopFailed {
                alloc_id: handle.alloc_id,
                reason: err.clone(),
            });
        }

        let mut states = self.states.write().await;
        let state = states
            .get_mut(&handle.alloc_id)
            .ok_or(RuntimeError::NotFound {
                alloc_id: handle.alloc_id,
            })?;

        state.stopped = true;
        Ok(ExitStatus::Signal(15))
    }

    async fn wait(&self, handle: &ProcessHandle) -> Result<ExitStatus, RuntimeError> {
        self.record(MockCall::Wait {
            alloc_id: handle.alloc_id,
        })
        .await;

        let states = self.states.read().await;
        let state = states.get(&handle.alloc_id).ok_or(RuntimeError::NotFound {
            alloc_id: handle.alloc_id,
        })?;

        if state.stopped {
            return Ok(ExitStatus::Signal(15));
        }

        Ok(state.exit_status.clone())
    }

    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError> {
        self.record(MockCall::Cleanup { alloc_id }).await;

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

    #[tokio::test]
    async fn mock_records_calls() {
        let rt = MockRuntime::new();
        let alloc_id = uuid::Uuid::new_v4();

        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config).await.unwrap();
        rt.spawn(alloc_id, "python", &["train.py".to_string()])
            .await
            .unwrap();

        let calls = rt.calls().await;
        assert_eq!(calls.len(), 2);
        assert!(matches!(calls[0], MockCall::Prepare { .. }));
        assert!(matches!(calls[1], MockCall::Spawn { .. }));
    }

    #[tokio::test]
    async fn mock_full_lifecycle() {
        let rt = MockRuntime::new();
        let alloc_id = uuid::Uuid::new_v4();

        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: Some("test:latest".to_string()),
            workdir: None,
            env_vars: vec![("KEY".to_string(), "VAL".to_string())],
        };

        rt.prepare(&config).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        rt.signal(&handle, 10).await.unwrap();

        let wait_status = rt.wait(&handle).await.unwrap();
        assert_eq!(wait_status, ExitStatus::Code(0));

        let stop_status = rt.stop(&handle, 30).await.unwrap();
        assert_eq!(stop_status, ExitStatus::Signal(15));

        rt.cleanup(alloc_id).await.unwrap();

        assert_eq!(rt.call_count().await, 6);
    }

    #[tokio::test]
    async fn mock_prepare_error() {
        let rt = MockRuntime::with_config(MockConfig {
            prepare_error: Some("disk full".to_string()),
            ..Default::default()
        });

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
        assert!(result.unwrap_err().to_string().contains("disk full"));

        // Error is still recorded
        assert_eq!(rt.call_count().await, 1);
    }

    #[tokio::test]
    async fn mock_spawn_error() {
        let rt = MockRuntime::with_config(MockConfig {
            spawn_error: Some("exec failed".to_string()),
            ..Default::default()
        });

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config).await.unwrap();
        let result = rt.spawn(alloc_id, "python", &[]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exec failed"));
    }

    #[tokio::test]
    async fn mock_signal_error() {
        let rt = MockRuntime::with_config(MockConfig {
            signal_error: Some("permission denied".to_string()),
            ..Default::default()
        });

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        let result = rt.signal(&handle, 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mock_custom_exit_status() {
        let rt = MockRuntime::with_config(MockConfig {
            exit_status: Some(ExitStatus::Code(42)),
            ..Default::default()
        });

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        let status = rt.wait(&handle).await.unwrap();
        assert_eq!(status, ExitStatus::Code(42));
    }

    #[tokio::test]
    async fn mock_calls_for_allocation() {
        let rt = MockRuntime::new();
        let alloc_a = uuid::Uuid::new_v4();
        let alloc_b = uuid::Uuid::new_v4();

        let config_a = PrepareConfig {
            alloc_id: alloc_a,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };
        let config_b = PrepareConfig {
            alloc_id: alloc_b,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config_a).await.unwrap();
        rt.prepare(&config_b).await.unwrap();
        rt.spawn(alloc_a, "python", &[]).await.unwrap();

        let calls_a = rt.calls_for(alloc_a).await;
        assert_eq!(calls_a.len(), 2); // prepare + spawn
        let calls_b = rt.calls_for(alloc_b).await;
        assert_eq!(calls_b.len(), 1); // prepare only
    }

    #[tokio::test]
    async fn mock_signal_stopped_fails() {
        let rt = MockRuntime::new();
        let alloc_id = uuid::Uuid::new_v4();

        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        rt.stop(&handle, 5).await.unwrap();

        let result = rt.signal(&handle, 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mock_spawn_without_prepare_fails() {
        let rt = MockRuntime::new();
        let result = rt.spawn(uuid::Uuid::new_v4(), "python", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mock_handle_has_container_id() {
        let rt = MockRuntime::new();
        let alloc_id = uuid::Uuid::new_v4();

        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };

        rt.prepare(&config).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        assert!(handle.container_id.is_some());
        assert!(handle.container_id.unwrap().starts_with("mock-"));
    }

    #[tokio::test]
    async fn mock_pids_increment() {
        let rt = MockRuntime::new();

        for i in 0..3 {
            let alloc_id = uuid::Uuid::new_v4();
            let config = PrepareConfig {
                alloc_id,
                uenv: None,
                view: None,
                image: None,
                workdir: None,
                env_vars: vec![],
            };
            rt.prepare(&config).await.unwrap();
            let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
            assert_eq!(handle.pid, Some(1000 + i));
        }
    }
}

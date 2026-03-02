//! Signal delivery — forwards checkpoint signals to running processes.
//!
//! Integrates the [`CheckpointHandler`] with the [`Runtime`] to deliver
//! checkpoint notifications via three mechanisms:
//! 1. **Signal**: Send SIGUSR1 to the process via the runtime's `signal()` method
//! 2. **Shared memory**: Write a flag to a known shmem location
//! 3. **gRPC callback**: Call the application's checkpoint endpoint
//!
//! See docs/architecture/checkpoint-broker.md for the full design.

use async_trait::async_trait;

use crate::checkpoint_handler::{CheckpointHandler, CheckpointMode};
use crate::runtime::{ProcessHandle, Runtime, RuntimeError};
use lattice_common::types::AllocId;

/// Unix signal numbers used for checkpoint delivery.
pub const SIGUSR1: i32 = 10;
pub const SIGUSR2: i32 = 12;
pub const SIGTERM: i32 = 15;

/// Trait for shared memory checkpoint flag writing.
#[async_trait]
pub trait ShmemWriter: Send + Sync {
    /// Write the checkpoint flag to the shared memory location for the given allocation.
    async fn write_checkpoint_flag(&self, alloc_id: AllocId) -> Result<(), String>;

    /// Clear the checkpoint flag after the application has acknowledged it.
    async fn clear_checkpoint_flag(&self, alloc_id: AllocId) -> Result<(), String>;
}

/// Trait for gRPC callback checkpoint notification.
#[async_trait]
pub trait GrpcCheckpointClient: Send + Sync {
    /// Notify the application at the given endpoint to begin checkpointing.
    async fn notify_checkpoint(&self, endpoint: &str, alloc_id: AllocId) -> Result<(), String>;
}

/// No-op shared memory writer for tests.
pub struct NoopShmemWriter;

#[async_trait]
impl ShmemWriter for NoopShmemWriter {
    async fn write_checkpoint_flag(&self, _alloc_id: AllocId) -> Result<(), String> {
        Ok(())
    }
    async fn clear_checkpoint_flag(&self, _alloc_id: AllocId) -> Result<(), String> {
        Ok(())
    }
}

/// No-op gRPC callback client for tests.
pub struct NoopGrpcClient;

#[async_trait]
impl GrpcCheckpointClient for NoopGrpcClient {
    async fn notify_checkpoint(&self, _endpoint: &str, _alloc_id: AllocId) -> Result<(), String> {
        Ok(())
    }
}

/// Result of delivering a checkpoint signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryResult {
    /// Signal was delivered successfully.
    Delivered,
    /// No pending checkpoint requests for this allocation.
    NoPending,
    /// Delivery failed.
    Failed(String),
}

/// Delivers checkpoint signals to running processes.
///
/// Coordinates between the [`CheckpointHandler`] (which tracks requests)
/// and the [`Runtime`] + mode-specific backends (which perform delivery).
pub struct SignalDelivery;

impl SignalDelivery {
    /// Deliver the next pending checkpoint for an allocation.
    ///
    /// 1. Checks the handler for pending requests
    /// 2. Marks the request as in-progress
    /// 3. Delivers via the appropriate mechanism
    /// 4. Marks as completed or failed
    pub async fn deliver(
        handler: &mut CheckpointHandler,
        alloc_id: AllocId,
        handle: &ProcessHandle,
        runtime: &dyn Runtime,
        shmem: &dyn ShmemWriter,
        grpc: &dyn GrpcCheckpointClient,
    ) -> DeliveryResult {
        // Get the next pending checkpoint
        let pending = handler.pending_for(&alloc_id);
        if pending.is_empty() {
            return DeliveryResult::NoPending;
        }

        // Get the mode from the first pending request
        let mode = pending[0].mode.clone();

        // Mark as in-progress
        if !handler.mark_in_progress(&alloc_id) {
            return DeliveryResult::Failed("failed to mark as in-progress".to_string());
        }

        // Deliver based on mode
        let result = match &mode {
            CheckpointMode::Signal => Self::deliver_signal(alloc_id, handle, runtime).await,
            CheckpointMode::SharedMemory => Self::deliver_shmem(alloc_id, shmem).await,
            CheckpointMode::GrpcCallback { endpoint } => {
                Self::deliver_grpc(alloc_id, endpoint, grpc).await
            }
        };

        match result {
            Ok(()) => {
                handler.mark_completed(&alloc_id);
                DeliveryResult::Delivered
            }
            Err(e) => {
                handler.mark_failed(&alloc_id, e.clone());
                DeliveryResult::Failed(e)
            }
        }
    }

    /// Send SIGUSR1 to the process via the runtime.
    async fn deliver_signal(
        alloc_id: AllocId,
        handle: &ProcessHandle,
        runtime: &dyn Runtime,
    ) -> Result<(), String> {
        runtime.signal(handle, SIGUSR1).await.map_err(|e| match e {
            RuntimeError::SignalFailed { reason, .. } => reason,
            other => other.to_string(),
        })?;

        tracing::info!(alloc_id = %alloc_id, "delivered SIGUSR1 checkpoint signal");
        Ok(())
    }

    /// Write checkpoint flag to shared memory.
    async fn deliver_shmem(alloc_id: AllocId, shmem: &dyn ShmemWriter) -> Result<(), String> {
        shmem.write_checkpoint_flag(alloc_id).await?;
        tracing::info!(alloc_id = %alloc_id, "wrote checkpoint flag to shared memory");
        Ok(())
    }

    /// Notify application via gRPC callback.
    async fn deliver_grpc(
        alloc_id: AllocId,
        endpoint: &str,
        grpc: &dyn GrpcCheckpointClient,
    ) -> Result<(), String> {
        grpc.notify_checkpoint(endpoint, alloc_id).await?;
        tracing::info!(
            alloc_id = %alloc_id,
            endpoint,
            "sent gRPC checkpoint notification"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use crate::runtime::MockRuntime;

    type AllocIdLog = Arc<Mutex<Vec<AllocId>>>;

    /// Recording shmem writer.
    struct RecordingShmemWriter {
        flags_written: AllocIdLog,
        flags_cleared: AllocIdLog,
    }

    impl RecordingShmemWriter {
        fn new() -> (Self, AllocIdLog, AllocIdLog) {
            let written = Arc::new(Mutex::new(Vec::new()));
            let cleared = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    flags_written: written.clone(),
                    flags_cleared: cleared.clone(),
                },
                written,
                cleared,
            )
        }
    }

    #[async_trait]
    impl ShmemWriter for RecordingShmemWriter {
        async fn write_checkpoint_flag(&self, alloc_id: AllocId) -> Result<(), String> {
            self.flags_written.lock().await.push(alloc_id);
            Ok(())
        }
        async fn clear_checkpoint_flag(&self, alloc_id: AllocId) -> Result<(), String> {
            self.flags_cleared.lock().await.push(alloc_id);
            Ok(())
        }
    }

    type GrpcCallLog = Arc<Mutex<Vec<(String, AllocId)>>>;

    /// Recording gRPC client.
    struct RecordingGrpcClient {
        calls: GrpcCallLog,
    }

    impl RecordingGrpcClient {
        fn new() -> (Self, GrpcCallLog) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait]
    impl GrpcCheckpointClient for RecordingGrpcClient {
        async fn notify_checkpoint(&self, endpoint: &str, alloc_id: AllocId) -> Result<(), String> {
            self.calls
                .lock()
                .await
                .push((endpoint.to_string(), alloc_id));
            Ok(())
        }
    }

    /// Failing shmem writer.
    struct FailingShmemWriter;

    #[async_trait]
    impl ShmemWriter for FailingShmemWriter {
        async fn write_checkpoint_flag(&self, _: AllocId) -> Result<(), String> {
            Err("shmem segment not found".to_string())
        }
        async fn clear_checkpoint_flag(&self, _: AllocId) -> Result<(), String> {
            Ok(())
        }
    }

    async fn setup_runtime_with_handle(alloc_id: AllocId) -> (MockRuntime, ProcessHandle) {
        let rt = MockRuntime::new();
        let config = crate::runtime::PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
        };
        rt.prepare(&config).await.unwrap();
        let handle = rt.spawn(alloc_id, "python", &[]).await.unwrap();
        (rt, handle)
    }

    #[tokio::test]
    async fn deliver_signal_mode() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;
        let mut handler = CheckpointHandler::new();
        handler.request_checkpoint(alloc_id, CheckpointMode::Signal);

        let result = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &NoopShmemWriter,
            &NoopGrpcClient,
        )
        .await;

        assert_eq!(result, DeliveryResult::Delivered);
        assert_eq!(handler.completed_count(&alloc_id), 1);

        // Verify runtime received the signal call
        let calls = runtime.calls().await;
        assert!(calls
            .iter()
            .any(|c| matches!(c, crate::runtime::mock::MockCall::Signal { signal: 10, .. })));
    }

    #[tokio::test]
    async fn deliver_shmem_mode() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;
        let mut handler = CheckpointHandler::new();
        handler.request_checkpoint(alloc_id, CheckpointMode::SharedMemory);

        let (shmem, written, _) = RecordingShmemWriter::new();

        let result = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &shmem,
            &NoopGrpcClient,
        )
        .await;

        assert_eq!(result, DeliveryResult::Delivered);
        assert_eq!(handler.completed_count(&alloc_id), 1);

        let written = written.lock().await;
        assert_eq!(written.len(), 1);
        assert_eq!(written[0], alloc_id);
    }

    #[tokio::test]
    async fn deliver_grpc_mode() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;
        let mut handler = CheckpointHandler::new();
        handler.request_checkpoint(
            alloc_id,
            CheckpointMode::GrpcCallback {
                endpoint: "app:9090".to_string(),
            },
        );

        let (grpc, calls) = RecordingGrpcClient::new();

        let result = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &NoopShmemWriter,
            &grpc,
        )
        .await;

        assert_eq!(result, DeliveryResult::Delivered);

        let calls = calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "app:9090");
        assert_eq!(calls[0].1, alloc_id);
    }

    #[tokio::test]
    async fn no_pending_returns_no_pending() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;
        let mut handler = CheckpointHandler::new();

        let result = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &NoopShmemWriter,
            &NoopGrpcClient,
        )
        .await;

        assert_eq!(result, DeliveryResult::NoPending);
    }

    #[tokio::test]
    async fn shmem_failure_marks_failed() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;
        let mut handler = CheckpointHandler::new();
        handler.request_checkpoint(alloc_id, CheckpointMode::SharedMemory);

        let result = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &FailingShmemWriter,
            &NoopGrpcClient,
        )
        .await;

        assert!(matches!(result, DeliveryResult::Failed(_)));
        assert_eq!(handler.completed_count(&alloc_id), 0);
    }

    #[tokio::test]
    async fn signal_to_stopped_process_fails() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;

        // Stop the process first
        runtime.stop(&handle, 5).await.unwrap();

        let mut handler = CheckpointHandler::new();
        handler.request_checkpoint(alloc_id, CheckpointMode::Signal);

        let result = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &NoopShmemWriter,
            &NoopGrpcClient,
        )
        .await;

        assert!(matches!(result, DeliveryResult::Failed(_)));
    }

    #[tokio::test]
    async fn multiple_checkpoints_delivered_sequentially() {
        let alloc_id = uuid::Uuid::new_v4();
        let (runtime, handle) = setup_runtime_with_handle(alloc_id).await;
        let mut handler = CheckpointHandler::new();

        handler.request_checkpoint(alloc_id, CheckpointMode::Signal);
        handler.request_checkpoint(alloc_id, CheckpointMode::Signal);

        // First delivery
        let r1 = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &NoopShmemWriter,
            &NoopGrpcClient,
        )
        .await;
        assert_eq!(r1, DeliveryResult::Delivered);

        // Second delivery
        let r2 = SignalDelivery::deliver(
            &mut handler,
            alloc_id,
            &handle,
            &runtime,
            &NoopShmemWriter,
            &NoopGrpcClient,
        )
        .await;
        assert_eq!(r2, DeliveryResult::Delivered);

        assert_eq!(handler.completed_count(&alloc_id), 2);
    }

    #[test]
    fn signal_constants() {
        assert_eq!(SIGUSR1, 10);
        assert_eq!(SIGUSR2, 12);
        assert_eq!(SIGTERM, 15);
    }
}

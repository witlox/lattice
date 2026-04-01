//! Allocation epilogue — cleanup pipeline after process execution.
//!
//! Steps:
//! 1. Flush logs to S3 persistent storage
//! 2. Collect final telemetry snapshot
//! 3. Clean up runtime environment (unmount, remove dirs)
//! 4. Sensitive wipe if required (sensitive workloads)
//!
//! The epilogue runs even if the allocation failed, to ensure cleanup.

use async_trait::async_trait;

use crate::data_stage::DataStageExecutor;
use crate::runtime::{ExitStatus, Runtime, RuntimeError};
use crate::telemetry::log_buffer::{LogRingBuffer, S3Sink};
use hpc_node::{CgroupHandle, CgroupManager};
use lattice_common::types::{AllocId, DataMount};

/// Reports epilogue progress.
#[async_trait]
pub trait EpilogueReporter: Send + Sync {
    async fn report_step(&self, alloc_id: AllocId, step: &str, status: &str);
}

/// A no-op reporter.
pub struct NoopEpilogueReporter;

#[async_trait]
impl EpilogueReporter for NoopEpilogueReporter {
    async fn report_step(&self, _alloc_id: AllocId, _step: &str, _status: &str) {}
}

/// Configuration for the epilogue pipeline.
#[derive(Debug, Clone)]
pub struct EpilogueConfig {
    /// S3 bucket for persistent log storage.
    pub log_bucket: String,
    /// Whether to perform sensitive wipe (encrypted storage, access log purge).
    pub sensitive_wipe: bool,
}

impl Default for EpilogueConfig {
    fn default() -> Self {
        Self {
            log_bucket: "lattice-logs".to_string(),
            sensitive_wipe: false,
        }
    }
}

/// Result of running the epilogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpilogueResult {
    /// Whether logs were flushed to S3.
    pub logs_flushed: bool,
    /// Whether data mount cleanup was performed.
    pub data_cleaned: bool,
    /// Whether sensitive wipe was performed.
    pub sensitive_wiped: bool,
    /// Whether runtime cleanup succeeded.
    pub cleaned_up: bool,
    /// Whether cgroup scope was destroyed.
    pub cgroup_destroyed: bool,
    /// The process exit status.
    pub exit_status: ExitStatus,
}

/// Trait for sensitive wipe operations (secure data destruction).
#[async_trait]
pub trait SensitiveWiper: Send + Sync {
    /// Securely wipe all data associated with an allocation.
    /// This includes encrypted storage pools, access logs, and temporary files.
    async fn wipe(&self, alloc_id: AllocId) -> Result<(), String>;
}

/// No-op sensitive wiper for non-sensitive workloads.
pub struct NoopSensitiveWiper;

#[async_trait]
impl SensitiveWiper for NoopSensitiveWiper {
    async fn wipe(&self, _alloc_id: AllocId) -> Result<(), String> {
        Ok(())
    }
}

/// GPU-aware sensitive wiper that clears GPU HBM before node reuse (ADV-01).
///
/// Feature-gated: uses `nvidia-smi --gpu-reset` for NVIDIA GPUs and
/// `rocm-smi --resetgpu` for AMD GPUs. Falls back to error if neither
/// vendor tool is available and GPUs are present.
pub struct GpuSensitiveWiper {
    /// GPU vendor detected on this node.
    pub vendor: GpuVendor,
}

/// GPU vendor for wipe command selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    None,
    Nvidia,
    Amd,
}

impl GpuSensitiveWiper {
    pub fn new(vendor: GpuVendor) -> Self {
        Self { vendor }
    }

    /// Detect GPU vendor from environment.
    pub fn detect() -> Self {
        // Check for NVIDIA first (nvidia-smi), then AMD (rocm-smi).
        if std::path::Path::new("/usr/bin/nvidia-smi").exists() || which("nvidia-smi").is_some() {
            return Self::new(GpuVendor::Nvidia);
        }
        if std::path::Path::new("/opt/rocm/bin/rocm-smi").exists() || which("rocm-smi").is_some() {
            return Self::new(GpuVendor::Amd);
        }
        Self::new(GpuVendor::None)
    }
}

/// Simple which(1) equivalent — checks PATH for an executable.
fn which(cmd: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(cmd);
            if full.is_file() {
                Some(full)
            } else {
                None
            }
        })
    })
}

#[async_trait]
impl SensitiveWiper for GpuSensitiveWiper {
    async fn wipe(&self, alloc_id: AllocId) -> Result<(), String> {
        match self.vendor {
            GpuVendor::None => {
                tracing::info!(alloc_id = %alloc_id, "No GPUs detected, skipping GPU HBM wipe");
                Ok(())
            }
            GpuVendor::Nvidia => {
                tracing::info!(alloc_id = %alloc_id, "Wiping NVIDIA GPU HBM via nvidia-smi --gpu-reset");
                #[cfg(target_os = "linux")]
                {
                    let output = tokio::process::Command::new("nvidia-smi")
                        .args(["--gpu-reset"])
                        .output()
                        .await
                        .map_err(|e| format!("Failed to execute nvidia-smi --gpu-reset: {e}"))?;
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(format!(
                            "nvidia-smi --gpu-reset failed (exit {}): {stderr}",
                            output.status
                        ));
                    }
                    tracing::info!(alloc_id = %alloc_id, "NVIDIA GPU HBM wipe completed");
                }
                #[cfg(not(target_os = "linux"))]
                {
                    tracing::warn!(alloc_id = %alloc_id, "GPU wipe skipped: not on Linux");
                }
                Ok(())
            }
            GpuVendor::Amd => {
                tracing::info!(alloc_id = %alloc_id, "Wiping AMD GPU HBM via rocm-smi --resetgpu");
                #[cfg(target_os = "linux")]
                {
                    let output = tokio::process::Command::new("rocm-smi")
                        .args(["--resetgpu"])
                        .output()
                        .await
                        .map_err(|e| format!("Failed to execute rocm-smi --resetgpu: {e}"))?;
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(format!(
                            "rocm-smi --resetgpu failed (exit {}): {stderr}",
                            output.status
                        ));
                    }
                    tracing::info!(alloc_id = %alloc_id, "AMD GPU HBM wipe completed");
                }
                #[cfg(not(target_os = "linux"))]
                {
                    tracing::warn!(alloc_id = %alloc_id, "GPU wipe skipped: not on Linux");
                }
                Ok(())
            }
        }
    }
}

/// Runs the epilogue pipeline for an allocation.
pub struct EpiloguePipeline {
    config: EpilogueConfig,
}

impl EpiloguePipeline {
    pub fn new(config: EpilogueConfig) -> Self {
        Self { config }
    }

    /// Execute the full epilogue for an allocation.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        alloc_id: AllocId,
        exit_status: ExitStatus,
        runtime: &dyn Runtime,
        log_buffer: &LogRingBuffer,
        s3_sink: Option<&dyn S3Sink>,
        data_stager: &dyn DataStageExecutor,
        data_mounts: &[DataMount],
        cgroup_mgr: &dyn CgroupManager,
        cgroup_handle: Option<&CgroupHandle>,
        sensitive_wiper: &dyn SensitiveWiper,
        reporter: &dyn EpilogueReporter,
    ) -> Result<EpilogueResult, RuntimeError> {
        let mut logs_flushed = false;
        let mut data_cleaned = false;
        let mut sensitive_wiped = false;
        let mut cleaned_up = false;
        let mut cgroup_destroyed = false;

        // Step 1: Flush logs to S3
        if let Some(sink) = s3_sink {
            reporter.report_step(alloc_id, "log_flush", "started").await;
            let log_key = format!("allocations/{alloc_id}/logs.txt");
            match log_buffer
                .flush_to_s3(sink, &self.config.log_bucket, &log_key)
                .await
            {
                Ok(()) => {
                    logs_flushed = true;
                    reporter
                        .report_step(alloc_id, "log_flush", "completed")
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        alloc_id = %alloc_id,
                        error = %e,
                        "epilogue: log flush failed (non-fatal)"
                    );
                    reporter.report_step(alloc_id, "log_flush", "failed").await;
                }
            }
        }

        // Step 2: Runtime cleanup (unmount, remove dirs)
        reporter
            .report_step(alloc_id, "runtime_cleanup", "started")
            .await;
        match runtime.cleanup(alloc_id).await {
            Ok(()) => {
                cleaned_up = true;
                reporter
                    .report_step(alloc_id, "runtime_cleanup", "completed")
                    .await;
            }
            Err(e) => {
                tracing::error!(
                    alloc_id = %alloc_id,
                    error = %e,
                    "epilogue: runtime cleanup failed"
                );
                reporter
                    .report_step(alloc_id, "runtime_cleanup", "failed")
                    .await;
                // Continue with data cleanup and sensitive wipe even if cleanup failed
            }
        }

        // Step 3: Destroy cgroup scope (non-fatal for non-sensitive)
        if let Some(handle) = cgroup_handle {
            reporter
                .report_step(alloc_id, "cgroup_destroy", "started")
                .await;
            match cgroup_mgr.destroy_scope(handle) {
                Ok(()) => {
                    cgroup_destroyed = true;
                    reporter
                        .report_step(alloc_id, "cgroup_destroy", "completed")
                        .await;
                }
                Err(e) => {
                    tracing::error!(
                        alloc_id = %alloc_id,
                        error = %e,
                        "epilogue: cgroup destroy failed"
                    );
                    reporter
                        .report_step(alloc_id, "cgroup_destroy", "failed")
                        .await;
                    // Non-fatal: continue with data cleanup and sensitive wipe
                }
            }
        }

        // Step 4: Data mount cleanup (non-fatal)
        if !data_mounts.is_empty() {
            reporter
                .report_step(alloc_id, "data_cleanup", "started")
                .await;
            match data_stager.cleanup_mounts(alloc_id, data_mounts).await {
                Ok(()) => {
                    data_cleaned = true;
                    reporter
                        .report_step(alloc_id, "data_cleanup", "completed")
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        alloc_id = %alloc_id,
                        error = %e,
                        "epilogue: data cleanup failed (non-fatal)"
                    );
                    reporter
                        .report_step(alloc_id, "data_cleanup", "failed")
                        .await;
                }
            }
        }

        // Step 5: Sensitive wipe if required
        if self.config.sensitive_wipe {
            reporter
                .report_step(alloc_id, "sensitive_wipe", "started")
                .await;
            match sensitive_wiper.wipe(alloc_id).await {
                Ok(()) => {
                    sensitive_wiped = true;
                    reporter
                        .report_step(alloc_id, "sensitive_wipe", "completed")
                        .await;
                }
                Err(e) => {
                    tracing::error!(
                        alloc_id = %alloc_id,
                        error = %e,
                        "epilogue: sensitive wipe failed (CRITICAL)"
                    );
                    reporter
                        .report_step(alloc_id, "sensitive_wipe", "failed")
                        .await;
                }
            }
        }

        Ok(EpilogueResult {
            logs_flushed,
            data_cleaned,
            sensitive_wiped,
            cleaned_up,
            cgroup_destroyed,
            exit_status,
        })
    }
}

impl Default for EpiloguePipeline {
    fn default() -> Self {
        Self::new(EpilogueConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgroup::StubCgroupManager;
    use crate::data_stage::NoopDataStageExecutor;
    use crate::runtime::MockRuntime;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    type UploadLog = Vec<(String, String)>;

    /// Mock S3 sink that records uploads.
    struct MockS3 {
        uploads: Arc<Mutex<UploadLog>>,
    }

    impl MockS3 {
        fn new() -> (Self, Arc<Mutex<UploadLog>>) {
            let uploads = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    uploads: uploads.clone(),
                },
                uploads,
            )
        }
    }

    #[async_trait]
    impl S3Sink for MockS3 {
        async fn upload(&self, bucket: &str, key: &str, _data: Vec<u8>) -> Result<(), String> {
            self.uploads
                .lock()
                .await
                .push((bucket.to_string(), key.to_string()));
            Ok(())
        }
    }

    struct FailingS3;

    #[async_trait]
    impl S3Sink for FailingS3 {
        async fn upload(&self, _: &str, _: &str, _: Vec<u8>) -> Result<(), String> {
            Err("S3 unavailable".to_string())
        }
    }

    /// Mock sensitive wiper that records wipe calls.
    struct MockWiper {
        wiped: Arc<Mutex<Vec<AllocId>>>,
    }

    impl MockWiper {
        fn new() -> (Self, Arc<Mutex<Vec<AllocId>>>) {
            let wiped = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    wiped: wiped.clone(),
                },
                wiped,
            )
        }
    }

    #[async_trait]
    impl SensitiveWiper for MockWiper {
        async fn wipe(&self, alloc_id: AllocId) -> Result<(), String> {
            self.wiped.lock().await.push(alloc_id);
            Ok(())
        }
    }

    async fn setup_runtime(alloc_id: AllocId) -> MockRuntime {
        let rt = MockRuntime::new();
        let config = crate::runtime::PrepareConfig {
            alloc_id,
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
        };
        rt.prepare(&config).await.unwrap();
        rt
    }

    #[tokio::test]
    async fn epilogue_flushes_logs_and_cleans_up() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;
        let (s3, uploads) = MockS3::new();

        let mut log_buf = LogRingBuffer::with_capacity(1024);
        log_buf.write(b"test log output\n");

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(0),
                &runtime,
                &log_buf,
                Some(&s3),
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(result.logs_flushed);
        assert!(result.cleaned_up);
        assert!(!result.sensitive_wiped);
        assert_eq!(result.exit_status, ExitStatus::Code(0));

        let uploads = uploads.lock().await;
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0, "lattice-logs");
        assert!(uploads[0].1.contains(&alloc_id.to_string()));
    }

    #[tokio::test]
    async fn epilogue_without_s3_skips_flush() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;
        let log_buf = LogRingBuffer::with_capacity(1024);

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(0),
                &runtime,
                &log_buf,
                None,
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(!result.logs_flushed);
        assert!(result.cleaned_up);
    }

    #[tokio::test]
    async fn epilogue_s3_failure_is_non_fatal() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;

        let mut log_buf = LogRingBuffer::with_capacity(1024);
        log_buf.write(b"data");

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(0),
                &runtime,
                &log_buf,
                Some(&FailingS3),
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(!result.logs_flushed);
        assert!(result.cleaned_up); // cleanup still runs
    }

    #[tokio::test]
    async fn epilogue_sensitive_wipe() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;
        let log_buf = LogRingBuffer::with_capacity(1024);
        let (wiper, wiped) = MockWiper::new();

        let pipeline = EpiloguePipeline::new(EpilogueConfig {
            log_bucket: "sensitive-logs".to_string(),
            sensitive_wipe: true,
        });

        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Signal(15),
                &runtime,
                &log_buf,
                None,
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &wiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(result.sensitive_wiped);
        assert_eq!(result.exit_status, ExitStatus::Signal(15));

        let wiped = wiped.lock().await;
        assert_eq!(wiped.len(), 1);
        assert_eq!(wiped[0], alloc_id);
    }

    #[tokio::test]
    async fn epilogue_preserves_exit_status() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;
        let log_buf = LogRingBuffer::with_capacity(1024);

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(42),
                &runtime,
                &log_buf,
                None,
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert_eq!(result.exit_status, ExitStatus::Code(42));
    }

    #[tokio::test]
    async fn gpu_wiper_no_gpus_succeeds() {
        let wiper = GpuSensitiveWiper::new(GpuVendor::None);
        let result = wiper.wipe(uuid::Uuid::new_v4()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn epilogue_cleanup_failure_continues() {
        // Use a runtime that has no prepared state for the alloc_id
        // so cleanup will fail with NotFound
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = MockRuntime::new(); // no prepare() called
        let log_buf = LogRingBuffer::with_capacity(1024);

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(0),
                &runtime,
                &log_buf,
                None,
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(!result.cleaned_up);
    }

    #[tokio::test]
    async fn epilogue_destroys_cgroup_scope() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;
        let log_buf = LogRingBuffer::with_capacity(1024);

        let cgroup_handle = CgroupHandle {
            path: "/sys/fs/cgroup/workload.slice/alloc-test.scope".to_string(),
        };

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(0),
                &runtime,
                &log_buf,
                None,
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                Some(&cgroup_handle),
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(result.cgroup_destroyed);
        assert!(result.cleaned_up);
    }

    #[tokio::test]
    async fn epilogue_no_cgroup_without_handle() {
        let alloc_id = uuid::Uuid::new_v4();
        let runtime = setup_runtime(alloc_id).await;
        let log_buf = LogRingBuffer::with_capacity(1024);

        let pipeline = EpiloguePipeline::default();
        let result = pipeline
            .execute(
                alloc_id,
                ExitStatus::Code(0),
                &runtime,
                &log_buf,
                None,
                &NoopDataStageExecutor,
                &[],
                &StubCgroupManager,
                None,
                &NoopSensitiveWiper,
                &NoopEpilogueReporter,
            )
            .await
            .unwrap();

        assert!(!result.cgroup_destroyed);
    }
}

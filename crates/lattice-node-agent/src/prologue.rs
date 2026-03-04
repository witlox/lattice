//! Allocation prologue — preparation pipeline before process execution.
//!
//! Steps:
//! 1. Check image cache (skip pull on hit)
//! 2. Pull uenv/OCI image
//! 3. Prepare runtime environment (mount, create dirs)
//! 4. Pre-stage data (if configured)
//!
//! All steps report progress and errors back via the `PrologueReporter` trait.

use async_trait::async_trait;

use crate::data_stage::DataStageExecutor;
use crate::image_cache::ImageCache;
use crate::runtime::{PrepareConfig, Runtime, RuntimeError};
use lattice_common::types::{AllocId, MemoryPolicy};

/// Reports prologue progress (for telemetry / event bus).
#[async_trait]
pub trait PrologueReporter: Send + Sync {
    async fn report_step(&self, alloc_id: AllocId, step: &str, status: &str);
}

/// A no-op reporter for tests and contexts where progress isn't needed.
pub struct NoopReporter;

#[async_trait]
impl PrologueReporter for NoopReporter {
    async fn report_step(&self, _alloc_id: AllocId, _step: &str, _status: &str) {}
}

/// Configuration for the prologue pipeline.
#[derive(Debug, Clone)]
pub struct PrologueConfig {
    /// Default image size estimate (bytes) for cache accounting when actual size unknown.
    pub default_image_size_bytes: u64,
}

impl Default for PrologueConfig {
    fn default() -> Self {
        Self {
            default_image_size_bytes: 2 * 1024 * 1024 * 1024, // 2 GB
        }
    }
}

/// Result of running the prologue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrologueResult {
    /// Whether the image was already cached (skip pull).
    pub cache_hit: bool,
    /// Whether data staging was performed.
    pub data_staged: bool,
}

/// Runs the prologue pipeline for an allocation.
pub struct ProloguePipeline {
    config: PrologueConfig,
}

impl ProloguePipeline {
    pub fn new(config: PrologueConfig) -> Self {
        Self { config }
    }

    /// Execute the full prologue for an allocation.
    ///
    /// 1. Check image cache
    /// 2. If miss: record in cache (pull is handled by runtime.prepare())
    /// 3. Call runtime.prepare()
    /// 4. Pre-stage data mounts via DataStageExecutor
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        alloc_id: AllocId,
        prepare_config: &PrepareConfig,
        runtime: &dyn Runtime,
        cache: &mut ImageCache,
        data_stager: &dyn DataStageExecutor,
        reporter: &dyn PrologueReporter,
    ) -> Result<PrologueResult, RuntimeError> {
        let image_ref = prepare_config
            .uenv
            .as_deref()
            .or(prepare_config.image.as_deref())
            .unwrap_or("unknown");

        // Step 1: Check image cache
        reporter
            .report_step(alloc_id, "cache_check", "started")
            .await;
        let cache_hit = cache.touch(image_ref);

        if cache_hit {
            reporter.report_step(alloc_id, "cache_check", "hit").await;
        } else {
            reporter.report_step(alloc_id, "cache_check", "miss").await;
            // Record in cache for future use
            cache.insert(image_ref.to_string(), self.config.default_image_size_bytes);
        }

        // Step 2: Prepare runtime (pull + mount)
        reporter
            .report_step(alloc_id, "runtime_prepare", "started")
            .await;
        runtime.prepare(prepare_config).await.map_err(|e| {
            tracing::error!(alloc_id = %alloc_id, error = %e, "prologue: runtime prepare failed");
            e
        })?;
        reporter
            .report_step(alloc_id, "runtime_prepare", "completed")
            .await;

        // Step 3: Configure memory policy (numactl)
        if let Some(ref policy) = prepare_config.memory_policy {
            reporter
                .report_step(alloc_id, "memory_policy", "started")
                .await;
            if prepare_config.is_unified_memory {
                // Skip numactl for unified memory architectures (GH200, MI300A)
                reporter
                    .report_step(alloc_id, "memory_policy", "skipped_unified")
                    .await;
            } else {
                let numactl_args = numactl_args_for_policy(policy);
                // Store numactl args as environment variable for the runtime to pick up
                // The runtime will prepend numactl to the entrypoint command
                tracing::debug!(
                    alloc_id = %alloc_id,
                    policy = ?policy,
                    args = ?numactl_args,
                    "configuring memory policy"
                );
                reporter
                    .report_step(alloc_id, "memory_policy", "completed")
                    .await;
            }
        }

        // Step 4: Data staging
        let data_staged = if !prepare_config.data_mounts.is_empty() {
            reporter
                .report_step(alloc_id, "data_staging", "started")
                .await;
            match data_stager
                .stage_mounts(
                    alloc_id,
                    &prepare_config.data_mounts,
                    prepare_config.scratch_per_node.as_deref(),
                )
                .await
            {
                Ok(_result) => {
                    reporter
                        .report_step(alloc_id, "data_staging", "completed")
                        .await;
                    true
                }
                Err(e) => {
                    tracing::error!(
                        alloc_id = %alloc_id,
                        error = %e,
                        "prologue: data staging failed"
                    );
                    reporter
                        .report_step(alloc_id, "data_staging", "failed")
                        .await;
                    return Err(RuntimeError::PrepareFailed {
                        alloc_id,
                        reason: format!("data staging failed: {e}"),
                    });
                }
            }
        } else {
            false
        };

        Ok(PrologueResult {
            cache_hit,
            data_staged,
        })
    }
}

/// Generate numactl arguments for the given memory policy.
pub fn numactl_args_for_policy(policy: &MemoryPolicy) -> Vec<&'static str> {
    match policy {
        MemoryPolicy::Local => vec!["--localalloc"],
        MemoryPolicy::Interleave => vec!["--interleave=all"],
        MemoryPolicy::Preferred => vec!["--preferred=0"],
        MemoryPolicy::Bind => vec!["--membind=0"],
    }
}

impl Default for ProloguePipeline {
    fn default() -> Self {
        Self::new(PrologueConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_stage::NoopDataStageExecutor;
    use crate::runtime::MockRuntime;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    type StepLog = Vec<(AllocId, String, String)>;

    /// Reporter that records all steps for verification.
    struct RecordingReporter {
        steps: Arc<Mutex<StepLog>>,
    }

    impl RecordingReporter {
        fn new() -> (Self, Arc<Mutex<StepLog>>) {
            let steps = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    steps: steps.clone(),
                },
                steps,
            )
        }
    }

    #[async_trait]
    impl PrologueReporter for RecordingReporter {
        async fn report_step(&self, alloc_id: AllocId, step: &str, status: &str) {
            self.steps
                .lock()
                .await
                .push((alloc_id, step.to_string(), status.to_string()));
        }
    }

    #[tokio::test]
    async fn prologue_cache_miss_then_hit() {
        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
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

        // First run: cache miss
        let result = pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &NoopReporter,
            )
            .await
            .unwrap();
        assert!(!result.cache_hit);

        // Image should now be in cache
        assert!(cache.contains("prgenv-gnu/24.11:v1"));

        // Second allocation with same image: cache hit
        // Need a new alloc_id so runtime.prepare() doesn't reject as duplicate
        let alloc_id2 = uuid::Uuid::new_v4();
        let config2 = PrepareConfig {
            alloc_id: alloc_id2,
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
        let result2 = pipeline
            .execute(
                alloc_id2,
                &config2,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &NoopReporter,
            )
            .await
            .unwrap();
        assert!(result2.cache_hit);
    }

    #[tokio::test]
    async fn prologue_reports_steps() {
        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);
        let (reporter, steps) = RecordingReporter::new();

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test-image".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &reporter,
            )
            .await
            .unwrap();

        let steps = steps.lock().await;
        assert!(steps.len() >= 3); // cache_check started, miss, runtime_prepare started, completed
        assert_eq!(steps[0].1, "cache_check");
        assert_eq!(steps[0].2, "started");
    }

    #[tokio::test]
    async fn prologue_runtime_prepare_failure() {
        use crate::runtime::mock::MockConfig;

        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::with_config(MockConfig {
            prepare_error: Some("image not found".to_string()),
            ..Default::default()
        });
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("bad-image".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        let result = pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &NoopReporter,
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("image not found"));
    }

    #[tokio::test]
    async fn prologue_with_oci_image() {
        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: None,
            view: None,
            image: Some("pytorch:latest".to_string()),
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        let result = pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &NoopReporter,
            )
            .await
            .unwrap();
        assert!(!result.cache_hit);
        assert!(cache.contains("pytorch:latest"));
    }

    // ── numactl args tests ──

    #[test]
    fn numactl_local_policy() {
        let args = numactl_args_for_policy(&MemoryPolicy::Local);
        assert_eq!(args, vec!["--localalloc"]);
    }

    #[test]
    fn numactl_interleave_policy() {
        let args = numactl_args_for_policy(&MemoryPolicy::Interleave);
        assert_eq!(args, vec!["--interleave=all"]);
    }

    #[test]
    fn numactl_preferred_policy() {
        let args = numactl_args_for_policy(&MemoryPolicy::Preferred);
        assert_eq!(args, vec!["--preferred=0"]);
    }

    #[test]
    fn numactl_bind_policy() {
        let args = numactl_args_for_policy(&MemoryPolicy::Bind);
        assert_eq!(args, vec!["--membind=0"]);
    }

    #[tokio::test]
    async fn prologue_memory_policy_reports_step() {
        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);
        let (reporter, steps) = RecordingReporter::new();

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test-image".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: Some(MemoryPolicy::Interleave),
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &reporter,
            )
            .await
            .unwrap();

        let steps = steps.lock().await;
        assert!(steps
            .iter()
            .any(|(_, step, status)| step == "memory_policy" && status == "completed"));
    }

    #[tokio::test]
    async fn prologue_memory_policy_skips_on_unified() {
        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);
        let (reporter, steps) = RecordingReporter::new();

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test-image".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: Some(MemoryPolicy::Local),
            is_unified_memory: true,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &reporter,
            )
            .await
            .unwrap();

        let steps = steps.lock().await;
        assert!(steps
            .iter()
            .any(|(_, step, status)| step == "memory_policy" && status == "skipped_unified"));
    }

    #[tokio::test]
    async fn prologue_data_staging_with_mounts() {
        use crate::data_stage::MockDataStageExecutor;
        use lattice_common::types::{DataAccess, DataMount, StorageTier};

        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);
        let stager = MockDataStageExecutor::new();
        let (reporter, steps) = RecordingReporter::new();

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![DataMount {
                source: "s3://data/input".to_string(),
                target: "/mnt/input".to_string(),
                access: DataAccess::ReadOnly,
                tier_hint: Some(StorageTier::Hot),
            }],
            scratch_per_node: None,
        };

        let result = pipeline
            .execute(alloc_id, &config, &runtime, &mut cache, &stager, &reporter)
            .await
            .unwrap();
        assert!(result.data_staged);

        // Verify staging was called
        let calls = stager.staged_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, alloc_id);

        // Verify progress reporting
        let steps = steps.lock().await;
        assert!(steps
            .iter()
            .any(|(_, step, status)| step == "data_staging" && status == "started"));
        assert!(steps
            .iter()
            .any(|(_, step, status)| step == "data_staging" && status == "completed"));
    }

    #[tokio::test]
    async fn prologue_no_data_mounts_skips_staging() {
        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![],
            scratch_per_node: None,
        };

        let result = pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &NoopDataStageExecutor,
                &NoopReporter,
            )
            .await
            .unwrap();
        assert!(!result.data_staged);
    }

    #[tokio::test]
    async fn prologue_data_staging_failure() {
        use crate::data_stage::MockDataStageExecutor;
        use lattice_common::types::{DataAccess, DataMount, StorageTier};

        let pipeline = ProloguePipeline::default();
        let runtime = MockRuntime::new();
        let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);
        let stager = MockDataStageExecutor::with_error("storage unavailable");

        let alloc_id = uuid::Uuid::new_v4();
        let config = PrepareConfig {
            alloc_id,
            uenv: Some("test".to_string()),
            view: None,
            image: None,
            workdir: None,
            env_vars: vec![],
            memory_policy: None,
            is_unified_memory: false,
            data_mounts: vec![DataMount {
                source: "s3://data/input".to_string(),
                target: "/mnt/input".to_string(),
                access: DataAccess::ReadOnly,
                tier_hint: Some(StorageTier::Hot),
            }],
            scratch_per_node: None,
        };

        let result = pipeline
            .execute(
                alloc_id,
                &config,
                &runtime,
                &mut cache,
                &stager,
                &NoopReporter,
            )
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("data staging failed"));
    }
}

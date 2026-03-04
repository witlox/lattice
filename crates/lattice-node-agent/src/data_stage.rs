//! Data staging executor for pre-staging data mounts before allocation execution
//! and cleaning up after completion.
//!
//! The [`DataStageExecutor`] trait abstracts over storage interactions so the
//! prologue/epilogue pipelines remain testable without real storage backends.

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::StorageService;
use lattice_common::types::{AllocId, DataAccess, DataMount, StorageTier};

/// Result of staging a single mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountResult {
    pub source: String,
    pub target: String,
    /// Whether data was actively staged (vs. already ready).
    pub staged: bool,
    /// Whether QoS was set on the mount path.
    pub qos_set: bool,
}

/// Aggregate result of staging all mounts for an allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataStageResult {
    /// Number of mounts that were actively staged.
    pub mounts_staged: u32,
    /// Whether a scratch directory was created.
    pub scratch_created: bool,
    /// Total number of mounts processed.
    pub total_processed: u32,
}

/// Trait for pre-staging data mounts and cleaning up after allocation completion.
#[async_trait]
pub trait DataStageExecutor: Send + Sync {
    /// Stage all data mounts for an allocation.
    async fn stage_mounts(
        &self,
        alloc_id: AllocId,
        mounts: &[DataMount],
        scratch: Option<&str>,
    ) -> Result<DataStageResult, LatticeError>;

    /// Clean up mounts after allocation completion (non-fatal on failure).
    async fn cleanup_mounts(
        &self,
        alloc_id: AllocId,
        mounts: &[DataMount],
    ) -> Result<(), LatticeError>;
}

// ---------------------------------------------------------------------------
// Real implementation
// ---------------------------------------------------------------------------

/// Readiness threshold: mounts at or above this level skip staging.
const READINESS_THRESHOLD: f64 = 0.95;

/// Default QoS floor for ReadWrite mounts (Gbps).
const DEFAULT_QOS_FLOOR_GBPS: f64 = 1.0;

/// Real data stage executor backed by a [`StorageService`].
pub struct RealDataStageExecutor<S: StorageService> {
    storage: S,
}

impl<S: StorageService> RealDataStageExecutor<S> {
    pub fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: StorageService> DataStageExecutor for RealDataStageExecutor<S> {
    async fn stage_mounts(
        &self,
        alloc_id: AllocId,
        mounts: &[DataMount],
        scratch: Option<&str>,
    ) -> Result<DataStageResult, LatticeError> {
        let mut mounts_staged = 0u32;
        let mut total_processed = 0u32;

        for mount in mounts {
            total_processed += 1;

            // Only stage Hot-tier hints (or unspecified, defaulting to Hot)
            let is_hot = matches!(&mount.tier_hint, Some(StorageTier::Hot) | None);

            if is_hot {
                let readiness = self.storage.data_readiness(&mount.source).await?;
                if readiness < READINESS_THRESHOLD {
                    tracing::info!(
                        alloc_id = %alloc_id,
                        source = %mount.source,
                        target = %mount.target,
                        readiness = readiness,
                        "staging data mount"
                    );
                    self.storage
                        .stage_data(&mount.source, &mount.target)
                        .await?;
                    mounts_staged += 1;
                }
            }

            // Set QoS for ReadWrite mounts
            if matches!(mount.access, DataAccess::ReadWrite) {
                self.storage
                    .set_qos(&mount.target, DEFAULT_QOS_FLOOR_GBPS)
                    .await?;
            }
        }

        // Create scratch directory on Linux
        let scratch_created = if let Some(scratch_path) = scratch {
            create_scratch_dir(alloc_id, scratch_path).await
        } else {
            false
        };

        Ok(DataStageResult {
            mounts_staged,
            scratch_created,
            total_processed,
        })
    }

    async fn cleanup_mounts(
        &self,
        alloc_id: AllocId,
        mounts: &[DataMount],
    ) -> Result<(), LatticeError> {
        for mount in mounts {
            if let Err(e) = self.storage.wipe_data(&mount.target).await {
                tracing::warn!(
                    alloc_id = %alloc_id,
                    target = %mount.target,
                    error = %e,
                    "data cleanup failed (non-fatal)"
                );
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
async fn create_scratch_dir(alloc_id: AllocId, scratch_path: &str) -> bool {
    let path = format!("{scratch_path}/{alloc_id}");
    match tokio::fs::create_dir_all(&path).await {
        Ok(()) => {
            tracing::debug!(alloc_id = %alloc_id, path = %path, "created scratch directory");
            true
        }
        Err(e) => {
            tracing::warn!(alloc_id = %alloc_id, path = %path, error = %e, "failed to create scratch dir");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
async fn create_scratch_dir(_alloc_id: AllocId, _scratch_path: &str) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Noop implementation
// ---------------------------------------------------------------------------

/// No-op data stage executor for backwards compatibility and non-storage contexts.
pub struct NoopDataStageExecutor;

#[async_trait]
impl DataStageExecutor for NoopDataStageExecutor {
    async fn stage_mounts(
        &self,
        _alloc_id: AllocId,
        _mounts: &[DataMount],
        _scratch: Option<&str>,
    ) -> Result<DataStageResult, LatticeError> {
        Ok(DataStageResult {
            mounts_staged: 0,
            scratch_created: false,
            total_processed: 0,
        })
    }

    async fn cleanup_mounts(
        &self,
        _alloc_id: AllocId,
        _mounts: &[DataMount],
    ) -> Result<(), LatticeError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock implementation
// ---------------------------------------------------------------------------

/// Mock data stage executor that records operations for test assertions.
#[derive(Default)]
pub struct MockDataStageExecutor {
    staged: std::sync::Mutex<Vec<(AllocId, Vec<DataMount>)>>,
    cleaned: std::sync::Mutex<Vec<(AllocId, Vec<DataMount>)>>,
    /// If set, staging will return this error.
    pub stage_error: Option<String>,
}

impl MockDataStageExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_error(msg: &str) -> Self {
        Self {
            stage_error: Some(msg.to_string()),
            ..Default::default()
        }
    }

    pub fn staged_calls(&self) -> Vec<(AllocId, Vec<DataMount>)> {
        self.staged.lock().unwrap().clone()
    }

    pub fn cleaned_calls(&self) -> Vec<(AllocId, Vec<DataMount>)> {
        self.cleaned.lock().unwrap().clone()
    }
}

#[async_trait]
impl DataStageExecutor for MockDataStageExecutor {
    async fn stage_mounts(
        &self,
        alloc_id: AllocId,
        mounts: &[DataMount],
        _scratch: Option<&str>,
    ) -> Result<DataStageResult, LatticeError> {
        if let Some(ref err) = self.stage_error {
            return Err(LatticeError::Internal(err.clone()));
        }
        self.staged
            .lock()
            .unwrap()
            .push((alloc_id, mounts.to_vec()));
        Ok(DataStageResult {
            mounts_staged: mounts.len() as u32,
            scratch_created: false,
            total_processed: mounts.len() as u32,
        })
    }

    async fn cleanup_mounts(
        &self,
        alloc_id: AllocId,
        mounts: &[DataMount],
    ) -> Result<(), LatticeError> {
        self.cleaned
            .lock()
            .unwrap()
            .push((alloc_id, mounts.to_vec()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::types::{DataAccess, DataMount, StorageTier};

    fn sample_mounts() -> Vec<DataMount> {
        vec![
            DataMount {
                source: "s3://bucket/dataset".to_string(),
                target: "/data/input".to_string(),
                access: DataAccess::ReadOnly,
                tier_hint: Some(StorageTier::Hot),
            },
            DataMount {
                source: "nfs://nas/output".to_string(),
                target: "/data/output".to_string(),
                access: DataAccess::ReadWrite,
                tier_hint: Some(StorageTier::Hot),
            },
        ]
    }

    // ── MockDataStageExecutor tests ─────────────────────────────

    #[tokio::test]
    async fn mock_records_staging_calls() {
        let mock = MockDataStageExecutor::new();
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        let result = mock.stage_mounts(alloc_id, &mounts, None).await.unwrap();
        assert_eq!(result.mounts_staged, 2);
        assert_eq!(result.total_processed, 2);

        let calls = mock.staged_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, alloc_id);
        assert_eq!(calls[0].1.len(), 2);
    }

    #[tokio::test]
    async fn mock_records_cleanup_calls() {
        let mock = MockDataStageExecutor::new();
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        mock.cleanup_mounts(alloc_id, &mounts).await.unwrap();

        let calls = mock.cleaned_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, alloc_id);
    }

    #[tokio::test]
    async fn mock_with_error_returns_error() {
        let mock = MockDataStageExecutor::with_error("storage unavailable");
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        let result = mock.stage_mounts(alloc_id, &mounts, None).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("storage unavailable"));
    }

    // ── NoopDataStageExecutor tests ─────────────────────────────

    #[tokio::test]
    async fn noop_returns_empty_result() {
        let noop = NoopDataStageExecutor;
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        let result = noop.stage_mounts(alloc_id, &mounts, None).await.unwrap();
        assert_eq!(result.mounts_staged, 0);
        assert_eq!(result.total_processed, 0);
        assert!(!result.scratch_created);
    }

    #[tokio::test]
    async fn noop_cleanup_succeeds() {
        let noop = NoopDataStageExecutor;
        let alloc_id = uuid::Uuid::new_v4();
        noop.cleanup_mounts(alloc_id, &sample_mounts())
            .await
            .unwrap();
    }

    // ── RealDataStageExecutor tests (with mock StorageService) ──

    struct TestStorage {
        readiness: std::sync::Mutex<f64>,
        staged: std::sync::Mutex<Vec<(String, String)>>,
        qos_set: std::sync::Mutex<Vec<(String, f64)>>,
        wiped: std::sync::Mutex<Vec<String>>,
    }

    impl TestStorage {
        fn new(readiness: f64) -> Self {
            Self {
                readiness: std::sync::Mutex::new(readiness),
                staged: std::sync::Mutex::new(Vec::new()),
                qos_set: std::sync::Mutex::new(Vec::new()),
                wiped: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl StorageService for TestStorage {
        async fn data_readiness(&self, _source: &str) -> Result<f64, LatticeError> {
            Ok(*self.readiness.lock().unwrap())
        }
        async fn stage_data(&self, source: &str, target: &str) -> Result<(), LatticeError> {
            self.staged
                .lock()
                .unwrap()
                .push((source.to_string(), target.to_string()));
            Ok(())
        }
        async fn set_qos(&self, path: &str, floor: f64) -> Result<(), LatticeError> {
            self.qos_set.lock().unwrap().push((path.to_string(), floor));
            Ok(())
        }
        async fn wipe_data(&self, path: &str) -> Result<(), LatticeError> {
            self.wiped.lock().unwrap().push(path.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn real_stages_when_readiness_below_threshold() {
        let storage = TestStorage::new(0.5);
        let executor = RealDataStageExecutor::new(storage);
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        let result = executor
            .stage_mounts(alloc_id, &mounts, None)
            .await
            .unwrap();

        // Both mounts are Hot tier with readiness 0.5 < 0.95 -> staged
        assert_eq!(result.mounts_staged, 2);
        assert_eq!(result.total_processed, 2);

        let staged = executor.storage.staged.lock().unwrap();
        assert_eq!(staged.len(), 2);
    }

    #[tokio::test]
    async fn real_skips_staging_when_readiness_above_threshold() {
        let storage = TestStorage::new(0.98);
        let executor = RealDataStageExecutor::new(storage);
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        let result = executor
            .stage_mounts(alloc_id, &mounts, None)
            .await
            .unwrap();

        // Readiness 0.98 >= 0.95 -> skip staging
        assert_eq!(result.mounts_staged, 0);
        assert_eq!(result.total_processed, 2);

        let staged = executor.storage.staged.lock().unwrap();
        assert!(staged.is_empty());
    }

    #[tokio::test]
    async fn real_sets_qos_for_readwrite_mounts() {
        let storage = TestStorage::new(0.99);
        let executor = RealDataStageExecutor::new(storage);
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        executor
            .stage_mounts(alloc_id, &mounts, None)
            .await
            .unwrap();

        // Only the ReadWrite mount gets QoS
        let qos = executor.storage.qos_set.lock().unwrap();
        assert_eq!(qos.len(), 1);
        assert_eq!(qos[0].0, "/data/output");
        assert!((qos[0].1 - DEFAULT_QOS_FLOOR_GBPS).abs() < 0.001);
    }

    #[tokio::test]
    async fn real_skips_non_hot_tier_staging() {
        let storage = TestStorage::new(0.5);
        let executor = RealDataStageExecutor::new(storage);
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = vec![DataMount {
            source: "s3://archive/old".to_string(),
            target: "/data/cold".to_string(),
            access: DataAccess::ReadOnly,
            tier_hint: Some(StorageTier::Cold),
        }];

        let result = executor
            .stage_mounts(alloc_id, &mounts, None)
            .await
            .unwrap();

        // Cold tier mount is not staged regardless of readiness
        assert_eq!(result.mounts_staged, 0);
        assert_eq!(result.total_processed, 1);
    }

    #[tokio::test]
    async fn real_cleanup_wipes_all_targets() {
        let storage = TestStorage::new(0.0);
        let executor = RealDataStageExecutor::new(storage);
        let alloc_id = uuid::Uuid::new_v4();
        let mounts = sample_mounts();

        executor.cleanup_mounts(alloc_id, &mounts).await.unwrap();

        let wiped = executor.storage.wiped.lock().unwrap();
        assert_eq!(wiped.len(), 2);
        assert!(wiped.contains(&"/data/input".to_string()));
        assert!(wiped.contains(&"/data/output".to_string()));
    }

    #[tokio::test]
    async fn real_empty_mounts_returns_zero() {
        let storage = TestStorage::new(0.0);
        let executor = RealDataStageExecutor::new(storage);
        let alloc_id = uuid::Uuid::new_v4();

        let result = executor.stage_mounts(alloc_id, &[], None).await.unwrap();

        assert_eq!(result.mounts_staged, 0);
        assert_eq!(result.total_processed, 0);
        assert!(!result.scratch_created);
    }
}

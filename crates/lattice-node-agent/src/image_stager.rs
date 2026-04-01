//! Image staging for node-local caching and shared store integration.
//!
//! Implements the `ImageStager` trait from the software-delivery spec:
//! - Checks local NVMe cache for uenv images
//! - Checks Parallax store for OCI images (soft-fail if unavailable)
//! - Falls back to registry pull
//! - Provides synchronous readiness queries for the scheduler's f5 cost function

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use lattice_common::types::{ImageRef, ImageType};

/// Error type for image staging operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ImageStageError {
    #[error("image not found in registry: {spec}")]
    NotFound { spec: String },
    #[error("pull failed for {spec}: {reason}")]
    PullFailed { spec: String, reason: String },
    #[error("staging error for {spec}: {reason}")]
    StagingError { spec: String, reason: String },
}

/// Result of a successful image staging operation.
#[derive(Debug, Clone)]
pub struct StagedImage {
    /// Local path to the staged image.
    pub local_path: String,
    /// Content hash of the staged image.
    pub sha256: String,
    /// Whether the image was already in cache.
    pub from_cache: bool,
}

/// Trait for image staging (pull to local/shared store).
///
/// The scheduler uses `readiness()` synchronously to compute the f5 cost factor.
/// The node agent uses `is_cached()` and `stage()` during prologue.
#[async_trait]
pub trait ImageStager: Send + Sync {
    /// Check if an image is available locally (cache or shared store).
    async fn is_cached(&self, image: &ImageRef) -> bool;

    /// Pull an image to the shared store (single pull, INV-SD6).
    async fn stage(&self, image: &ImageRef) -> Result<StagedImage, ImageStageError>;

    /// Synchronous readiness score for the scheduler's f5 cost function.
    /// Returns 1.0 if cached, 0.5 if in registry but not cached, 0.0 if unresolved.
    fn readiness(&self, image: &ImageRef) -> f64;
}

/// Configuration for the local image stager.
#[derive(Debug, Clone)]
pub struct LocalImageStagerConfig {
    /// Directory for uenv SquashFS image cache (NVMe).
    pub uenv_cache_dir: String,
    /// Parallax shared image store path (for OCI images).
    pub parallax_imagestore: Option<String>,
    /// Parallax binary path.
    pub parallax_bin: Option<String>,
}

impl Default for LocalImageStagerConfig {
    fn default() -> Self {
        Self {
            uenv_cache_dir: "/var/cache/lattice/uenv".to_string(),
            parallax_imagestore: None,
            parallax_bin: None,
        }
    }
}

/// Local image stager that checks NVMe cache and Parallax store.
///
/// Maintains an in-memory cache of known-staged images for synchronous
/// readiness queries (the scheduler never blocks on network I/O).
pub struct LocalImageStager {
    config: LocalImageStagerConfig,
    /// Cache of known-staged image sha256 digests.
    staged_cache: Arc<RwLock<HashSet<String>>>,
}

impl LocalImageStager {
    /// Create a new local image stager with the given configuration.
    pub fn new(config: LocalImageStagerConfig) -> Self {
        Self {
            config,
            staged_cache: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Refresh the staged cache by scanning the cache directory.
    /// Called periodically by a background task.
    pub async fn refresh_cache(&self) {
        let mut new_cache = HashSet::new();

        // Scan uenv cache directory
        match tokio::fs::read_dir(&self.config.uenv_cache_dir).await {
            Ok(mut entries) => {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "squashfs") {
                        // Extract sha256 from filename (format: <sha256>.squashfs)
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            new_cache.insert(stem.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    dir = %self.config.uenv_cache_dir,
                    error = %e,
                    "failed to scan uenv cache directory"
                );
            }
        }

        let mut cache = self.staged_cache.write().await;
        *cache = new_cache;
    }

    /// Check if a uenv image is in the NVMe cache.
    async fn check_uenv_cache(&self, image: &ImageRef) -> bool {
        if image.sha256.is_empty() {
            return false;
        }
        let path = format!("{}/{}.squashfs", self.config.uenv_cache_dir, image.sha256);
        tokio::fs::metadata(&path).await.is_ok()
    }

    /// Check if an OCI image is in the Parallax shared store.
    /// Soft-fails: returns false if Parallax is not configured or unavailable.
    async fn check_parallax_store(&self, image: &ImageRef) -> bool {
        let (Some(ref _store), Some(ref parallax_bin)) =
            (&self.config.parallax_imagestore, &self.config.parallax_bin)
        else {
            // Parallax not configured — soft-fail
            return false;
        };

        // Check if parallax binary exists
        if !std::path::Path::new(parallax_bin).exists() {
            tracing::debug!(
                bin = %parallax_bin,
                "Parallax binary not found, cannot check store"
            );
            return false;
        }

        // Use `parallax --exists` to check (soft-fail on any error)
        match tokio::process::Command::new(parallax_bin)
            .args(["--exists", &image.spec])
            .status()
            .await
        {
            Ok(status) => status.success(),
            Err(e) => {
                tracing::warn!(
                    image = %image.spec,
                    error = %e,
                    "Parallax check failed, assuming not cached"
                );
                false
            }
        }
    }
}

#[async_trait]
impl ImageStager for LocalImageStager {
    async fn is_cached(&self, image: &ImageRef) -> bool {
        match image.image_type {
            ImageType::Uenv => self.check_uenv_cache(image).await,
            ImageType::Oci => self.check_parallax_store(image).await,
        }
    }

    async fn stage(&self, image: &ImageRef) -> Result<StagedImage, ImageStageError> {
        // Check cache first
        if self.is_cached(image).await {
            let local_path = match image.image_type {
                ImageType::Uenv => {
                    format!("{}/{}.squashfs", self.config.uenv_cache_dir, image.sha256)
                }
                ImageType::Oci => image.spec.clone(),
            };
            return Ok(StagedImage {
                local_path,
                sha256: image.sha256.clone(),
                from_cache: true,
            });
        }

        // For uenv: we would pull from registry to NVMe cache
        // For OCI: we would use parallax --migrate or podman pull
        // Both are delegated to the runtime's prepare() in practice.
        // Here we record the intent and update the staged cache.

        if image.sha256.is_empty() {
            return Err(ImageStageError::NotFound {
                spec: image.spec.clone(),
            });
        }

        // Mark as staged in cache (the actual pull happens in runtime.prepare())
        let mut cache = self.staged_cache.write().await;
        cache.insert(image.sha256.clone());

        let local_path = match image.image_type {
            ImageType::Uenv => {
                format!("{}/{}.squashfs", self.config.uenv_cache_dir, image.sha256)
            }
            ImageType::Oci => image.spec.clone(),
        };

        Ok(StagedImage {
            local_path,
            sha256: image.sha256.clone(),
            from_cache: false,
        })
    }

    fn readiness(&self, image: &ImageRef) -> f64 {
        if image.sha256.is_empty() {
            // Unresolved image — lowest readiness
            return 0.0;
        }

        // Synchronous check against staged_cache (no blocking I/O)
        // Use try_read to avoid blocking; if lock is contended, return 0.5 (conservative)
        match self.staged_cache.try_read() {
            Ok(cache) => {
                if cache.contains(&image.sha256) {
                    1.0
                } else {
                    0.5
                }
            }
            Err(_) => 0.5, // Lock contended, assume "in registry but not cached"
        }
    }
}

/// A no-op image stager for testing.
pub struct NoopImageStager;

#[async_trait]
impl ImageStager for NoopImageStager {
    async fn is_cached(&self, _image: &ImageRef) -> bool {
        false
    }

    async fn stage(&self, image: &ImageRef) -> Result<StagedImage, ImageStageError> {
        Ok(StagedImage {
            local_path: format!("/tmp/noop/{}", image.spec),
            sha256: image.sha256.clone(),
            from_cache: false,
        })
    }

    fn readiness(&self, _image: &ImageRef) -> f64 {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image(spec: &str, sha256: &str, image_type: ImageType) -> ImageRef {
        ImageRef {
            spec: spec.to_string(),
            image_type,
            sha256: sha256.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn readiness_unresolved_image_returns_zero() {
        let stager = LocalImageStager::new(LocalImageStagerConfig::default());
        let img = make_image("prgenv-gnu/24.11:v1", "", ImageType::Uenv);
        assert!((stager.readiness(&img) - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn readiness_uncached_image_returns_half() {
        let stager = LocalImageStager::new(LocalImageStagerConfig::default());
        let img = make_image("prgenv-gnu/24.11:v1", "abc123def456", ImageType::Uenv);
        assert!((stager.readiness(&img) - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn readiness_cached_image_returns_one() {
        let stager = LocalImageStager::new(LocalImageStagerConfig::default());
        // Manually insert into staged cache
        {
            let mut cache = stager.staged_cache.write().await;
            cache.insert("abc123def456".to_string());
        }
        let img = make_image("prgenv-gnu/24.11:v1", "abc123def456", ImageType::Uenv);
        assert!((stager.readiness(&img) - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn is_cached_returns_false_for_missing_uenv() {
        let config = LocalImageStagerConfig {
            uenv_cache_dir: "/nonexistent/cache".to_string(),
            ..Default::default()
        };
        let stager = LocalImageStager::new(config);
        let img = make_image("prgenv-gnu/24.11:v1", "abc123", ImageType::Uenv);
        assert!(!stager.is_cached(&img).await);
    }

    #[tokio::test]
    async fn is_cached_returns_false_for_empty_sha256() {
        let stager = LocalImageStager::new(LocalImageStagerConfig::default());
        let img = make_image("prgenv-gnu/24.11:v1", "", ImageType::Uenv);
        assert!(!stager.is_cached(&img).await);
    }

    #[tokio::test]
    async fn is_cached_returns_false_for_oci_without_parallax() {
        let config = LocalImageStagerConfig {
            parallax_imagestore: None,
            parallax_bin: None,
            ..Default::default()
        };
        let stager = LocalImageStager::new(config);
        let img = make_image("nvcr.io/nvidia/pytorch:24.01", "abc123", ImageType::Oci);
        assert!(!stager.is_cached(&img).await);
    }

    #[tokio::test]
    async fn stage_unresolved_image_fails() {
        let stager = LocalImageStager::new(LocalImageStagerConfig::default());
        let img = make_image("prgenv-gnu/24.11:v1", "", ImageType::Uenv);
        let result = stager.stage(&img).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn stage_records_in_cache() {
        let stager = LocalImageStager::new(LocalImageStagerConfig::default());
        let img = make_image("prgenv-gnu/24.11:v1", "abc123def456", ImageType::Uenv);

        let staged = stager.stage(&img).await.unwrap();
        assert!(!staged.from_cache);
        assert_eq!(staged.sha256, "abc123def456");

        // After staging, readiness should be 1.0
        assert!((stager.readiness(&img) - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn is_cached_uenv_with_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "deadbeef1234567890abcdef";
        let squashfs_path = dir.path().join(format!("{sha}.squashfs"));
        tokio::fs::write(&squashfs_path, b"fake").await.unwrap();

        let config = LocalImageStagerConfig {
            uenv_cache_dir: dir.path().to_str().unwrap().to_string(),
            ..Default::default()
        };
        let stager = LocalImageStager::new(config);
        let img = make_image("prgenv-gnu/24.11:v1", sha, ImageType::Uenv);
        assert!(stager.is_cached(&img).await);
    }

    #[tokio::test]
    async fn stage_cached_image_returns_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "deadbeef1234567890abcdef";
        let squashfs_path = dir.path().join(format!("{sha}.squashfs"));
        tokio::fs::write(&squashfs_path, b"fake").await.unwrap();

        let config = LocalImageStagerConfig {
            uenv_cache_dir: dir.path().to_str().unwrap().to_string(),
            ..Default::default()
        };
        let stager = LocalImageStager::new(config);
        let img = make_image("prgenv-gnu/24.11:v1", sha, ImageType::Uenv);

        let staged = stager.stage(&img).await.unwrap();
        assert!(staged.from_cache);
    }

    #[tokio::test]
    async fn refresh_cache_populates_staged_set() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "abc123xyz789";
        let squashfs_path = dir.path().join(format!("{sha}.squashfs"));
        tokio::fs::write(&squashfs_path, b"fake").await.unwrap();

        let config = LocalImageStagerConfig {
            uenv_cache_dir: dir.path().to_str().unwrap().to_string(),
            ..Default::default()
        };
        let stager = LocalImageStager::new(config);

        // Before refresh, readiness should be 0.5 (not in cache set)
        let img = make_image("test", sha, ImageType::Uenv);
        assert!((stager.readiness(&img) - 0.5).abs() < f64::EPSILON);

        // After refresh, readiness should be 1.0
        stager.refresh_cache().await;
        assert!((stager.readiness(&img) - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn noop_stager_readiness() {
        let stager = NoopImageStager;
        let img = make_image("test", "abc", ImageType::Uenv);
        assert!((stager.readiness(&img) - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn noop_stager_is_cached() {
        let stager = NoopImageStager;
        let img = make_image("test", "abc", ImageType::Uenv);
        assert!(!stager.is_cached(&img).await);
    }

    #[tokio::test]
    async fn noop_stager_stage() {
        let stager = NoopImageStager;
        let img = make_image("test", "abc", ImageType::Uenv);
        let staged = stager.stage(&img).await.unwrap();
        assert!(!staged.from_cache);
        assert!(staged.local_path.contains("test"));
    }

    #[tokio::test]
    async fn parallax_soft_fail_no_binary() {
        let config = LocalImageStagerConfig {
            parallax_imagestore: Some("/opt/parallax/store".to_string()),
            parallax_bin: Some("/nonexistent/parallax".to_string()),
            ..Default::default()
        };
        let stager = LocalImageStager::new(config);
        let img = make_image("nvcr.io/nvidia/pytorch:24.01", "abc123", ImageType::Oci);
        // Should soft-fail, returning false (not crash)
        assert!(!stager.is_cached(&img).await);
    }
}

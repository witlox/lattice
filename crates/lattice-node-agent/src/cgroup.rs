//! cgroup v2 resource isolation for allocations.
//!
//! Implements [`hpc_node::CgroupManager`] with two variants:
//! - [`LinuxCgroupManager`]: Real cgroup v2 filesystem operations (Linux only).
//! - [`StubCgroupManager`]: No-op for testing and non-Linux platforms.
//!
//! The lattice-node-agent creates a scope under `workload.slice/` for each
//! allocation, applies resource limits, and destroys the scope on epilogue.

use hpc_node::{CgroupError, CgroupHandle, CgroupManager, CgroupMetrics, ResourceLimits};

/// Stub cgroup manager for testing and non-Linux platforms.
///
/// All methods return `Ok` with sensible defaults. `create_scope` returns a
/// handle with a synthetic path; `read_metrics` returns zeroed metrics.
pub struct StubCgroupManager;

impl CgroupManager for StubCgroupManager {
    fn create_hierarchy(&self) -> Result<(), CgroupError> {
        Ok(())
    }

    fn create_scope(
        &self,
        parent_slice: &str,
        name: &str,
        _limits: &ResourceLimits,
    ) -> Result<CgroupHandle, CgroupError> {
        Ok(CgroupHandle {
            path: format!("/sys/fs/cgroup/{parent_slice}/{name}.scope"),
        })
    }

    fn destroy_scope(&self, _handle: &CgroupHandle) -> Result<(), CgroupError> {
        Ok(())
    }

    fn read_metrics(&self, _path: &str) -> Result<CgroupMetrics, CgroupError> {
        Ok(CgroupMetrics::default())
    }

    fn is_scope_empty(&self, _handle: &CgroupHandle) -> Result<bool, CgroupError> {
        Ok(true)
    }
}

/// Real cgroup v2 manager operating on `/sys/fs/cgroup`.
///
/// Creates scopes under the workload slice, writes resource limit files,
/// and uses `cgroup.kill` (Linux 5.14+) for scope teardown with a fallback
/// to `cgroup.procs` + SIGKILL on older kernels.
#[cfg(target_os = "linux")]
pub struct LinuxCgroupManager {
    /// Root path for the cgroup2 filesystem (typically `/sys/fs/cgroup`).
    cgroup_root: String,
}

#[cfg(target_os = "linux")]
impl LinuxCgroupManager {
    /// Create a new manager rooted at `/sys/fs/cgroup`.
    pub fn new() -> Self {
        Self {
            cgroup_root: "/sys/fs/cgroup".to_string(),
        }
    }

    /// Create a new manager with a custom cgroup root (for testing).
    #[cfg(test)]
    pub fn with_root(root: String) -> Self {
        Self { cgroup_root: root }
    }

    fn full_path(&self, relative: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.cgroup_root).join(relative)
    }
}

#[cfg(target_os = "linux")]
impl Default for LinuxCgroupManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl CgroupManager for LinuxCgroupManager {
    fn create_hierarchy(&self) -> Result<(), CgroupError> {
        let workload_path = self.full_path(hpc_node::cgroup::slices::WORKLOAD_ROOT);
        if !workload_path.exists() {
            std::fs::create_dir_all(&workload_path).map_err(|e| CgroupError::CreationFailed {
                reason: format!("failed to create {}: {e}", workload_path.display()),
            })?;
        }
        Ok(())
    }

    fn create_scope(
        &self,
        parent_slice: &str,
        name: &str,
        limits: &ResourceLimits,
    ) -> Result<CgroupHandle, CgroupError> {
        let relative = format!("{parent_slice}/{name}.scope");
        let scope_path = self.full_path(&relative);

        std::fs::create_dir_all(&scope_path).map_err(|e| CgroupError::CreationFailed {
            reason: format!("failed to create {}: {e}", scope_path.display()),
        })?;

        // Apply resource limits
        if let Some(memory_max) = limits.memory_max {
            let mem_file = scope_path.join("memory.max");
            std::fs::write(&mem_file, memory_max.to_string()).map_err(|e| {
                CgroupError::CreationFailed {
                    reason: format!("failed to write memory.max: {e}"),
                }
            })?;
        }

        if let Some(cpu_weight) = limits.cpu_weight {
            let cpu_file = scope_path.join("cpu.weight");
            std::fs::write(&cpu_file, cpu_weight.to_string()).map_err(|e| {
                CgroupError::CreationFailed {
                    reason: format!("failed to write cpu.weight: {e}"),
                }
            })?;
        }

        if let Some(io_max) = limits.io_max {
            let io_file = scope_path.join("io.max");
            std::fs::write(&io_file, io_max.to_string()).map_err(|e| {
                CgroupError::CreationFailed {
                    reason: format!("failed to write io.max: {e}"),
                }
            })?;
        }

        Ok(CgroupHandle {
            path: scope_path.to_string_lossy().into_owned(),
        })
    }

    fn destroy_scope(&self, handle: &CgroupHandle) -> Result<(), CgroupError> {
        let scope_path = std::path::Path::new(&handle.path);
        if !scope_path.exists() {
            return Ok(());
        }

        // Try cgroup.kill first (Linux 5.14+)
        let kill_file = scope_path.join("cgroup.kill");
        if kill_file.exists() {
            if let Err(e) = std::fs::write(&kill_file, "1") {
                tracing::warn!(
                    path = %handle.path,
                    error = %e,
                    "cgroup.kill write failed, falling back to SIGKILL"
                );
                self.kill_procs_fallback(scope_path)?;
            }
        } else {
            self.kill_procs_fallback(scope_path)?;
        }

        // Brief wait for processes to exit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check if scope is now empty
        let procs_file = scope_path.join("cgroup.procs");
        if procs_file.exists() {
            let content = std::fs::read_to_string(&procs_file).unwrap_or_default();
            if !content.trim().is_empty() {
                return Err(CgroupError::KillFailed {
                    path: handle.path.clone(),
                    reason: "processes remain after kill (possibly D-state)".to_string(),
                });
            }
        }

        // Remove the directory
        if scope_path.exists() {
            std::fs::remove_dir(scope_path).map_err(|e| CgroupError::KillFailed {
                path: handle.path.clone(),
                reason: format!("failed to remove scope directory: {e}"),
            })?;
        }

        Ok(())
    }

    fn read_metrics(&self, path: &str) -> Result<CgroupMetrics, CgroupError> {
        let cgroup_path = std::path::Path::new(path);
        if !cgroup_path.exists() {
            return Err(CgroupError::NotFound {
                path: path.to_string(),
            });
        }

        let memory_current = Self::read_u64_file(&cgroup_path.join("memory.current"))?;

        let memory_max_raw = Self::read_file_string(&cgroup_path.join("memory.max"))?;
        let memory_max = if memory_max_raw.trim() == "max" {
            None
        } else {
            memory_max_raw.trim().parse::<u64>().ok()
        };

        let cpu_usage_usec = Self::parse_cpu_stat(&cgroup_path.join("cpu.stat"))?;

        let nr_processes = Self::count_procs(&cgroup_path.join("cgroup.procs"))?;

        Ok(CgroupMetrics {
            memory_current,
            memory_max,
            cpu_usage_usec,
            nr_processes,
        })
    }

    fn is_scope_empty(&self, handle: &CgroupHandle) -> Result<bool, CgroupError> {
        let procs_file = std::path::Path::new(&handle.path).join("cgroup.procs");
        if !procs_file.exists() {
            return Ok(true);
        }
        let content = std::fs::read_to_string(&procs_file).map_err(CgroupError::Io)?;
        Ok(content.trim().is_empty())
    }
}

#[cfg(target_os = "linux")]
impl LinuxCgroupManager {
    /// Fallback kill: read PIDs from `cgroup.procs` and send SIGKILL.
    fn kill_procs_fallback(&self, scope_path: &std::path::Path) -> Result<(), CgroupError> {
        let procs_file = scope_path.join("cgroup.procs");
        if !procs_file.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&procs_file).map_err(CgroupError::Io)?;
        for line in content.lines() {
            if let Ok(pid) = line.trim().parse::<i32>() {
                // SAFETY: sending SIGKILL to a PID is safe; worst case the PID
                // no longer exists and kill returns ESRCH which we ignore.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
        Ok(())
    }

    fn read_u64_file(path: &std::path::Path) -> Result<u64, CgroupError> {
        if !path.exists() {
            return Ok(0);
        }
        let content = std::fs::read_to_string(path).map_err(CgroupError::Io)?;
        Ok(content.trim().parse::<u64>().unwrap_or(0))
    }

    fn read_file_string(path: &std::path::Path) -> Result<String, CgroupError> {
        if !path.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(path).map_err(CgroupError::Io)
    }

    fn parse_cpu_stat(path: &std::path::Path) -> Result<u64, CgroupError> {
        if !path.exists() {
            return Ok(0);
        }
        let content = std::fs::read_to_string(path).map_err(CgroupError::Io)?;
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("usage_usec ") {
                return Ok(val.trim().parse::<u64>().unwrap_or(0));
            }
        }
        Ok(0)
    }

    fn count_procs(path: &std::path::Path) -> Result<u32, CgroupError> {
        if !path.exists() {
            return Ok(0);
        }
        let content = std::fs::read_to_string(path).map_err(CgroupError::Io)?;
        Ok(content.lines().filter(|l| !l.trim().is_empty()).count() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpc_node::cgroup::slices;

    // ── StubCgroupManager tests (all platforms) ──

    #[test]
    fn stub_create_hierarchy_succeeds() {
        let mgr = StubCgroupManager;
        assert!(mgr.create_hierarchy().is_ok());
    }

    #[test]
    fn stub_create_scope_returns_handle() {
        let mgr = StubCgroupManager;
        let handle = mgr
            .create_scope(
                slices::WORKLOAD_ROOT,
                "alloc-42",
                &ResourceLimits::default(),
            )
            .unwrap();
        assert_eq!(handle.path, "/sys/fs/cgroup/workload.slice/alloc-42.scope");
    }

    #[test]
    fn stub_create_scope_with_limits() {
        let mgr = StubCgroupManager;
        let limits = ResourceLimits {
            memory_max: Some(1024 * 1024 * 1024),
            cpu_weight: Some(200),
            io_max: Some(50_000_000),
        };
        let handle = mgr
            .create_scope(slices::WORKLOAD_ROOT, "alloc-99", &limits)
            .unwrap();
        assert!(handle.path.contains("alloc-99"));
    }

    #[test]
    fn stub_destroy_scope_succeeds() {
        let mgr = StubCgroupManager;
        let handle = CgroupHandle {
            path: "/sys/fs/cgroup/workload.slice/alloc-42.scope".to_string(),
        };
        assert!(mgr.destroy_scope(&handle).is_ok());
    }

    #[test]
    fn stub_read_metrics_returns_zeros() {
        let mgr = StubCgroupManager;
        let metrics = mgr.read_metrics("/any/path").unwrap();
        assert_eq!(metrics.memory_current, 0);
        assert!(metrics.memory_max.is_none());
        assert_eq!(metrics.cpu_usage_usec, 0);
        assert_eq!(metrics.nr_processes, 0);
    }

    #[test]
    fn stub_is_scope_empty_returns_true() {
        let mgr = StubCgroupManager;
        let handle = CgroupHandle {
            path: "/sys/fs/cgroup/workload.slice/alloc-42.scope".to_string(),
        };
        assert!(mgr.is_scope_empty(&handle).unwrap());
    }

    #[test]
    fn stub_full_lifecycle() {
        let mgr = StubCgroupManager;
        mgr.create_hierarchy().unwrap();

        let handle = mgr
            .create_scope(
                slices::WORKLOAD_ROOT,
                "test-alloc",
                &ResourceLimits {
                    memory_max: Some(512 * 1024 * 1024),
                    cpu_weight: Some(100),
                    io_max: None,
                },
            )
            .unwrap();

        let metrics = mgr.read_metrics(&handle.path).unwrap();
        assert_eq!(metrics.nr_processes, 0);

        assert!(mgr.is_scope_empty(&handle).unwrap());
        mgr.destroy_scope(&handle).unwrap();
    }

    // ── LinuxCgroupManager tests (Linux only, tempdir-based) ──

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::*;

        fn make_manager() -> (tempfile::TempDir, LinuxCgroupManager) {
            let tmpdir = tempfile::tempdir().unwrap();
            let mgr = LinuxCgroupManager::with_root(tmpdir.path().to_string_lossy().into_owned());
            (tmpdir, mgr)
        }

        #[test]
        fn create_hierarchy_creates_workload_slice() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let workload = mgr.full_path(slices::WORKLOAD_ROOT);
            assert!(workload.exists());
        }

        #[test]
        fn create_hierarchy_idempotent() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            mgr.create_hierarchy().unwrap();
            let workload = mgr.full_path(slices::WORKLOAD_ROOT);
            assert!(workload.exists());
        }

        #[test]
        fn create_scope_creates_directory() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let handle = mgr
                .create_scope(
                    slices::WORKLOAD_ROOT,
                    "alloc-test",
                    &ResourceLimits::default(),
                )
                .unwrap();
            assert!(std::path::Path::new(&handle.path).exists());
        }

        #[test]
        fn create_scope_writes_memory_max() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let limits = ResourceLimits {
                memory_max: Some(1024 * 1024),
                cpu_weight: None,
                io_max: None,
            };
            let handle = mgr
                .create_scope(slices::WORKLOAD_ROOT, "mem-test", &limits)
                .unwrap();
            let content =
                std::fs::read_to_string(std::path::Path::new(&handle.path).join("memory.max"))
                    .unwrap();
            assert_eq!(content, "1048576");
        }

        #[test]
        fn create_scope_writes_cpu_weight() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let limits = ResourceLimits {
                memory_max: None,
                cpu_weight: Some(500),
                io_max: None,
            };
            let handle = mgr
                .create_scope(slices::WORKLOAD_ROOT, "cpu-test", &limits)
                .unwrap();
            let content =
                std::fs::read_to_string(std::path::Path::new(&handle.path).join("cpu.weight"))
                    .unwrap();
            assert_eq!(content, "500");
        }

        #[test]
        fn create_scope_writes_io_max() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let limits = ResourceLimits {
                memory_max: None,
                cpu_weight: None,
                io_max: Some(100_000_000),
            };
            let handle = mgr
                .create_scope(slices::WORKLOAD_ROOT, "io-test", &limits)
                .unwrap();
            let content =
                std::fs::read_to_string(std::path::Path::new(&handle.path).join("io.max")).unwrap();
            assert_eq!(content, "100000000");
        }

        #[test]
        fn create_scope_writes_all_limits() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let limits = ResourceLimits {
                memory_max: Some(2 * 1024 * 1024 * 1024),
                cpu_weight: Some(300),
                io_max: Some(50_000_000),
            };
            let handle = mgr
                .create_scope(slices::WORKLOAD_ROOT, "full-test", &limits)
                .unwrap();
            let scope = std::path::Path::new(&handle.path);
            assert!(scope.join("memory.max").exists());
            assert!(scope.join("cpu.weight").exists());
            assert!(scope.join("io.max").exists());
        }

        #[test]
        fn destroy_scope_removes_empty_dir() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let handle = mgr
                .create_scope(
                    slices::WORKLOAD_ROOT,
                    "destroy-test",
                    &ResourceLimits::default(),
                )
                .unwrap();

            // Remove all files first (destroy expects only the dir)
            for entry in std::fs::read_dir(&handle.path).unwrap() {
                let entry = entry.unwrap();
                std::fs::remove_file(entry.path()).unwrap();
            }

            mgr.destroy_scope(&handle).unwrap();
            assert!(!std::path::Path::new(&handle.path).exists());
        }

        #[test]
        fn destroy_scope_nonexistent_ok() {
            let (_tmpdir, mgr) = make_manager();
            let handle = CgroupHandle {
                path: "/nonexistent/path".to_string(),
            };
            assert!(mgr.destroy_scope(&handle).is_ok());
        }

        #[test]
        fn read_metrics_from_fake_cgroup() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let handle = mgr
                .create_scope(
                    slices::WORKLOAD_ROOT,
                    "metrics-test",
                    &ResourceLimits::default(),
                )
                .unwrap();

            let scope = std::path::Path::new(&handle.path);

            // Write fake cgroup files
            std::fs::write(scope.join("memory.current"), "4096").unwrap();
            std::fs::write(scope.join("memory.max"), "1048576").unwrap();
            std::fs::write(
                scope.join("cpu.stat"),
                "usage_usec 123456\nuser_usec 100000\nsystem_usec 23456\n",
            )
            .unwrap();
            std::fs::write(scope.join("cgroup.procs"), "1234\n5678\n").unwrap();

            let metrics = mgr.read_metrics(&handle.path).unwrap();
            assert_eq!(metrics.memory_current, 4096);
            assert_eq!(metrics.memory_max, Some(1048576));
            assert_eq!(metrics.cpu_usage_usec, 123456);
            assert_eq!(metrics.nr_processes, 2);
        }

        #[test]
        fn read_metrics_memory_max_unlimited() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let handle = mgr
                .create_scope(
                    slices::WORKLOAD_ROOT,
                    "max-test",
                    &ResourceLimits::default(),
                )
                .unwrap();

            let scope = std::path::Path::new(&handle.path);
            std::fs::write(scope.join("memory.current"), "0").unwrap();
            std::fs::write(scope.join("memory.max"), "max").unwrap();

            let metrics = mgr.read_metrics(&handle.path).unwrap();
            assert!(metrics.memory_max.is_none());
        }

        #[test]
        fn read_metrics_not_found() {
            let (_tmpdir, mgr) = make_manager();
            let result = mgr.read_metrics("/nonexistent/cgroup");
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), CgroupError::NotFound { .. }));
        }

        #[test]
        fn is_scope_empty_with_procs() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let handle = mgr
                .create_scope(
                    slices::WORKLOAD_ROOT,
                    "empty-test",
                    &ResourceLimits::default(),
                )
                .unwrap();

            let scope = std::path::Path::new(&handle.path);

            // No cgroup.procs file yet → empty
            assert!(mgr.is_scope_empty(&handle).unwrap());

            // Empty cgroup.procs → empty
            std::fs::write(scope.join("cgroup.procs"), "").unwrap();
            assert!(mgr.is_scope_empty(&handle).unwrap());

            // With PIDs → not empty
            std::fs::write(scope.join("cgroup.procs"), "1234\n").unwrap();
            assert!(!mgr.is_scope_empty(&handle).unwrap());
        }

        #[test]
        fn is_scope_empty_nonexistent_path() {
            let (_tmpdir, mgr) = make_manager();
            let handle = CgroupHandle {
                path: "/nonexistent/scope".to_string(),
            };
            // No cgroup.procs file → considered empty
            assert!(mgr.is_scope_empty(&handle).unwrap());
        }

        #[test]
        fn read_metrics_missing_files_returns_zeros() {
            let (_tmpdir, mgr) = make_manager();
            mgr.create_hierarchy().unwrap();
            let handle = mgr
                .create_scope(
                    slices::WORKLOAD_ROOT,
                    "sparse-test",
                    &ResourceLimits::default(),
                )
                .unwrap();

            // Scope dir exists but no metric files written
            let metrics = mgr.read_metrics(&handle.path).unwrap();
            assert_eq!(metrics.memory_current, 0);
            assert_eq!(metrics.cpu_usage_usec, 0);
            assert_eq!(metrics.nr_processes, 0);
        }
    }
}

//! Process reattach — recover running workloads after agent restart.
//!
//! On startup the agent loads the persisted [`AgentState`] and checks
//! whether each workload's PID is still alive. Living processes are
//! recovered into the agent's allocation tracking; dead ones are
//! flagged as orphans so their cgroups and mount points can be cleaned up.

use tracing::{info, warn};

use crate::state::{AgentState, PersistedAllocation};

/// Result of attempting to reattach to previously running allocations.
#[derive(Debug, Default)]
pub struct ReattachResult {
    /// Allocations whose PID is still alive — the agent resumes tracking them.
    pub recovered: Vec<RecoveredAllocation>,
    /// Allocations whose PID is dead — need cgroup/mount cleanup.
    pub orphans: Vec<PersistedAllocation>,
}

/// A single allocation that was successfully reattached.
#[derive(Debug)]
pub struct RecoveredAllocation {
    pub persisted: PersistedAllocation,
    pub process_alive: bool,
}

/// Check whether a PID is still alive.
///
/// Uses `kill(pid, 0)` which is portable across Linux and macOS.
/// Returns `false` for `None` PIDs.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) succeeds if the process exists and we have permission,
        // or fails with EPERM if the process exists but we lack permission
        // (still alive). Only ESRCH means the process is gone.
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        // EPERM means "exists but we can't signal it" — still alive.
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        errno == libc::EPERM
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Attempt to reattach to previously running allocations from persisted state.
///
/// For each allocation:
/// - If PID is present and alive → recovered
/// - If PID is absent or dead → orphan (needs cleanup)
pub fn reattach(state: &AgentState) -> ReattachResult {
    let mut result = ReattachResult::default();

    for alloc in &state.allocations {
        let alive = alloc.pid.is_some_and(is_process_alive);

        if alive {
            info!(
                alloc_id = %alloc.id,
                pid = ?alloc.pid,
                "reattached to running workload"
            );
            result.recovered.push(RecoveredAllocation {
                persisted: alloc.clone(),
                process_alive: true,
            });
        } else {
            warn!(
                alloc_id = %alloc.id,
                pid = ?alloc.pid,
                cgroup = ?alloc.cgroup_path,
                "workload process dead — marking as orphan for cleanup"
            );
            result.orphans.push(alloc.clone());
        }
    }

    result
}

/// Clean up orphaned cgroup scopes left behind by dead workloads.
///
/// Attempts to remove the cgroup directory. On non-Linux or if the path
/// doesn't exist this is a no-op.
pub fn cleanup_orphan_cgroup(cgroup_path: &str) {
    let path = std::path::Path::new(cgroup_path);
    if path.exists() {
        match std::fs::remove_dir(path) {
            Ok(()) => info!(path = %cgroup_path, "removed orphan cgroup"),
            Err(e) => warn!(path = %cgroup_path, error = %e, "failed to remove orphan cgroup"),
        }
    }
}

/// Scan for cgroup scopes under `/sys/fs/cgroup/workload.slice/` that
/// have no matching persisted allocation. These may be left over from
/// a previous agent crash.
///
/// Returns the paths of orphaned cgroup scopes. On non-Linux this
/// returns an empty vec.
pub fn scan_orphan_cgroups(known_ids: &[uuid::Uuid]) -> Vec<String> {
    let base = std::path::Path::new("/sys/fs/cgroup/workload.slice");
    if !base.exists() {
        return vec![];
    }

    let mut orphans = vec![];
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("alloc-") && name.ends_with(".scope") {
                // Extract the UUID from "alloc-{uuid}.scope"
                let id_str = name
                    .strip_prefix("alloc-")
                    .and_then(|s| s.strip_suffix(".scope"))
                    .unwrap_or("");
                let is_known = uuid::Uuid::parse_str(id_str)
                    .map(|id| known_ids.contains(&id))
                    .unwrap_or(false);
                if !is_known {
                    orphans.push(entry.path().to_string_lossy().to_string());
                }
            }
        }
    }

    orphans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentState, PersistedAllocation, RuntimeType};
    use chrono::Utc;

    fn make_alloc(pid: Option<u32>) -> PersistedAllocation {
        PersistedAllocation {
            id: uuid::Uuid::new_v4(),
            pid,
            container_id: None,
            cgroup_path: None,
            entrypoint: "test".to_string(),
            started_at: Utc::now(),
            runtime_type: RuntimeType::Uenv,
            mount_point: None,
        }
    }

    #[test]
    fn is_process_alive_current_process() {
        // Our own PID should be alive.
        let pid = std::process::id();
        assert!(is_process_alive(pid));
    }

    #[test]
    fn is_process_alive_nonexistent() {
        // PID 4_000_000_000 almost certainly doesn't exist.
        assert!(!is_process_alive(4_000_000_000));
    }

    #[test]
    fn reattach_with_alive_pid() {
        let pid = std::process::id();
        let alloc = PersistedAllocation {
            pid: Some(pid),
            ..make_alloc(None)
        };
        let state = AgentState {
            node_id: "n0".to_string(),
            updated_at: Utc::now(),
            allocations: vec![alloc.clone()],
        };

        let result = reattach(&state);
        assert_eq!(result.recovered.len(), 1);
        assert!(result.orphans.is_empty());
        assert!(result.recovered[0].process_alive);
        assert_eq!(result.recovered[0].persisted.id, alloc.id);
    }

    #[test]
    fn reattach_with_dead_pid() {
        let alloc = make_alloc(Some(4_000_000_000));
        let state = AgentState {
            node_id: "n0".to_string(),
            updated_at: Utc::now(),
            allocations: vec![alloc.clone()],
        };

        let result = reattach(&state);
        assert!(result.recovered.is_empty());
        assert_eq!(result.orphans.len(), 1);
        assert_eq!(result.orphans[0].id, alloc.id);
    }

    #[test]
    fn reattach_with_no_pid() {
        let alloc = make_alloc(None);
        let state = AgentState {
            node_id: "n0".to_string(),
            updated_at: Utc::now(),
            allocations: vec![alloc.clone()],
        };

        let result = reattach(&state);
        assert!(result.recovered.is_empty());
        assert_eq!(result.orphans.len(), 1);
    }

    #[test]
    fn reattach_mixed() {
        let alive = PersistedAllocation {
            pid: Some(std::process::id()),
            ..make_alloc(None)
        };
        let dead = make_alloc(Some(4_000_000_000));

        let state = AgentState {
            node_id: "n0".to_string(),
            updated_at: Utc::now(),
            allocations: vec![alive.clone(), dead.clone()],
        };

        let result = reattach(&state);
        assert_eq!(result.recovered.len(), 1);
        assert_eq!(result.orphans.len(), 1);
        assert_eq!(result.recovered[0].persisted.id, alive.id);
        assert_eq!(result.orphans[0].id, dead.id);
    }

    #[test]
    fn reattach_empty_state() {
        let state = AgentState {
            node_id: "n0".to_string(),
            updated_at: Utc::now(),
            allocations: vec![],
        };

        let result = reattach(&state);
        assert!(result.recovered.is_empty());
        assert!(result.orphans.is_empty());
    }

    #[test]
    fn scan_orphan_cgroups_nonexistent_base() {
        // On macOS (or CI without cgroups), this should return empty.
        let result = scan_orphan_cgroups(&[]);
        // Can't assert much — on Linux with real cgroups this might find things.
        // Just verify it doesn't panic.
        let _ = result;
    }

    #[test]
    fn cleanup_orphan_cgroup_nonexistent_is_noop() {
        // Should not panic on a path that doesn't exist.
        cleanup_orphan_cgroup("/sys/fs/cgroup/workload.slice/alloc-nonexistent.scope");
    }
}

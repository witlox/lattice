//! Agent state persistence — survives agent restarts.
//!
//! Writes a JSON state file atomically (write to `.tmp`, rename) so that
//! a crash mid-write never corrupts the on-disk state. On startup the
//! agent loads the state file and attempts to reattach to running workloads.

use std::fs;
use std::io;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use lattice_common::types::AllocId;

/// Default path for the agent state file.
pub const STATE_FILE_PATH: &str = "/var/lib/lattice/agent-state.json";

/// Snapshot of all allocations the agent was managing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentState {
    /// Node identifier (xname).
    pub node_id: String,
    /// When this snapshot was written.
    pub updated_at: DateTime<Utc>,
    /// Active allocations at time of write.
    pub allocations: Vec<PersistedAllocation>,
}

/// A single allocation's state on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersistedAllocation {
    pub id: AllocId,
    pub pid: Option<u32>,
    pub container_id: Option<String>,
    pub cgroup_path: Option<String>,
    pub entrypoint: String,
    pub started_at: DateTime<Utc>,
    pub runtime_type: RuntimeType,
    pub mount_point: Option<String>,
}

/// Which runtime was used to launch the workload.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuntimeType {
    Uenv,
    Sarus,
    Dmtcp,
}

/// Persist `state` to `path` atomically.
///
/// Writes to `path.tmp` first, then renames. This ensures that a crash
/// during the write never leaves a truncated state file on disk.
pub fn save_state(path: &Path, state: &AgentState) -> io::Result<()> {
    let tmp_path = path.with_extension("json.tmp");

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    fs::write(&tmp_path, json)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load a previously saved state file. Returns `Ok(None)` when the file
/// does not exist (first boot) and `Err` on parse/IO failures.
pub fn load_state(path: &Path) -> io::Result<Option<AgentState>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let state: AgentState = serde_json::from_str(&contents)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(state))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Remove the state file (called when agent shuts down with no active
/// allocations).
pub fn remove_state(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_state() -> AgentState {
        AgentState {
            node_id: "nid001234".to_string(),
            updated_at: Utc::now(),
            allocations: vec![
                PersistedAllocation {
                    id: uuid::Uuid::new_v4(),
                    pid: Some(42000),
                    container_id: None,
                    cgroup_path: Some("/sys/fs/cgroup/workload.slice/alloc-abc.scope".to_string()),
                    entrypoint: "python train.py".to_string(),
                    started_at: Utc::now(),
                    runtime_type: RuntimeType::Uenv,
                    mount_point: Some("/mnt/uenv/abc".to_string()),
                },
                PersistedAllocation {
                    id: uuid::Uuid::new_v4(),
                    pid: Some(42001),
                    container_id: Some("sarus-xyz".to_string()),
                    cgroup_path: None,
                    entrypoint: "bash run.sh".to_string(),
                    started_at: Utc::now(),
                    runtime_type: RuntimeType::Sarus,
                    mount_point: None,
                },
            ],
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent-state.json");
        let state = sample_state();

        save_state(&path, &state).unwrap();
        let loaded = load_state(&path).unwrap().expect("state should exist");

        assert_eq!(loaded.node_id, state.node_id);
        assert_eq!(loaded.allocations.len(), 2);
        assert_eq!(loaded.allocations[0].id, state.allocations[0].id);
        assert_eq!(loaded.allocations[1].runtime_type, RuntimeType::Sarus);
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let result = load_state(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_corrupt_file_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent-state.json");
        fs::write(&path, "not valid json {{{").unwrap();

        let result = load_state(&path);
        assert!(result.is_err());
    }

    #[test]
    fn atomic_write_no_tmp_left_behind() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent-state.json");
        let state = sample_state();

        save_state(&path, &state).unwrap();

        // The .tmp file should not exist after a successful save.
        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dir").join("state.json");
        let state = sample_state();

        save_state(&path, &state).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn remove_state_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent-state.json");
        let state = sample_state();

        save_state(&path, &state).unwrap();
        assert!(path.exists());

        remove_state(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn remove_nonexistent_is_ok() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.json");
        remove_state(&path).unwrap(); // should not error
    }

    #[test]
    fn empty_allocations_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent-state.json");
        let state = AgentState {
            node_id: "node-0".to_string(),
            updated_at: Utc::now(),
            allocations: vec![],
        };

        save_state(&path, &state).unwrap();
        let loaded = load_state(&path).unwrap().unwrap();
        assert!(loaded.allocations.is_empty());
    }

    #[test]
    fn overwrite_existing_state() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent-state.json");

        let state1 = sample_state();
        save_state(&path, &state1).unwrap();

        let state2 = AgentState {
            node_id: "different-node".to_string(),
            updated_at: Utc::now(),
            allocations: vec![],
        };
        save_state(&path, &state2).unwrap();

        let loaded = load_state(&path).unwrap().unwrap();
        assert_eq!(loaded.node_id, "different-node");
        assert!(loaded.allocations.is_empty());
    }
}

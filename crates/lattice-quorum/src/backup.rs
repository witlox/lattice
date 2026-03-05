//! Backup export, verify, and restore for Raft state.
//!
//! Backup format: tar.gz containing:
//! ```text
//! backup-{timestamp}/
//!   metadata.json     — term, index, timestamp, node_id
//!   snapshot.json     — GlobalState serialized
//! ```

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::debug;

use crate::global_state::GlobalState;

/// Metadata about a backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMetadata {
    pub timestamp: DateTime<Utc>,
    pub snapshot_term: u64,
    pub snapshot_index: u64,
    pub node_count: usize,
    pub allocation_count: usize,
    pub tenant_count: usize,
    pub audit_entry_count: usize,
}

/// Export the current state to a tar.gz backup at the given path.
pub async fn export_backup(
    state: &Arc<RwLock<GlobalState>>,
    path: &Path,
) -> io::Result<BackupMetadata> {
    let state_guard = state.read().await;
    let state_json = serde_json::to_vec_pretty(&*state_guard).map_err(io::Error::other)?;

    let metadata = BackupMetadata {
        timestamp: Utc::now(),
        snapshot_term: 0,
        snapshot_index: 0,
        node_count: state_guard.nodes.len(),
        allocation_count: state_guard.allocations.len(),
        tenant_count: state_guard.tenants.len(),
        audit_entry_count: state_guard.audit_log.len(),
    };
    let metadata_json = serde_json::to_vec_pretty(&metadata).map_err(io::Error::other)?;

    drop(state_guard);

    let prefix = format!("backup-{}", metadata.timestamp.format("%Y%m%dT%H%M%SZ"));

    // Create tar.gz
    let file = std::fs::File::create(path)?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);

    // Add metadata.json
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata_json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(
        &mut header,
        format!("{prefix}/metadata.json"),
        metadata_json.as_slice(),
    )?;

    // Add snapshot.json
    let mut header = tar::Header::new_gnu();
    header.set_size(state_json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(
        &mut header,
        format!("{prefix}/snapshot.json"),
        state_json.as_slice(),
    )?;

    tar.into_inner()?.finish()?;

    debug!("Exported backup to {}", path.display());
    Ok(metadata)
}

/// Verify a backup file's integrity and return its metadata.
pub fn verify_backup(path: &Path) -> io::Result<BackupMetadata> {
    let file = std::fs::File::open(path)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    let mut found_metadata = false;
    let mut found_snapshot = false;
    let mut metadata: Option<BackupMetadata> = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        match name.as_str() {
            "metadata.json" => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                metadata = Some(
                    serde_json::from_slice(&buf)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                );
                found_metadata = true;
            }
            "snapshot.json" => {
                // Validate it's parseable as GlobalState
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                let _state: GlobalState = serde_json::from_slice(&buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                found_snapshot = true;
            }
            _ => {}
        }
    }

    if !found_metadata {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "backup missing metadata.json",
        ));
    }
    if !found_snapshot {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "backup missing snapshot.json",
        ));
    }

    Ok(metadata.unwrap())
}

/// Restore a backup into the given data_dir for Raft to load on restart.
///
/// Extracts the snapshot from the backup and places it in the snapshots
/// directory so the state machine will load it on next startup.
pub fn restore_backup(backup_path: &Path, data_dir: &Path) -> io::Result<BackupMetadata> {
    // First verify the backup
    let metadata = verify_backup(backup_path)?;

    let snapshot_dir = data_dir.join("raft").join("snapshots");
    std::fs::create_dir_all(&snapshot_dir)?;

    // Extract the snapshot data
    let file = std::fs::File::open(backup_path)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if name == "snapshot.json" {
            let mut state_data = Vec::new();
            entry.read_to_end(&mut state_data)?;

            let global_state: GlobalState = serde_json::from_slice(&state_data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            // Create a snapshot file in the format expected by the state machine
            let snap_filename = format!(
                "snap-{}-{}.json",
                metadata.snapshot_term, metadata.snapshot_index
            );
            let snap_path = snapshot_dir.join(&snap_filename);

            let persisted = serde_json::json!({
                "meta": {
                    "last_log_id": null,
                    "last_membership": openraft::StoredMembership::<crate::TypeConfig>::default(),
                    "snapshot_id": format!("restored-{}", metadata.timestamp.format("%Y%m%dT%H%M%SZ")),
                },
                "state": global_state,
            });

            let json = serde_json::to_vec_pretty(&persisted).map_err(io::Error::other)?;
            std::fs::write(&snap_path, &json)?;

            // Update "current" pointer
            let current = snapshot_dir.join("current");
            std::fs::write(&current, snap_filename.as_bytes())?;

            debug!("Restored backup to {}", snap_path.display());
            break;
        }
    }

    // Clean up WAL since we're restoring from a snapshot
    let wal_dir = data_dir.join("raft").join("wal");
    if wal_dir.exists() {
        for entry in std::fs::read_dir(&wal_dir)? {
            let entry = entry?;
            let _ = std::fs::remove_file(entry.path());
        }
    }

    // Clean up vote/committed files
    let vote_path = data_dir.join("raft").join("vote.json");
    let committed_path = data_dir.join("raft").join("committed.json");
    let _ = std::fs::remove_file(&vote_path);
    let _ = std::fs::remove_file(&committed_path);

    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> Arc<RwLock<GlobalState>> {
        let mut state = GlobalState::new();
        // Add some test data
        use lattice_test_harness::fixtures::{AllocationBuilder, NodeBuilder, TenantBuilder};

        let node = NodeBuilder::new().id("test-node-1").build();
        state.nodes.insert(node.id.clone(), node);

        let tenant = TenantBuilder::new("test-tenant").build();
        state.tenants.insert(tenant.id.clone(), tenant);

        let alloc = AllocationBuilder::new().tenant("test-tenant").build();
        state.allocations.insert(alloc.id, alloc);

        Arc::new(RwLock::new(state))
    }

    #[tokio::test]
    async fn export_and_verify_roundtrip() {
        let state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let backup_path = dir.path().join("test-backup.tar.gz");

        let export_meta = export_backup(&state, &backup_path).await.unwrap();
        assert_eq!(export_meta.node_count, 1);
        assert_eq!(export_meta.tenant_count, 1);
        assert_eq!(export_meta.allocation_count, 1);

        let verify_meta = verify_backup(&backup_path).unwrap();
        assert_eq!(verify_meta.node_count, 1);
        assert_eq!(verify_meta.tenant_count, 1);
        assert_eq!(verify_meta.allocation_count, 1);
    }

    #[tokio::test]
    async fn verify_corrupt_backup_fails() {
        let dir = tempfile::tempdir().unwrap();
        let backup_path = dir.path().join("corrupt.tar.gz");
        std::fs::write(&backup_path, b"not a valid tar.gz").unwrap();

        let result = verify_backup(&backup_path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn verify_missing_snapshot_fails() {
        let dir = tempfile::tempdir().unwrap();
        let backup_path = dir.path().join("incomplete.tar.gz");

        // Create a tar.gz with only metadata
        let file = std::fs::File::create(&backup_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        let metadata = BackupMetadata {
            timestamp: Utc::now(),
            snapshot_term: 0,
            snapshot_index: 0,
            node_count: 0,
            allocation_count: 0,
            tenant_count: 0,
            audit_entry_count: 0,
        };
        let metadata_json = serde_json::to_vec(&metadata).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(metadata_json.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(
                &mut header,
                "backup/metadata.json",
                metadata_json.as_slice(),
            )
            .unwrap();

        tar_builder.into_inner().unwrap().finish().unwrap();

        let result = verify_backup(&backup_path);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing snapshot.json"));
    }

    #[tokio::test]
    async fn restore_writes_snapshot_files() {
        let state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let backup_path = dir.path().join("backup.tar.gz");
        let data_dir = dir.path().join("restored");

        export_backup(&state, &backup_path).await.unwrap();
        let meta = restore_backup(&backup_path, &data_dir).unwrap();

        assert_eq!(meta.node_count, 1);

        // Verify snapshot file was created
        let snap_dir = data_dir.join("raft").join("snapshots");
        assert!(snap_dir.exists());
        assert!(snap_dir.join("current").exists());

        // Verify the current pointer points to a valid file
        let current = std::fs::read_to_string(snap_dir.join("current")).unwrap();
        assert!(snap_dir.join(current.trim()).exists());
    }

    #[test]
    fn verify_nonexistent_backup_fails() {
        let result = verify_backup(Path::new("/nonexistent/backup.tar.gz"));
        assert!(result.is_err());
    }
}

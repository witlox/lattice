//! Backup export, verify, and restore for Raft state.
//!
//! Delegates to `raft_hpc_core::backup` for the actual tar.gz operations,
//! using Lattice-specific backup metadata (node/allocation/tenant counts).

use std::io;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::global_state::{GlobalState, LatticeBackupMeta};
use crate::TypeConfig;

/// Backup metadata type for Lattice (wraps the generic raft-hpc-core metadata).
pub type BackupMetadata = raft_hpc_core::BackupMetadata<LatticeBackupMeta>;

/// Export the current state to a tar.gz backup at the given path.
pub async fn export_backup(
    state: &Arc<RwLock<GlobalState>>,
    path: &Path,
) -> io::Result<BackupMetadata> {
    raft_hpc_core::export_backup::<GlobalState>(state, path).await
}

/// Verify a backup file's integrity and return its metadata.
pub fn verify_backup(path: &Path) -> io::Result<BackupMetadata> {
    raft_hpc_core::verify_backup::<GlobalState, LatticeBackupMeta>(path)
}

/// Restore a backup into the given data_dir for Raft to load on restart.
pub fn restore_backup(backup_path: &Path, data_dir: &Path) -> io::Result<BackupMetadata> {
    raft_hpc_core::restore_backup::<TypeConfig, GlobalState, LatticeBackupMeta>(
        backup_path,
        data_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> Arc<RwLock<GlobalState>> {
        let mut state = GlobalState::new();
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
        assert_eq!(export_meta.app.node_count, 1);
        assert_eq!(export_meta.app.tenant_count, 1);
        assert_eq!(export_meta.app.allocation_count, 1);

        let verify_meta = verify_backup(&backup_path).unwrap();
        assert_eq!(verify_meta.app.node_count, 1);
        assert_eq!(verify_meta.app.tenant_count, 1);
        assert_eq!(verify_meta.app.allocation_count, 1);
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
            timestamp: chrono::Utc::now(),
            snapshot_term: 0,
            snapshot_index: 0,
            app: LatticeBackupMeta {
                node_count: 0,
                allocation_count: 0,
                tenant_count: 0,
                audit_entry_count: 0,
                audit_archive_count: 0,
                total_audit_entries: 0,
            },
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

        assert_eq!(meta.app.node_count, 1);

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

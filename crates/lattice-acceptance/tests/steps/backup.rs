use std::sync::Arc;

use cucumber::{given, then, when};
use tokio::sync::RwLock;

use crate::LatticeWorld;
use lattice_quorum::backup::{export_backup, restore_backup, verify_backup};
use lattice_quorum::global_state::GlobalState;
use lattice_test_harness::fixtures::*;

// ─── Helpers ────────────────────────────────────────────────

fn build_test_state(node_count: usize, alloc_count: usize) -> GlobalState {
    let mut state = GlobalState::new();

    for i in 0..node_count {
        let node = NodeBuilder::new().id(&format!("backup-node-{i}")).build();
        state.nodes.insert(node.id.clone(), node);
    }

    let tenant = TenantBuilder::new("backup-tenant").build();
    state.tenants.insert(tenant.id.clone(), tenant);

    for _ in 0..alloc_count {
        let alloc = AllocationBuilder::new().tenant("backup-tenant").build();
        state.allocations.insert(alloc.id, alloc);
    }

    state
}

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^a quorum with (\d+) nodes and (\d+) allocations$"#)]
fn given_quorum_with_state(world: &mut LatticeWorld, node_count: usize, alloc_count: usize) {
    let state = build_test_state(node_count, alloc_count);
    let tempdir = tempfile::tempdir().expect("failed to create tempdir");
    world.backup_state = Some(Arc::new(RwLock::new(state)));
    world._backup_tempdir = Some(tempdir);
}

#[given("I export a backup")]
fn given_export_backup(world: &mut LatticeWorld) {
    let state = world.backup_state.as_ref().expect("no quorum state set up");
    let tempdir = world._backup_tempdir.as_ref().expect("no tempdir");
    let backup_path = tempdir.path().join("test-backup.tar.gz");

    let metadata = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(export_backup(state, &backup_path))
    })
    .expect("export_backup failed");

    world.backup_path = Some(backup_path);
    world.backup_metadata = Some(metadata);
}

#[given("a corrupt backup file")]
fn given_corrupt_backup(world: &mut LatticeWorld) {
    let tempdir = tempfile::tempdir().expect("failed to create tempdir");
    let backup_path = tempdir.path().join("corrupt-backup.tar.gz");
    std::fs::write(&backup_path, b"not a valid tar.gz file").expect("failed to write corrupt file");
    world.backup_path = Some(backup_path);
    world._backup_tempdir = Some(tempdir);
}

#[given("a data directory with existing WAL files")]
fn given_data_dir_with_wal(world: &mut LatticeWorld) {
    let tempdir = world._backup_tempdir.as_ref().expect("no tempdir");
    let data_dir = tempdir.path().join("restore-with-wal");
    let wal_dir = data_dir.join("raft").join("wal");
    std::fs::create_dir_all(&wal_dir).expect("failed to create WAL dir");
    std::fs::write(wal_dir.join("wal-00001.log"), b"fake wal data").expect("failed to write WAL");
    world.restore_data_dir = Some(data_dir);
}

// ─── When Steps ────────────────────────────────────────────

#[when("I export a backup")]
fn when_export_backup(world: &mut LatticeWorld) {
    let state = world.backup_state.as_ref().expect("no quorum state set up");
    let tempdir = world._backup_tempdir.as_ref().expect("no tempdir");
    let backup_path = tempdir.path().join("test-backup.tar.gz");

    let metadata = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(export_backup(state, &backup_path))
    })
    .expect("export_backup failed");

    world.backup_path = Some(backup_path);
    world.backup_metadata = Some(metadata);
}

#[when("I verify the backup")]
fn when_verify_backup(world: &mut LatticeWorld) {
    let backup_path = world.backup_path.as_ref().expect("no backup path set");
    match verify_backup(backup_path) {
        Ok(meta) => {
            world.verified_metadata = Some(meta);
            world.backup_error = None;
        }
        Err(e) => {
            world.verified_metadata = None;
            world.backup_error = Some(e.to_string());
        }
    }
}

#[when("I restore the backup to a new data directory")]
fn when_restore_to_new_dir(world: &mut LatticeWorld) {
    let backup_path = world.backup_path.as_ref().expect("no backup path set");
    let tempdir = world._backup_tempdir.as_ref().expect("no tempdir");
    let data_dir = tempdir.path().join("restored-new");
    std::fs::create_dir_all(&data_dir).expect("failed to create data dir");

    match restore_backup(backup_path, &data_dir) {
        Ok(meta) => {
            world.verified_metadata = Some(meta);
            world.restore_data_dir = Some(data_dir);
            world.backup_error = None;
        }
        Err(e) => {
            world.backup_error = Some(e.to_string());
        }
    }
}

#[when("I restore the backup to that data directory")]
fn when_restore_to_existing_dir(world: &mut LatticeWorld) {
    let backup_path = world.backup_path.as_ref().expect("no backup path set");
    let data_dir = world
        .restore_data_dir
        .as_ref()
        .expect("no restore data dir set");

    match restore_backup(backup_path, data_dir) {
        Ok(meta) => {
            world.verified_metadata = Some(meta);
            world.backup_error = None;
        }
        Err(e) => {
            world.backup_error = Some(e.to_string());
        }
    }
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^the backup metadata should show (\d+) nodes and (\d+) allocations$"#)]
fn then_backup_metadata_counts(
    world: &mut LatticeWorld,
    expected_nodes: usize,
    expected_allocs: usize,
) {
    let meta = world.backup_metadata.as_ref().expect("no backup metadata");
    assert_eq!(
        meta.app.node_count, expected_nodes,
        "expected {} nodes in backup, got {}",
        expected_nodes, meta.app.node_count
    );
    assert_eq!(
        meta.app.allocation_count, expected_allocs,
        "expected {} allocations in backup, got {}",
        expected_allocs, meta.app.allocation_count
    );
}

#[then("the backup file should exist on disk")]
fn then_backup_exists(world: &mut LatticeWorld) {
    let path = world.backup_path.as_ref().expect("no backup path");
    assert!(path.exists(), "backup file does not exist at {path:?}");
}

#[then("the verification should succeed")]
fn then_verify_succeeds(world: &mut LatticeWorld) {
    assert!(
        world.backup_error.is_none(),
        "verification failed: {:?}",
        world.backup_error
    );
    assert!(
        world.verified_metadata.is_some(),
        "verified metadata should be present"
    );
}

#[then("the verified metadata should match the export metadata")]
fn then_verified_matches_export(world: &mut LatticeWorld) {
    let export_meta = world.backup_metadata.as_ref().expect("no export metadata");
    let verify_meta = world
        .verified_metadata
        .as_ref()
        .expect("no verified metadata");
    assert_eq!(
        export_meta.app.node_count, verify_meta.app.node_count,
        "node count mismatch"
    );
    assert_eq!(
        export_meta.app.allocation_count, verify_meta.app.allocation_count,
        "allocation count mismatch"
    );
    assert_eq!(
        export_meta.app.tenant_count, verify_meta.app.tenant_count,
        "tenant count mismatch"
    );
}

#[then(regex = r#"^the verification should fail with "([^"]+)"$"#)]
fn then_verify_fails(world: &mut LatticeWorld, expected_fragment: String) {
    let error = world
        .backup_error
        .as_ref()
        .expect("expected verification to fail, but it succeeded");
    assert!(
        error
            .to_lowercase()
            .contains(&expected_fragment.to_lowercase()),
        "expected error containing '{expected_fragment}', got '{error}'"
    );
}

#[then("the snapshot directory should contain a current pointer")]
fn then_snapshot_has_current(world: &mut LatticeWorld) {
    let data_dir = world
        .restore_data_dir
        .as_ref()
        .expect("no restore data dir");
    let snap_dir = data_dir.join("raft").join("snapshots");
    let current_path = snap_dir.join("current");
    assert!(
        current_path.exists(),
        "current pointer not found at {current_path:?}"
    );
}

#[then("the restored snapshot should be valid JSON")]
fn then_snapshot_valid_json(world: &mut LatticeWorld) {
    let data_dir = world
        .restore_data_dir
        .as_ref()
        .expect("no restore data dir");
    let snap_dir = data_dir.join("raft").join("snapshots");
    let current =
        std::fs::read_to_string(snap_dir.join("current")).expect("failed to read current pointer");
    let snapshot_path = snap_dir.join(current.trim());
    let contents = std::fs::read_to_string(&snapshot_path).expect("failed to read snapshot file");
    let _: serde_json::Value = serde_json::from_str(&contents).expect("snapshot is not valid JSON");
}

#[then("the WAL directory should be empty")]
fn then_wal_empty(world: &mut LatticeWorld) {
    let data_dir = world
        .restore_data_dir
        .as_ref()
        .expect("no restore data dir");
    let wal_dir = data_dir.join("raft").join("wal");
    if wal_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&wal_dir)
            .expect("failed to read WAL dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "WAL directory should be empty but contains {} entries",
            entries.len()
        );
    }
    // If WAL dir doesn't exist at all, that also counts as empty.
}

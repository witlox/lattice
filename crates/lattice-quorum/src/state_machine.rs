//! Raft state machine implementation backed by `GlobalState`.

use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openraft::storage::{RaftStateMachine, Snapshot};
use openraft::{
    EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, SnapshotMeta, StoredMembership,
};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::commands::CommandResponse;
use crate::global_state::GlobalState;
use crate::TypeConfig;

type LogIdOf = LogId<TypeConfig>;

/// Maximum number of old snapshot files to keep.
const MAX_SNAPSHOTS: usize = 3;

/// The Raft state machine wrapping our `GlobalState`.
pub struct LatticeStateMachine {
    state: Arc<RwLock<GlobalState>>,
    last_applied: Option<LogIdOf>,
    last_membership: StoredMembership<TypeConfig>,
    snapshot_idx: u64,
    /// When set, snapshots are persisted to this directory.
    snapshot_dir: Option<PathBuf>,
}

impl LatticeStateMachine {
    pub fn new(state: Arc<RwLock<GlobalState>>) -> Self {
        Self {
            state,
            last_applied: None,
            last_membership: StoredMembership::default(),
            snapshot_idx: 0,
            snapshot_dir: None,
        }
    }

    /// Create a state machine with persistent snapshot directory.
    ///
    /// On startup, loads the latest snapshot from disk if available.
    pub fn with_snapshot_dir(
        state: Arc<RwLock<GlobalState>>,
        snapshot_dir: PathBuf,
    ) -> io::Result<Self> {
        std::fs::create_dir_all(&snapshot_dir)?;

        let mut sm = Self {
            state,
            last_applied: None,
            last_membership: StoredMembership::default(),
            snapshot_idx: 0,
            snapshot_dir: Some(snapshot_dir),
        };

        // Try to load the latest snapshot from disk
        if let Some(ref dir) = sm.snapshot_dir {
            if let Some((meta, global_state)) = load_latest_snapshot(dir)? {
                debug!(
                    "Loaded snapshot from disk at {:?}",
                    meta.last_log_id
                );
                let mut state = sm.state.blocking_write();
                *state = global_state;
                sm.last_applied = meta.last_log_id;
                sm.last_membership = meta.last_membership;
                sm.snapshot_idx += 1;
            }
        }

        Ok(sm)
    }

    /// Get a read handle to the global state (for queries).
    pub fn state(&self) -> Arc<RwLock<GlobalState>> {
        Arc::clone(&self.state)
    }
}

/// Persisted snapshot metadata (stored alongside the state).
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedSnapshot {
    meta: PersistedSnapshotMeta,
    state: GlobalState,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedSnapshotMeta {
    last_log_id: Option<LogIdOf>,
    last_membership: StoredMembership<TypeConfig>,
    snapshot_id: String,
}

fn snapshot_filename(meta: &SnapshotMeta<TypeConfig>) -> String {
    let term = meta
        .last_log_id
        .map(|l| l.leader_id.term)
        .unwrap_or(0);
    let index = meta.last_log_id.map(|l| l.index).unwrap_or(0);
    format!("snap-{term}-{index}.json")
}

fn persist_snapshot(
    dir: &Path,
    meta: &SnapshotMeta<TypeConfig>,
    data: &[u8],
) -> io::Result<()> {
    let filename = snapshot_filename(meta);
    let path = dir.join(&filename);

    // Parse the state to persist it in our format
    let global_state: GlobalState = serde_json::from_slice(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let persisted = PersistedSnapshot {
        meta: PersistedSnapshotMeta {
            last_log_id: meta.last_log_id,
            last_membership: meta.last_membership.clone(),
            snapshot_id: meta.snapshot_id.clone(),
        },
        state: global_state,
    };

    let json = serde_json::to_vec_pretty(&persisted).map_err(io::Error::other)?;

    // Atomic write
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, &path)?;

    // Update "current" symlink
    let current = dir.join("current");
    let _ = std::fs::remove_file(&current);
    // Use a regular file pointing to the filename instead of symlink for portability
    std::fs::write(&current, filename.as_bytes())?;

    // Prune old snapshots
    prune_old_snapshots(dir)?;

    debug!("Persisted snapshot to {}", path.display());
    Ok(())
}

fn prune_old_snapshots(dir: &Path) -> io::Result<()> {
    let mut snaps: Vec<(PathBuf, u64)> = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("snap-") && name_str.ends_with(".json") {
            // Extract index from snap-{term}-{index}.json
            let parts: Vec<&str> = name_str
                .trim_start_matches("snap-")
                .trim_end_matches(".json")
                .split('-')
                .collect();
            if parts.len() == 2 {
                if let Ok(index) = parts[1].parse::<u64>() {
                    snaps.push((entry.path(), index));
                }
            }
        }
    }

    // Sort by index descending
    snaps.sort_by(|a, b| b.1.cmp(&a.1));

    // Remove old ones beyond MAX_SNAPSHOTS
    for (path, _) in snaps.iter().skip(MAX_SNAPSHOTS) {
        debug!("Pruning old snapshot: {}", path.display());
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

fn load_latest_snapshot(dir: &Path) -> io::Result<Option<(SnapshotMeta<TypeConfig>, GlobalState)>> {
    // Try to read "current" file
    let current_path = dir.join("current");
    if !current_path.exists() {
        return Ok(None);
    }

    let filename = std::fs::read_to_string(&current_path)?.trim().to_string();
    let snap_path = dir.join(&filename);

    if !snap_path.exists() {
        warn!("Current snapshot file {} not found", snap_path.display());
        return Ok(None);
    }

    let data = std::fs::read_to_string(&snap_path)?;
    let persisted: PersistedSnapshot = serde_json::from_str(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let meta = SnapshotMeta {
        last_log_id: persisted.meta.last_log_id,
        last_membership: persisted.meta.last_membership,
        snapshot_id: persisted.meta.snapshot_id,
    };

    Ok(Some((meta, persisted.state)))
}

impl RaftStateMachine<TypeConfig> for LatticeStateMachine {
    type SnapshotBuilder = LatticeSnapshotBuilder;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogIdOf>, StoredMembership<TypeConfig>), io::Error> {
        Ok((self.last_applied, self.last_membership.clone()))
    }

    async fn apply<Strm>(&mut self, entries: Strm) -> Result<(), io::Error>
    where
        Strm: futures::Stream<Item = Result<openraft::storage::EntryResponder<TypeConfig>, io::Error>>
            + Unpin
            + OptionalSend,
    {
        use futures::StreamExt;

        let mut stream = entries;
        while let Some(item) = stream.next().await {
            let (entry, responder) = item?;

            self.last_applied = Some(entry.log_id);

            let response = match entry.payload {
                EntryPayload::Blank => CommandResponse::Ok,
                EntryPayload::Normal(cmd) => {
                    let mut state = self.state.write().await;
                    debug!("Applying command to state machine: {}", cmd);
                    state.apply(cmd)
                }
                EntryPayload::Membership(mem) => {
                    self.last_membership = StoredMembership::new(self.last_applied, mem);
                    CommandResponse::Ok
                }
            };

            if let Some(r) = responder {
                r.send(response);
            }
        }

        Ok(())
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        LatticeSnapshotBuilder {
            state: Arc::clone(&self.state),
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot_idx: self.snapshot_idx,
            snapshot_dir: self.snapshot_dir.clone(),
        }
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Cursor<Vec<u8>>, io::Error> {
        Ok(Cursor::new(Vec::new()))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<TypeConfig>,
        snapshot: Cursor<Vec<u8>>,
    ) -> Result<(), io::Error> {
        let data = snapshot.into_inner();
        let new_state: GlobalState = serde_json::from_slice(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Persist snapshot to disk if configured
        if let Some(ref dir) = self.snapshot_dir {
            persist_snapshot(dir, meta, &data)?;
        }

        let mut state = self.state.write().await;
        *state = new_state;

        self.last_applied = meta.last_log_id;
        self.last_membership = meta.last_membership.clone();
        self.snapshot_idx += 1;

        debug!("Installed snapshot at {:?}", meta.last_log_id);
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<TypeConfig>>, io::Error> {
        // If we have a snapshot dir, try loading from disk first (cold start case)
        if self.last_applied.is_none() {
            if let Some(ref dir) = self.snapshot_dir {
                if let Some((meta, global_state)) = load_latest_snapshot(dir)? {
                    let data = serde_json::to_vec(&global_state).map_err(io::Error::other)?;
                    let mut state = self.state.write().await;
                    *state = global_state;
                    self.last_applied = meta.last_log_id;
                    self.last_membership = meta.last_membership.clone();
                    self.snapshot_idx += 1;

                    return Ok(Some(Snapshot {
                        meta,
                        snapshot: Cursor::new(data),
                    }));
                }
            }
        }

        let state = self.state.read().await;
        let data = serde_json::to_vec(&*state).map_err(io::Error::other)?;

        if self.last_applied.is_none() {
            return Ok(None);
        }

        let snapshot = Snapshot {
            meta: SnapshotMeta {
                last_log_id: self.last_applied,
                last_membership: self.last_membership.clone(),
                snapshot_id: format!("snap-{}", self.snapshot_idx),
            },
            snapshot: Cursor::new(data),
        };

        Ok(Some(snapshot))
    }
}

/// Builds snapshots from the current state.
pub struct LatticeSnapshotBuilder {
    state: Arc<RwLock<GlobalState>>,
    last_applied: Option<LogIdOf>,
    last_membership: StoredMembership<TypeConfig>,
    snapshot_idx: u64,
    snapshot_dir: Option<PathBuf>,
}

impl RaftSnapshotBuilder<TypeConfig> for LatticeSnapshotBuilder {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, io::Error> {
        let state = self.state.read().await;
        let data = serde_json::to_vec(&*state).map_err(io::Error::other)?;

        self.snapshot_idx += 1;

        let meta = SnapshotMeta {
            last_log_id: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot_id: format!("snap-{}", self.snapshot_idx),
        };

        // Persist snapshot to disk if configured
        if let Some(ref dir) = self.snapshot_dir {
            persist_snapshot(dir, &meta, &data)?;
        }

        let snapshot = Snapshot {
            meta,
            snapshot: Cursor::new(data),
        };

        debug!("Built snapshot at {:?}", self.last_applied);
        Ok(snapshot)
    }
}

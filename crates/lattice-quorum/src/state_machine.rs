//! Raft state machine implementation backed by `GlobalState`.

use std::io;
use std::io::Cursor;
use std::sync::Arc;

use openraft::storage::{RaftStateMachine, Snapshot};
use openraft::{
    EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, SnapshotMeta, StoredMembership,
};
use tokio::sync::RwLock;
use tracing::debug;

use crate::commands::CommandResponse;
use crate::global_state::GlobalState;
use crate::TypeConfig;

type LogIdOf = LogId<TypeConfig>;

/// The Raft state machine wrapping our `GlobalState`.
pub struct LatticeStateMachine {
    state: Arc<RwLock<GlobalState>>,
    last_applied: Option<LogIdOf>,
    last_membership: StoredMembership<TypeConfig>,
    snapshot_idx: u64,
}

impl LatticeStateMachine {
    pub fn new(state: Arc<RwLock<GlobalState>>) -> Self {
        Self {
            state,
            last_applied: None,
            last_membership: StoredMembership::default(),
            snapshot_idx: 0,
        }
    }

    /// Get a read handle to the global state (for queries).
    pub fn state(&self) -> Arc<RwLock<GlobalState>> {
        Arc::clone(&self.state)
    }
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

        let mut state = self.state.write().await;
        *state = new_state;

        self.last_applied = meta.last_log_id;
        self.last_membership = meta.last_membership.clone();
        self.snapshot_idx += 1;

        debug!("Installed snapshot at {:?}", meta.last_log_id);
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<TypeConfig>>, io::Error> {
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
}

impl RaftSnapshotBuilder<TypeConfig> for LatticeSnapshotBuilder {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, io::Error> {
        let state = self.state.read().await;
        let data = serde_json::to_vec(&*state).map_err(io::Error::other)?;

        self.snapshot_idx += 1;

        let snapshot = Snapshot {
            meta: SnapshotMeta {
                last_log_id: self.last_applied,
                last_membership: self.last_membership.clone(),
                snapshot_id: format!("snap-{}", self.snapshot_idx),
            },
            snapshot: Cursor::new(data),
        };

        debug!("Built snapshot at {:?}", self.last_applied);
        Ok(snapshot)
    }
}

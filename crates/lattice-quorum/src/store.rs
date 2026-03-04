//! In-memory log storage for Raft.
//!
//! This provides both `RaftLogStorage` and `RaftLogReader` implementations
//! backed by a `Vec<Entry>` and a stored vote. Suitable for testing and
//! single-process deployments; production would add persistence.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::io;
use std::ops::RangeBounds;
use std::sync::Arc;

use openraft::storage::{IOFlushed, RaftLogStorage};
use openraft::{LogId, LogState, OptionalSend, RaftLogReader};
use tokio::sync::RwLock;

use crate::TypeConfig;

type EntryOf = openraft::Entry<TypeConfig>;
type VoteOf = openraft::vote::Vote<TypeConfig>;
type LogIdOf = LogId<TypeConfig>;

/// In-memory log store shared behind an `Arc<RwLock<_>>`.
#[derive(Clone)]
pub struct MemLogStore {
    inner: Arc<RwLock<MemLogStoreInner>>,
}

struct MemLogStoreInner {
    vote: Option<VoteOf>,
    log: BTreeMap<u64, EntryOf>,
    committed: Option<LogIdOf>,
    last_purged: Option<LogIdOf>,
}

impl Default for MemLogStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MemLogStoreInner {
                vote: None,
                log: BTreeMap::new(),
                committed: None,
                last_purged: None,
            })),
        }
    }
}

impl MemLogStore {
    pub fn new() -> Self {
        Self::default()
    }
}

// ── RaftLogReader ──────────────────────────────────────────

/// A cloneable reader into the in-memory log store.
#[derive(Clone)]
pub struct MemLogReader {
    inner: Arc<RwLock<MemLogStoreInner>>,
}

impl RaftLogReader<TypeConfig> for MemLogReader {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<EntryOf>, io::Error> {
        let inner = self.inner.read().await;
        let entries: Vec<EntryOf> = inner.log.range(range).map(|(_, e)| e.clone()).collect();
        Ok(entries)
    }

    async fn read_vote(&mut self) -> Result<Option<VoteOf>, io::Error> {
        let inner = self.inner.read().await;
        Ok(inner.vote)
    }
}

// ── RaftLogStorage ─────────────────────────────────────────

impl RaftLogStorage<TypeConfig> for MemLogStore {
    type LogReader = MemLogReader;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, io::Error> {
        let inner = self.inner.read().await;
        let last = inner.log.values().last().map(|e| e.log_id);
        Ok(LogState {
            last_purged_log_id: inner.last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        MemLogReader {
            inner: Arc::clone(&self.inner),
        }
    }

    async fn save_vote(&mut self, vote: &VoteOf) -> Result<(), io::Error> {
        let mut inner = self.inner.write().await;
        inner.vote = Some(*vote);
        Ok(())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: IOFlushed<TypeConfig>,
    ) -> Result<(), io::Error>
    where
        I: IntoIterator<Item = EntryOf> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let mut inner = self.inner.write().await;
        for entry in entries {
            let index = entry.log_id.index;
            inner.log.insert(index, entry);
        }
        // In-memory store: data is immediately durable
        callback.io_completed(Ok(()));
        Ok(())
    }

    async fn truncate_after(&mut self, last_log_id: Option<LogIdOf>) -> Result<(), io::Error> {
        let mut inner = self.inner.write().await;
        match last_log_id {
            Some(id) => {
                // Keep entries up to and including id.index
                let keys_to_remove: Vec<u64> =
                    inner.log.range((id.index + 1)..).map(|(k, _)| *k).collect();
                for k in keys_to_remove {
                    inner.log.remove(&k);
                }
            }
            None => {
                inner.log.clear();
            }
        }
        Ok(())
    }

    async fn purge(&mut self, log_id: LogIdOf) -> Result<(), io::Error> {
        let mut inner = self.inner.write().await;
        let keys_to_remove: Vec<u64> = inner.log.range(..=log_id.index).map(|(k, _)| *k).collect();
        for k in keys_to_remove {
            inner.log.remove(&k);
        }
        inner.last_purged = Some(log_id);
        Ok(())
    }

    async fn save_committed(&mut self, committed: Option<LogIdOf>) -> Result<(), io::Error> {
        let mut inner = self.inner.write().await;
        inner.committed = committed;
        Ok(())
    }

    async fn read_committed(&mut self) -> Result<Option<LogIdOf>, io::Error> {
        let inner = self.inner.read().await;
        Ok(inner.committed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initial_state_is_empty() {
        let mut store = MemLogStore::new();
        let state = store.get_log_state().await.unwrap();
        assert!(state.last_log_id.is_none());
        assert!(state.last_purged_log_id.is_none());
    }

    #[tokio::test]
    async fn save_and_read_vote() {
        let mut store = MemLogStore::new();
        let vote = openraft::vote::Vote::new(1, 1);
        store.save_vote(&vote).await.unwrap();

        let mut reader = store.get_log_reader().await;
        let read = reader.read_vote().await.unwrap();
        assert_eq!(read.unwrap(), vote);
    }

    // Note: append/truncate/purge are tested implicitly via the integration
    // tests in lib.rs (single_node_quorum_works, three_node_cluster_works)
    // since IOFlushed::new is pub(crate) in openraft 0.10.
}

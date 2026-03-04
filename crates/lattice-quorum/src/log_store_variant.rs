//! Polymorphic log store that switches between in-memory and file-backed storage.

use std::fmt::Debug;
use std::io;
use std::ops::RangeBounds;

use openraft::storage::{IOFlushed, RaftLogStorage};
use openraft::{LogState, OptionalSend, RaftLogReader};

use crate::persistent_store::{FileLogReader, FileLogStore};
use crate::store::{MemLogReader, MemLogStore};
use crate::TypeConfig;

type EntryOf = openraft::Entry<TypeConfig>;
type VoteOf = openraft::vote::Vote<TypeConfig>;
type LogIdOf = openraft::LogId<TypeConfig>;

/// A log store that can be either in-memory or file-backed.
#[derive(Clone)]
pub enum LogStoreVariant {
    Memory(MemLogStore),
    File(FileLogStore),
}

/// A log reader that matches the LogStoreVariant.
#[derive(Clone)]
pub enum LogReaderVariant {
    Memory(MemLogReader),
    File(FileLogReader),
}

impl RaftLogReader<TypeConfig> for LogReaderVariant {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<EntryOf>, io::Error> {
        match self {
            LogReaderVariant::Memory(r) => r.try_get_log_entries(range).await,
            LogReaderVariant::File(r) => r.try_get_log_entries(range).await,
        }
    }

    async fn read_vote(&mut self) -> Result<Option<VoteOf>, io::Error> {
        match self {
            LogReaderVariant::Memory(r) => r.read_vote().await,
            LogReaderVariant::File(r) => r.read_vote().await,
        }
    }
}

impl RaftLogStorage<TypeConfig> for LogStoreVariant {
    type LogReader = LogReaderVariant;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, io::Error> {
        match self {
            LogStoreVariant::Memory(s) => s.get_log_state().await,
            LogStoreVariant::File(s) => s.get_log_state().await,
        }
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        match self {
            LogStoreVariant::Memory(s) => LogReaderVariant::Memory(s.get_log_reader().await),
            LogStoreVariant::File(s) => LogReaderVariant::File(s.get_log_reader().await),
        }
    }

    async fn save_vote(&mut self, vote: &VoteOf) -> Result<(), io::Error> {
        match self {
            LogStoreVariant::Memory(s) => s.save_vote(vote).await,
            LogStoreVariant::File(s) => s.save_vote(vote).await,
        }
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
        match self {
            LogStoreVariant::Memory(s) => s.append(entries, callback).await,
            LogStoreVariant::File(s) => s.append(entries, callback).await,
        }
    }

    async fn truncate_after(&mut self, last_log_id: Option<LogIdOf>) -> Result<(), io::Error> {
        match self {
            LogStoreVariant::Memory(s) => s.truncate_after(last_log_id).await,
            LogStoreVariant::File(s) => s.truncate_after(last_log_id).await,
        }
    }

    async fn purge(&mut self, log_id: LogIdOf) -> Result<(), io::Error> {
        match self {
            LogStoreVariant::Memory(s) => s.purge(log_id).await,
            LogStoreVariant::File(s) => s.purge(log_id).await,
        }
    }

    async fn save_committed(&mut self, committed: Option<LogIdOf>) -> Result<(), io::Error> {
        match self {
            LogStoreVariant::Memory(s) => s.save_committed(committed).await,
            LogStoreVariant::File(s) => s.save_committed(committed).await,
        }
    }

    async fn read_committed(&mut self) -> Result<Option<LogIdOf>, io::Error> {
        match self {
            LogStoreVariant::Memory(s) => s.read_committed().await,
            LogStoreVariant::File(s) => s.read_committed().await,
        }
    }
}

//! Local key-value store and fence barrier for PMI-2.
//!
//! Each node agent maintains a `LocalKvs` per launch. During a fence,
//! local entries are collected and exchanged with peer node agents.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

/// Local PMI-2 key-value store for one launch on one node.
#[derive(Debug)]
pub struct LocalKvs {
    inner: Arc<Mutex<KvsInner>>,
    fence_complete: Arc<Notify>,
}

#[derive(Debug)]
struct KvsInner {
    /// Local entries put by ranks on this node (pre-fence).
    pending: HashMap<String, String>,
    /// Merged global KVS (available after fence completes).
    merged: HashMap<String, String>,
    /// How many local ranks have called kvsfence.
    fence_count: u32,
    /// Total local ranks expected.
    local_rank_count: u32,
    /// Generation counter — incremented on each fence completion.
    /// Ranks wait for their generation to advance.
    generation: u64,
}

impl LocalKvs {
    /// Create a new KVS for `local_rank_count` ranks on this node.
    pub fn new(local_rank_count: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(KvsInner {
                pending: HashMap::new(),
                merged: HashMap::new(),
                fence_count: 0,
                local_rank_count,
                generation: 0,
            })),
            fence_complete: Arc::new(Notify::new()),
        }
    }

    /// Store a key-value pair (called by a rank's kvsput).
    pub async fn put(&self, key: String, value: String) {
        let mut inner = self.inner.lock().await;
        inner.pending.insert(key, value);
    }

    /// Get a value by key. Searches merged KVS first (post-fence),
    /// then pending (pre-fence local entries).
    pub async fn get(&self, key: &str) -> Option<String> {
        let inner = self.inner.lock().await;
        inner
            .merged
            .get(key)
            .or_else(|| inner.pending.get(key))
            .cloned()
    }

    /// Called by a rank when it hits kvsfence. Returns `(all_local, generation)`
    /// where `all_local` is true if all local ranks reached the fence.
    /// The returned generation is the current (pre-completion) generation.
    pub async fn enter_fence(&self) -> (bool, u64) {
        let mut inner = self.inner.lock().await;
        inner.fence_count += 1;
        let all = inner.fence_count >= inner.local_rank_count;
        (all, inner.generation)
    }

    /// Get all pending (local) KVS entries for cross-node exchange.
    pub async fn drain_pending(&self) -> HashMap<String, String> {
        let inner = self.inner.lock().await;
        inner.pending.clone()
    }

    /// Set the merged KVS after cross-node exchange completes.
    /// Advances the generation and wakes all ranks blocked on the fence.
    pub async fn complete_fence(&self, merged: HashMap<String, String>) {
        {
            let mut inner = self.inner.lock().await;
            inner.merged.extend(merged);
            // Also merge pending into merged so local keys are findable
            let pending = std::mem::take(&mut inner.pending);
            inner.merged.extend(pending);
            inner.generation += 1;
            inner.fence_count = 0;
        }
        self.fence_complete.notify_waiters();
    }

    /// Wait for the fence generation to advance past `gen`.
    pub async fn wait_fence(&self, gen: u64) {
        loop {
            let notified = self.fence_complete.notified();
            {
                let inner = self.inner.lock().await;
                if inner.generation > gen {
                    return;
                }
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_and_get() {
        let kvs = LocalKvs::new(2);
        kvs.put("key1".into(), "val1".into()).await;
        assert_eq!(kvs.get("key1").await, Some("val1".to_string()));
        assert_eq!(kvs.get("missing").await, None);
    }

    #[tokio::test]
    async fn fence_barrier_triggers_on_all_ranks() {
        let kvs = LocalKvs::new(3);
        assert!(!kvs.enter_fence().await.0);
        assert!(!kvs.enter_fence().await.0);
        assert!(kvs.enter_fence().await.0); // 3rd rank triggers
    }

    #[tokio::test]
    async fn merged_kvs_available_after_fence() {
        let kvs = LocalKvs::new(1);
        kvs.put("local_key".into(), "local_val".into()).await;

        let mut merged = HashMap::new();
        merged.insert("remote_key".into(), "remote_val".into());
        kvs.complete_fence(merged).await;

        assert_eq!(kvs.get("remote_key").await, Some("remote_val".to_string()));
        assert_eq!(kvs.get("local_key").await, Some("local_val".to_string()));
    }

    #[tokio::test]
    async fn drain_pending_returns_local_entries() {
        let kvs = LocalKvs::new(2);
        kvs.put("a".into(), "1".into()).await;
        kvs.put("b".into(), "2".into()).await;
        let pending = kvs.drain_pending().await;
        assert_eq!(pending.len(), 2);
        assert_eq!(pending.get("a").unwrap(), "1");
    }

    #[tokio::test]
    async fn wait_fence_returns_after_complete() {
        let kvs = Arc::new(LocalKvs::new(1));
        let kvs2 = kvs.clone();

        let handle = tokio::spawn(async move {
            kvs2.wait_fence(0).await;
        });

        // Small delay to ensure waiter is registered
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        kvs.complete_fence(HashMap::new()).await;

        // Should not hang
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("timeout")
            .expect("join");
    }

    #[tokio::test]
    async fn multiple_fence_generations() {
        let kvs = Arc::new(LocalKvs::new(1));

        // First fence
        let (all, gen) = kvs.enter_fence().await;
        assert!(all);
        assert_eq!(gen, 0);
        kvs.complete_fence(HashMap::new()).await;

        // Second fence
        let (all, gen) = kvs.enter_fence().await;
        assert!(all);
        assert_eq!(gen, 1);
        kvs.complete_fence(HashMap::new()).await;
    }
}

//! Cross-node PMI fence coordinator.
//!
//! The fence is a barrier + allgather of KVS entries across all nodes
//! in a launch. Uses a star topology: non-head nodes send their local
//! KVS entries to the head node, which merges and broadcasts back.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::debug;

use lattice_common::types::{LaunchId, PeerInfo};

/// Trait for sending fence RPCs to peer node agents.
/// Implemented by the gRPC client; mockable for testing.
#[async_trait::async_trait]
pub trait FenceTransport: Send + Sync {
    /// Send local KVS entries to the head node. Returns the merged KVS.
    async fn send_fence(
        &self,
        peer_address: &str,
        launch_id: LaunchId,
        kvs_entries: HashMap<String, String>,
        node_index: u32,
    ) -> Result<HashMap<String, String>, String>;
}

/// Coordinates cross-node PMI fence for one launch.
pub struct FenceCoordinator {
    launch_id: LaunchId,
    peers: Vec<PeerInfo>,
    head_index: u32,
    my_index: u32,
    transport: Arc<dyn FenceTransport>,
    fence_timeout: Duration,
    /// Head-only: collected entries from peers during fence.
    collected: Arc<Mutex<HeadState>>,
}

impl std::fmt::Debug for FenceCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FenceCoordinator")
            .field("launch_id", &self.launch_id)
            .field("head_index", &self.head_index)
            .field("my_index", &self.my_index)
            .finish_non_exhaustive()
    }
}

struct HeadState {
    entries: HashMap<u32, HashMap<String, String>>,
    expected: u32,
}

impl FenceCoordinator {
    pub fn new(
        launch_id: LaunchId,
        peers: Vec<PeerInfo>,
        head_index: u32,
        my_index: u32,
        transport: Arc<dyn FenceTransport>,
    ) -> Self {
        let expected = peers.len() as u32;
        Self {
            launch_id,
            peers,
            head_index,
            my_index,
            transport,
            fence_timeout: Duration::from_secs(60),
            collected: Arc::new(Mutex::new(HeadState {
                entries: HashMap::new(),
                expected,
            })),
        }
    }

    /// Set a custom fence timeout (default: 60s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.fence_timeout = timeout;
        self
    }

    /// Whether this node is the head (fence coordinator).
    pub fn is_head(&self) -> bool {
        self.my_index == self.head_index
    }

    /// Execute the fence for this node's local KVS entries.
    ///
    /// - Non-head: sends entries to head, receives merged KVS.
    /// - Head: waits for all peers, merges, returns merged KVS.
    ///   (Peers are handled via `receive_peer_fence`.)
    pub async fn execute_fence(
        &self,
        local_entries: HashMap<String, String>,
    ) -> Result<HashMap<String, String>, String> {
        if self.peers.len() <= 1 {
            // Single node: no cross-node exchange needed
            return Ok(local_entries);
        }

        if self.is_head() {
            // Head node: store own entries and wait for peers
            self.receive_peer_fence(self.my_index, local_entries).await
        } else {
            // Non-head: send to head, get merged back
            let head_addr = &self.peers[self.head_index as usize].grpc_address;
            let result = tokio::time::timeout(
                self.fence_timeout,
                self.transport
                    .send_fence(head_addr, self.launch_id, local_entries, self.my_index),
            )
            .await
            .map_err(|_| "fence timeout".to_string())?;

            result
        }
    }

    /// Head node: receive KVS entries from a peer (or self).
    /// Returns the merged KVS when all peers have reported.
    pub async fn receive_peer_fence(
        &self,
        node_index: u32,
        entries: HashMap<String, String>,
    ) -> Result<HashMap<String, String>, String> {
        let mut state = self.collected.lock().await;
        state.entries.insert(node_index, entries);

        debug!(
            launch_id = %self.launch_id,
            received = state.entries.len(),
            expected = state.expected,
            "fence: received entries from node {node_index}"
        );

        if state.entries.len() as u32 >= state.expected {
            // All peers reported — merge
            let mut merged = HashMap::new();
            for (_idx, kvs) in state.entries.drain() {
                merged.extend(kvs);
            }
            debug!(
                launch_id = %self.launch_id,
                keys = merged.len(),
                "fence: merged all KVS entries"
            );
            Ok(merged)
        } else {
            // Not all peers yet — caller needs to wait
            // In the actual implementation, this is handled by the gRPC
            // server blocking until all peers report. For now, return empty
            // to indicate "not ready" — the server layer handles the wait.
            Err("fence: waiting for more peers".to_string())
        }
    }

    /// Reset fence state for the next fence cycle.
    pub async fn reset(&self) {
        let mut state = self.collected.lock().await;
        state.entries.clear();
    }
}

/// Mock transport for testing.
pub struct MockFenceTransport {
    /// When send_fence is called, store the request and return this response.
    response: Mutex<Option<HashMap<String, String>>>,
}

impl Default for MockFenceTransport {
    fn default() -> Self {
        Self {
            response: Mutex::new(None),
        }
    }
}

impl MockFenceTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set_response(&self, merged: HashMap<String, String>) {
        *self.response.lock().await = Some(merged);
    }
}

#[async_trait::async_trait]
impl FenceTransport for MockFenceTransport {
    async fn send_fence(
        &self,
        _peer_address: &str,
        _launch_id: LaunchId,
        _kvs_entries: HashMap<String, String>,
        _node_index: u32,
    ) -> Result<HashMap<String, String>, String> {
        self.response
            .lock()
            .await
            .clone()
            .ok_or_else(|| "mock: no response set".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_peers() -> Vec<PeerInfo> {
        vec![
            PeerInfo {
                node_id: "node0".into(),
                grpc_address: "http://node0:50052".into(),
                first_rank: 0,
                num_ranks: 4,
            },
            PeerInfo {
                node_id: "node1".into(),
                grpc_address: "http://node1:50052".into(),
                first_rank: 4,
                num_ranks: 4,
            },
        ]
    }

    #[tokio::test]
    async fn single_node_fence_returns_local() {
        let transport = Arc::new(MockFenceTransport::new());
        let coord = FenceCoordinator::new(
            uuid::Uuid::new_v4(),
            vec![PeerInfo {
                node_id: "node0".into(),
                grpc_address: "http://node0:50052".into(),
                first_rank: 0,
                num_ranks: 4,
            }],
            0,
            0,
            transport,
        );

        let mut local = HashMap::new();
        local.insert("key0".into(), "val0".into());

        let merged = coord.execute_fence(local.clone()).await.unwrap();
        assert_eq!(merged, local);
    }

    #[tokio::test]
    async fn non_head_sends_to_head() {
        let transport = Arc::new(MockFenceTransport::new());
        let mut expected = HashMap::new();
        expected.insert("key0".into(), "val0".into());
        expected.insert("key1".into(), "val1".into());
        transport.set_response(expected.clone()).await;

        let coord = FenceCoordinator::new(
            uuid::Uuid::new_v4(),
            two_peers(),
            0, // head is node 0
            1, // we are node 1
            transport,
        );

        let mut local = HashMap::new();
        local.insert("key1".into(), "val1".into());

        let merged = coord.execute_fence(local).await.unwrap();
        assert_eq!(merged, expected);
    }

    #[tokio::test]
    async fn head_merges_all_peers() {
        let transport = Arc::new(MockFenceTransport::new());
        let coord = FenceCoordinator::new(uuid::Uuid::new_v4(), two_peers(), 0, 0, transport);

        // Simulate peer 1 arriving first
        let mut peer1_entries = HashMap::new();
        peer1_entries.insert("key1".into(), "val1".into());
        let result = coord.receive_peer_fence(1, peer1_entries).await;
        assert!(result.is_err()); // Not all peers yet

        // Now head's own entries arrive
        let mut head_entries = HashMap::new();
        head_entries.insert("key0".into(), "val0".into());
        let merged = coord.receive_peer_fence(0, head_entries).await.unwrap();

        assert_eq!(merged.len(), 2);
        assert_eq!(merged.get("key0").unwrap(), "val0");
        assert_eq!(merged.get("key1").unwrap(), "val1");
    }

    #[tokio::test]
    async fn reset_clears_state() {
        let transport = Arc::new(MockFenceTransport::new());
        let coord = FenceCoordinator::new(uuid::Uuid::new_v4(), two_peers(), 0, 0, transport);

        let mut entries = HashMap::new();
        entries.insert("k".into(), "v".into());
        let _ = coord.receive_peer_fence(0, entries).await;

        coord.reset().await;

        // After reset, should need all peers again
        let entries2 = HashMap::new();
        let result = coord.receive_peer_fence(0, entries2).await;
        assert!(result.is_err()); // Missing peer 1
    }

    #[tokio::test]
    async fn transport_failure_propagates_error() {
        // MockFenceTransport with no response set returns Err("mock: no response set")
        let transport = Arc::new(MockFenceTransport::new());
        // Don't set a response — send_fence will return Err

        let coord = FenceCoordinator::new(
            uuid::Uuid::new_v4(),
            two_peers(),
            0, // head is node 0
            1, // we are node 1 (non-head)
            transport,
        );

        let local = HashMap::new();
        let result = coord.execute_fence(local).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("no response set"),
            "transport error should propagate"
        );
    }

    #[tokio::test]
    async fn head_with_single_peer() {
        // Head node (index 0) with only 1 peer (itself).
        // execute_fence should return local entries directly (single-node fast path).
        let transport = Arc::new(MockFenceTransport::new());
        let peers = vec![PeerInfo {
            node_id: "node0".into(),
            grpc_address: "http://node0:50052".into(),
            first_rank: 0,
            num_ranks: 4,
        }];
        let coord = FenceCoordinator::new(uuid::Uuid::new_v4(), peers, 0, 0, transport);

        let mut local = HashMap::new();
        local.insert("mykey".into(), "myval".into());

        let merged = coord.execute_fence(local.clone()).await.unwrap();
        assert_eq!(merged, local);
    }

    #[tokio::test]
    async fn fence_with_empty_kvs() {
        // All nodes contribute empty HashMap. Merged result should be empty.
        let transport = Arc::new(MockFenceTransport::new());
        let coord = FenceCoordinator::new(uuid::Uuid::new_v4(), two_peers(), 0, 0, transport);

        // Peer 1 sends empty
        let result = coord.receive_peer_fence(1, HashMap::new()).await;
        assert!(result.is_err()); // still waiting for peer 0

        // Head (peer 0) sends empty
        let merged = coord.receive_peer_fence(0, HashMap::new()).await.unwrap();
        assert!(
            merged.is_empty(),
            "merged KVS should be empty when all contribute nothing"
        );
    }
}

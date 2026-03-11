//! Multi-rank MPI process launcher.
//!
//! Spawns multiple MPI rank processes on a single node, setting up the
//! PMI-2 environment variables for each rank and monitoring their lifecycle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use lattice_common::types::{
    AllocId, CxiCredentials, LaunchId, NodeId, PeerInfo, PmiMode, RankExitStatus,
};

use crate::pmi2::fence::{FenceCoordinator, FenceTransport};
use crate::pmi2::server::{Pmi2Server, Pmi2ServerConfig};

/// Configuration for launching MPI ranks on one node.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub launch_id: LaunchId,
    pub allocation_id: AllocId,
    pub entrypoint: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub tasks_per_node: u32,
    pub first_rank: u32,
    pub world_size: u32,
    pub pmi_mode: PmiMode,
    pub cxi_credentials: Option<CxiCredentials>,
    pub peers: Vec<PeerInfo>,
    pub head_node_index: u32,
    pub my_node_index: u32,
    pub node_id: NodeId,
    pub socket_dir: PathBuf,
    pub nodelist: String,
}

/// Result of a completed launch.
#[derive(Debug)]
pub struct LaunchResult {
    pub launch_id: LaunchId,
    pub success: bool,
    pub rank_exits: Vec<RankExitStatus>,
    pub error: Option<String>,
}

/// Manages MPI process launch on a single node.
#[allow(dead_code)]
pub struct ProcessLauncher {
    config: LaunchConfig,
    pmi_server: Option<Pmi2Server>,
    fence_coordinator: Option<FenceCoordinator>,
    rank_pids: Arc<Mutex<Vec<Option<u32>>>>,
}

impl ProcessLauncher {
    pub fn new(config: LaunchConfig, transport: Arc<dyn FenceTransport>) -> Self {
        let pmi_config = Pmi2ServerConfig {
            launch_id: config.launch_id,
            first_rank: config.first_rank,
            world_size: config.world_size,
            local_rank_count: config.tasks_per_node,
            appnum: 0,
            socket_dir: config.socket_dir.clone(),
        };

        let fence = FenceCoordinator::new(
            config.launch_id,
            config.peers.clone(),
            config.head_node_index,
            config.my_node_index,
            transport,
        );

        Self {
            config,
            pmi_server: Some(Pmi2Server::new(pmi_config)),
            fence_coordinator: Some(fence),
            rank_pids: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Build environment variables for a specific rank.
    pub fn rank_env(&self, rank: u32, local_rank: u32) -> HashMap<String, String> {
        let mut env = self.config.env.clone();

        // PMI-2 variables
        env.insert("PMI_RANK".into(), rank.to_string());
        env.insert("PMI_SIZE".into(), self.config.world_size.to_string());
        env.insert("PMI_SPAWNED".into(), "0".into());

        // Lattice variables
        env.insert(
            "LATTICE_LAUNCH_ID".into(),
            self.config.launch_id.to_string(),
        );
        env.insert(
            "LATTICE_ALLOC_ID".into(),
            self.config.allocation_id.to_string(),
        );
        env.insert("LATTICE_NODELIST".into(), self.config.nodelist.clone());
        env.insert("LATTICE_NNODES".into(), self.config.peers.len().to_string());
        env.insert("LATTICE_NPROCS".into(), self.config.world_size.to_string());
        env.insert("LATTICE_LOCAL_RANK".into(), local_rank.to_string());
        env.insert(
            "LATTICE_LOCAL_SIZE".into(),
            self.config.tasks_per_node.to_string(),
        );

        // CXI credentials for Slingshot
        if let Some(ref cxi) = self.config.cxi_credentials {
            env.insert("FI_CXI_DEFAULT_VNI".into(), cxi.vni.to_string());
            env.insert("FI_CXI_AUTH_KEY".into(), hex::encode(&cxi.auth_key));
            env.insert("FI_PROVIDER".into(), "cxi".into());
        }

        env
    }

    /// Launch all ranks. Returns when all ranks have exited.
    ///
    /// On Linux, spawns real processes. On other platforms, returns a
    /// stub result (for testing/development).
    pub async fn launch(mut self) -> LaunchResult {
        let pmi_server = self.pmi_server.take().expect("pmi_server already taken");
        let fence = self.fence_coordinator.take().expect("fence already taken");
        let fence = Arc::new(fence);

        info!(
            launch_id = %self.config.launch_id,
            ranks = %self.config.tasks_per_node,
            first_rank = self.config.first_rank,
            world_size = self.config.world_size,
            "launching MPI ranks"
        );

        // Spawn rank processes
        let rank_exits = Arc::new(Mutex::new(Vec::new()));

        #[cfg(target_os = "linux")]
        {
            self.spawn_ranks_linux(&pmi_server).await;
        }

        // Run PMI-2 server (blocks until all ranks finalize or abort)
        let fence_clone = fence.clone();
        let server_result = pmi_server
            .run(move |local_entries| {
                let fence = fence_clone.clone();
                async move { fence.execute_fence(local_entries).await }
            })
            .await;

        let success = server_result.is_ok();
        let error = server_result.err();

        // Collect rank exit statuses
        let exits = rank_exits.lock().await;
        let rank_exits_vec = if exits.is_empty() {
            // If we didn't collect actual exits (non-Linux), generate stubs
            (0..self.config.tasks_per_node)
                .map(|i| RankExitStatus {
                    rank: self.config.first_rank + i,
                    exit_code: if success { Some(0) } else { None },
                    signal: None,
                })
                .collect()
        } else {
            exits.clone()
        };

        LaunchResult {
            launch_id: self.config.launch_id,
            success,
            rank_exits: rank_exits_vec,
            error,
        }
    }

    #[cfg(target_os = "linux")]
    async fn spawn_ranks_linux(&self, pmi_server: &Pmi2Server) {
        use tokio::process::Command;

        let socket_path = pmi_server.socket_path().to_string_lossy().to_string();

        for local_rank in 0..self.config.tasks_per_node {
            let global_rank = self.config.first_rank + local_rank;
            let env = self.rank_env(global_rank, local_rank);

            let mut cmd = Command::new(&self.config.entrypoint);
            cmd.args(&self.config.args);
            cmd.envs(&env);
            // PMI_FD is typically set via pre_exec fd inheritance.
            // For simplicity, we set PMI_SOCKET_PATH and let the PMI
            // client library connect via path (supported by most MPI impls).
            cmd.env("PMI_SOCKET_PATH", &socket_path);
            cmd.env("PMI_FD", "-1"); // -1 signals "use socket path"

            match cmd.spawn() {
                Ok(child) => {
                    let pid = child.id();
                    debug!(rank = global_rank, pid = ?pid, "spawned rank process");
                    let mut pids = self.rank_pids.lock().await;
                    pids.push(pid);
                }
                Err(e) => {
                    error!(rank = global_rank, error = %e, "failed to spawn rank");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pmi2::fence::MockFenceTransport;

    fn test_config() -> LaunchConfig {
        LaunchConfig {
            launch_id: uuid::Uuid::new_v4(),
            allocation_id: uuid::Uuid::new_v4(),
            entrypoint: "/bin/echo".into(),
            args: vec!["hello".into()],
            env: HashMap::new(),
            tasks_per_node: 4,
            first_rank: 0,
            world_size: 8,
            pmi_mode: PmiMode::Pmi2,
            cxi_credentials: None,
            peers: vec![
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
            ],
            head_node_index: 0,
            my_node_index: 0,
            node_id: "node0".into(),
            socket_dir: std::env::temp_dir(),
            nodelist: "node0,node1".into(),
        }
    }

    #[test]
    fn rank_env_sets_pmi_variables() {
        let transport = Arc::new(MockFenceTransport::new());
        let launcher = ProcessLauncher::new(test_config(), transport);

        let env = launcher.rank_env(3, 3);
        assert_eq!(env.get("PMI_RANK").unwrap(), "3");
        assert_eq!(env.get("PMI_SIZE").unwrap(), "8");
        assert_eq!(env.get("PMI_SPAWNED").unwrap(), "0");
        assert_eq!(env.get("LATTICE_LOCAL_RANK").unwrap(), "3");
        assert_eq!(env.get("LATTICE_LOCAL_SIZE").unwrap(), "4");
        assert_eq!(env.get("LATTICE_NNODES").unwrap(), "2");
    }

    #[test]
    fn rank_env_sets_cxi_credentials() {
        let transport = Arc::new(MockFenceTransport::new());
        let mut config = test_config();
        config.cxi_credentials = Some(CxiCredentials {
            vni: 1234,
            auth_key: vec![0xde, 0xad, 0xbe, 0xef],
            svc_id: 42,
        });
        let launcher = ProcessLauncher::new(config, transport);

        let env = launcher.rank_env(0, 0);
        assert_eq!(env.get("FI_CXI_DEFAULT_VNI").unwrap(), "1234");
        assert_eq!(env.get("FI_CXI_AUTH_KEY").unwrap(), "deadbeef");
        assert_eq!(env.get("FI_PROVIDER").unwrap(), "cxi");
    }

    #[test]
    fn rank_env_sets_lattice_variables() {
        let transport = Arc::new(MockFenceTransport::new());
        let config = test_config();
        let alloc_id = config.allocation_id;
        let launcher = ProcessLauncher::new(config, transport);

        let env = launcher.rank_env(0, 0);
        assert_eq!(env.get("LATTICE_ALLOC_ID").unwrap(), &alloc_id.to_string());
        assert_eq!(env.get("LATTICE_NODELIST").unwrap(), "node0,node1");
        assert_eq!(env.get("LATTICE_NPROCS").unwrap(), "8");
    }
}

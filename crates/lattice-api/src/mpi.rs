//! MPI launch orchestrator.
//!
//! Computes rank layout and fans out `LaunchProcesses` RPCs to node agents.

use std::collections::HashMap;
use std::sync::Arc;

use tracing::info;
use uuid::Uuid;

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::types::{AllocId, LaunchId, NodeId, PmiMode, RankLayout};

/// Trait for communicating with node agents to launch processes.
#[async_trait::async_trait]
pub trait NodeAgentPool: Send + Sync {
    /// Send a LaunchProcesses request to a specific node agent.
    async fn launch_processes(
        &self,
        node_address: &str,
        request: pb::LaunchProcessesRequest,
    ) -> Result<pb::LaunchProcessesResponse, String>;
}

/// Real gRPC implementation of NodeAgentPool.
pub struct GrpcNodeAgentPool;

#[async_trait::async_trait]
impl NodeAgentPool for GrpcNodeAgentPool {
    async fn launch_processes(
        &self,
        node_address: &str,
        request: pb::LaunchProcessesRequest,
    ) -> Result<pb::LaunchProcessesResponse, String> {
        let mut client = pb::node_agent_service_client::NodeAgentServiceClient::connect(
            node_address.to_string(),
        )
        .await
        .map_err(|e| format!("connect to {node_address}: {e}"))?;

        let resp = client
            .launch_processes(request)
            .await
            .map_err(|e| format!("LaunchProcesses to {node_address}: {e}"))?;

        Ok(resp.into_inner())
    }
}

/// Stub NodeAgentPool for testing.
pub struct StubNodeAgentPool;

#[async_trait::async_trait]
impl NodeAgentPool for StubNodeAgentPool {
    async fn launch_processes(
        &self,
        _node_address: &str,
        _request: pb::LaunchProcessesRequest,
    ) -> Result<pb::LaunchProcessesResponse, String> {
        Ok(pb::LaunchProcessesResponse {
            accepted: true,
            message: "stub".into(),
        })
    }
}

/// Orchestrate an MPI launch across multiple nodes.
pub struct MpiLaunchOrchestrator {
    agent_pool: Arc<dyn NodeAgentPool>,
}

impl MpiLaunchOrchestrator {
    pub fn new(agent_pool: Arc<dyn NodeAgentPool>) -> Self {
        Self { agent_pool }
    }

    /// Launch MPI ranks across the given nodes.
    ///
    /// Returns the launch ID on success.
    #[allow(clippy::too_many_arguments)]
    pub async fn launch(
        &self,
        allocation_id: AllocId,
        assigned_nodes: &[NodeId],
        node_addresses: &HashMap<NodeId, String>,
        entrypoint: &str,
        args: &[String],
        env: &HashMap<String, String>,
        num_tasks: u32,
        tasks_per_node: u32,
        pmi_mode: PmiMode,
    ) -> Result<LaunchId, String> {
        let launch_id = Uuid::new_v4();

        // Compute rank layout
        let tasks_per = if tasks_per_node > 0 {
            tasks_per_node
        } else if num_tasks > 0 && !assigned_nodes.is_empty() {
            let n = assigned_nodes.len() as u32;
            num_tasks.div_ceil(n)
        } else {
            1
        };

        let layout = RankLayout::compute(assigned_nodes, tasks_per);

        info!(
            launch_id = %launch_id,
            alloc_id = %allocation_id,
            total_ranks = layout.total_ranks,
            nodes = assigned_nodes.len(),
            tasks_per_node = tasks_per,
            "orchestrating MPI launch"
        );

        // Build peer list
        let peers: Vec<pb::PeerInfo> = layout
            .node_assignments
            .iter()
            .map(|a| {
                let addr = node_addresses
                    .get(&a.node_id)
                    .cloned()
                    .unwrap_or_else(|| format!("http://{}:50052", a.node_id));
                pb::PeerInfo {
                    node_id: a.node_id.clone(),
                    grpc_address: addr,
                    first_rank: a.first_rank,
                    num_ranks: a.num_ranks,
                }
            })
            .collect();

        // Fan out to all node agents in parallel
        let mut join_set = tokio::task::JoinSet::new();

        for (idx, assignment) in layout.node_assignments.iter().enumerate() {
            let addr = node_addresses
                .get(&assignment.node_id)
                .cloned()
                .unwrap_or_else(|| format!("http://{}:50052", assignment.node_id));

            let request = pb::LaunchProcessesRequest {
                launch_id: launch_id.to_string(),
                allocation_id: allocation_id.to_string(),
                entrypoint: entrypoint.to_string(),
                args: args.to_vec(),
                tasks_per_node: assignment.num_ranks,
                first_rank: assignment.first_rank,
                world_size: layout.total_ranks,
                env: env.clone(),
                pmi_mode: match pmi_mode {
                    PmiMode::Pmi2 => pb::PmiMode::Pmi2 as i32,
                    PmiMode::Pmix => pb::PmiMode::Pmix as i32,
                },
                cxi_credentials: None, // TODO: integrate fabric manager
                peers: peers.clone(),
                head_node_index: 0,
            };

            let pool = self.agent_pool.clone();
            join_set.spawn(async move {
                let result = pool.launch_processes(&addr, request).await;
                (idx, result)
            });
        }

        // Collect results
        let mut errors = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok((idx, Ok(resp))) => {
                    if !resp.accepted {
                        errors.push(format!("node {idx} rejected: {}", resp.message));
                    }
                }
                Ok((idx, Err(e))) => {
                    errors.push(format!("node {idx}: {e}"));
                }
                Err(e) => {
                    errors.push(format!("join error: {e}"));
                }
            }
        }

        if errors.is_empty() {
            Ok(launch_id)
        } else {
            Err(format!(
                "launch failed on {} node(s): {}",
                errors.len(),
                errors.join("; ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_layout_computation() {
        let nodes = vec!["n0".into(), "n1".into(), "n2".into(), "n3".into()];
        let layout = RankLayout::compute(&nodes, 4);
        assert_eq!(layout.total_ranks, 16);
        assert_eq!(layout.node_assignments.len(), 4);
        assert_eq!(layout.node_assignments[0].first_rank, 0);
        assert_eq!(layout.node_assignments[0].num_ranks, 4);
        assert_eq!(layout.node_assignments[1].first_rank, 4);
        assert_eq!(layout.node_assignments[3].first_rank, 12);
    }

    #[tokio::test]
    async fn stub_launch_succeeds() {
        let pool = Arc::new(StubNodeAgentPool);
        let orch = MpiLaunchOrchestrator::new(pool);

        let mut addrs = HashMap::new();
        addrs.insert("n0".to_string(), "http://n0:50052".to_string());
        addrs.insert("n1".to_string(), "http://n1:50052".to_string());

        let result = orch
            .launch(
                Uuid::new_v4(),
                &["n0".into(), "n1".into()],
                &addrs,
                "./my_mpi_app",
                &[],
                &HashMap::new(),
                8,
                4,
                PmiMode::Pmi2,
            )
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn tasks_per_node_auto_computed() {
        let pool = Arc::new(StubNodeAgentPool);
        let orch = MpiLaunchOrchestrator::new(pool);

        let mut addrs = HashMap::new();
        addrs.insert("n0".to_string(), "http://n0:50052".to_string());

        // 10 tasks on 1 node → tasks_per_node=0, auto=10
        let result = orch
            .launch(
                Uuid::new_v4(),
                &["n0".into()],
                &addrs,
                "echo",
                &[],
                &HashMap::new(),
                10,
                0,
                PmiMode::Pmi2,
            )
            .await;

        assert!(result.is_ok());
    }
}

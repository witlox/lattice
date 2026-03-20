//! Scale executor — translates autoscaler decisions into infrastructure actions.
//!
//! When the autoscaler decides to scale up, the executor boots new nodes via
//! OpenCHAMI. When scaling down, it drains and releases idle nodes.

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::types::NodeId;

/// Executes scaling decisions from the autoscaler.
///
/// Scale-up provisions new nodes through the infrastructure service.
/// Scale-down drains idle nodes and returns them to the pool.
#[async_trait]
pub trait ScaleExecutor: Send + Sync {
    /// Provision `count` new nodes for the vCluster.
    /// Returns the IDs of nodes that were successfully booted.
    async fn scale_up(&self, count: u32) -> Result<Vec<NodeId>, LatticeError>;

    /// Release `count` idle nodes from the vCluster.
    /// Returns the IDs of nodes that were successfully drained and released.
    async fn scale_down(&self, count: u32) -> Result<Vec<NodeId>, LatticeError>;
}

/// Scale executor backed by OpenCHAMI infrastructure + a node pool.
///
/// Scale-up: picks unassigned nodes from the standby pool, boots them via
/// OpenCHAMI, and registers them as Ready.
///
/// Scale-down: picks idle Ready nodes, drains them, and returns them to
/// the standby pool.
pub struct InfraScaleExecutor<I, N> {
    infra: I,
    nodes: N,
    /// OS image to boot new nodes with.
    boot_image: String,
}

impl<I, N> InfraScaleExecutor<I, N>
where
    I: lattice_common::traits::InfrastructureService,
    N: lattice_common::traits::NodeRegistry,
{
    pub fn new(infra: I, nodes: N, boot_image: String) -> Self {
        Self {
            infra,
            nodes,
            boot_image,
        }
    }
}

#[async_trait]
impl<I, N> ScaleExecutor for InfraScaleExecutor<I, N>
where
    I: lattice_common::traits::InfrastructureService,
    N: lattice_common::traits::NodeRegistry,
{
    async fn scale_up(&self, count: u32) -> Result<Vec<NodeId>, LatticeError> {
        // Find standby/down nodes that can be booted
        let filter = lattice_common::traits::NodeFilter {
            state: Some(lattice_common::types::NodeState::Drained),
            ..Default::default()
        };
        let candidates = self.nodes.list_nodes(&filter).await?;

        let mut booted = Vec::new();
        for node in candidates.iter().take(count as usize) {
            match self.infra.boot_node(&node.id, &self.boot_image).await {
                Ok(()) => {
                    tracing::info!(node_id = %node.id, "Scale-up: booted node");
                    booted.push(node.id.clone());
                }
                Err(e) => {
                    tracing::warn!(node_id = %node.id, error = %e, "Scale-up: failed to boot node");
                }
            }
        }

        Ok(booted)
    }

    async fn scale_down(&self, count: u32) -> Result<Vec<NodeId>, LatticeError> {
        // Find idle Ready nodes without an owner
        let filter = lattice_common::traits::NodeFilter {
            state: Some(lattice_common::types::NodeState::Ready),
            ..Default::default()
        };
        let candidates = self.nodes.list_nodes(&filter).await?;

        let idle: Vec<_> = candidates
            .iter()
            .filter(|n| n.owner.is_none())
            .take(count as usize)
            .collect();

        let mut drained = Vec::new();
        for node in &idle {
            match self
                .nodes
                .update_node_state(&node.id, lattice_common::types::NodeState::Draining)
                .await
            {
                Ok(()) => {
                    tracing::info!(node_id = %node.id, "Scale-down: draining node");
                    drained.push(node.id.clone());
                }
                Err(e) => {
                    tracing::warn!(node_id = %node.id, error = %e, "Scale-down: failed to drain node");
                }
            }
        }

        Ok(drained)
    }
}

/// No-op scale executor for testing or when infrastructure management is disabled.
pub struct NoopScaleExecutor;

#[async_trait]
impl ScaleExecutor for NoopScaleExecutor {
    async fn scale_up(&self, _count: u32) -> Result<Vec<NodeId>, LatticeError> {
        Ok(vec![])
    }

    async fn scale_down(&self, _count: u32) -> Result<Vec<NodeId>, LatticeError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_executor_returns_empty() {
        let executor = NoopScaleExecutor;
        assert!(executor.scale_up(5).await.unwrap().is_empty());
        assert!(executor.scale_down(3).await.unwrap().is_empty());
    }
}

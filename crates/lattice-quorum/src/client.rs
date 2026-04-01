//! QuorumClient — proposes commands to the Raft leader and waits for commit.

use std::sync::Arc;

use lattice_common::error::LatticeError;
use lattice_common::traits::{
    AllocationFilter, AllocationStore, AuditArchiveInfo, AuditEntry, AuditFilter, AuditLog,
    NodeFilter, NodeRegistry,
};
use lattice_common::types::{
    AllocId, Allocation, AllocationState, Node, NodeId, NodeOwnership, NodeState, TenantId,
};
use openraft::Raft;
use tokio::sync::RwLock;
use tracing::debug;

use crate::commands::{Command, CommandResponse};
use crate::global_state::GlobalState;
use crate::TypeConfig;

/// Client for interacting with the quorum.
///
/// Proposes commands to the Raft leader and reads from local state.
#[derive(Clone)]
pub struct QuorumClient {
    raft: Raft<TypeConfig>,
    state: Arc<RwLock<GlobalState>>,
}

impl QuorumClient {
    pub fn new(raft: Raft<TypeConfig>, state: Arc<RwLock<GlobalState>>) -> Self {
        Self { raft, state }
    }

    /// Propose a command to the Raft leader. Blocks until committed.
    pub async fn propose(&self, cmd: Command) -> Result<CommandResponse, LatticeError> {
        debug!("Proposing command: {:?}", cmd);

        let result = self
            .raft
            .client_write(cmd)
            .await
            .map_err(|e| LatticeError::QuorumError(e.to_string()))?;

        Ok(result.response().clone())
    }

    /// Get the underlying Raft handle.
    pub fn raft(&self) -> &Raft<TypeConfig> {
        &self.raft
    }

    /// Get a read handle to the state.
    pub fn state(&self) -> &Arc<RwLock<GlobalState>> {
        &self.state
    }

    /// Resolve a deferred image reference on an allocation (INV-SD4).
    pub async fn resolve_image(
        &self,
        id: &lattice_common::types::AllocId,
        image_index: usize,
        resolved: lattice_common::types::ImageRef,
    ) -> Result<(), LatticeError> {
        let resp = self
            .propose(Command::ResolveImage {
                id: *id,
                image_index,
                resolved,
            })
            .await?;

        match resp {
            CommandResponse::Ok => Ok(()),
            CommandResponse::Error(e) => Err(LatticeError::Internal(e)),
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }
}

#[async_trait::async_trait]
impl NodeRegistry for QuorumClient {
    async fn get_node(&self, id: &NodeId) -> Result<Node, LatticeError> {
        let state = self.state.read().await;
        state
            .nodes
            .get(id)
            .cloned()
            .ok_or_else(|| LatticeError::NodeNotFound(id.clone()))
    }

    async fn list_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, LatticeError> {
        let state = self.state.read().await;
        let nodes = state
            .nodes
            .values()
            .filter(|n| {
                if let Some(ref s) = filter.state {
                    if &n.state != s {
                        return false;
                    }
                }
                if let Some(ref g) = filter.group {
                    if &n.group != g {
                        return false;
                    }
                }
                if let Some(ref t) = filter.tenant {
                    if let Some(ref owner) = n.owner {
                        if &owner.tenant != t {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        Ok(nodes)
    }

    async fn update_node_state(&self, id: &NodeId, state: NodeState) -> Result<(), LatticeError> {
        let resp = self
            .propose(Command::UpdateNodeState {
                id: id.clone(),
                state,
                reason: None,
            })
            .await?;

        match resp {
            CommandResponse::Ok => Ok(()),
            CommandResponse::Error(e) => Err(LatticeError::Internal(e)),
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }

    async fn claim_node(&self, id: &NodeId, ownership: NodeOwnership) -> Result<(), LatticeError> {
        let resp = self
            .propose(Command::ClaimNode {
                id: id.clone(),
                ownership,
            })
            .await?;

        match resp {
            CommandResponse::Ok => Ok(()),
            CommandResponse::Error(e) => {
                if e.contains("already claimed") {
                    Err(LatticeError::OwnershipConflict {
                        node: id.clone(),
                        owner: e,
                    })
                } else {
                    Err(LatticeError::Internal(e))
                }
            }
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }

    async fn release_node(&self, id: &NodeId) -> Result<(), LatticeError> {
        let resp = self
            .propose(Command::ReleaseNode { id: id.clone() })
            .await?;

        match resp {
            CommandResponse::Ok => Ok(()),
            CommandResponse::Error(e) => Err(LatticeError::Internal(e)),
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }
}

#[async_trait::async_trait]
impl AllocationStore for QuorumClient {
    async fn insert(&self, allocation: Allocation) -> Result<(), LatticeError> {
        let resp = self.propose(Command::SubmitAllocation(allocation)).await?;

        match resp {
            CommandResponse::AllocationId(_) => Ok(()),
            CommandResponse::Error(e) => {
                if e.contains("max_nodes") || e.contains("max_concurrent") {
                    // Parse tenant from error
                    Err(LatticeError::QuotaExceeded {
                        tenant: "unknown".into(),
                        detail: e,
                    })
                } else {
                    Err(LatticeError::Internal(e))
                }
            }
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }

    async fn get(&self, id: &AllocId) -> Result<Allocation, LatticeError> {
        let state = self.state.read().await;
        state
            .allocations
            .get(id)
            .cloned()
            .ok_or_else(|| LatticeError::AllocationNotFound(id.to_string()))
    }

    async fn update_state(&self, id: &AllocId, state: AllocationState) -> Result<(), LatticeError> {
        let resp = self
            .propose(Command::UpdateAllocationState {
                id: *id,
                state,
                message: None,
                exit_code: None,
            })
            .await?;

        match resp {
            CommandResponse::Ok => Ok(()),
            CommandResponse::Error(e) => Err(LatticeError::Internal(e)),
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }

    async fn list(&self, filter: &AllocationFilter) -> Result<Vec<Allocation>, LatticeError> {
        let state = self.state.read().await;
        let allocs = state
            .allocations
            .values()
            .filter(|a| {
                if let Some(ref u) = filter.user {
                    if &a.user != u {
                        return false;
                    }
                }
                if let Some(ref t) = filter.tenant {
                    if &a.tenant != t {
                        return false;
                    }
                }
                if let Some(ref s) = filter.state {
                    if &a.state != s {
                        return false;
                    }
                }
                if let Some(ref vc) = filter.vcluster {
                    if &a.vcluster != vc {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        Ok(allocs)
    }

    async fn count_running(&self, tenant: &TenantId) -> Result<u32, LatticeError> {
        let state = self.state.read().await;
        let count = state
            .allocations
            .values()
            .filter(|a| &a.tenant == tenant && a.state == AllocationState::Running)
            .count() as u32;
        Ok(count)
    }
}

#[async_trait::async_trait]
impl AuditLog for QuorumClient {
    async fn record(&self, entry: AuditEntry) -> Result<(), LatticeError> {
        let resp = self.propose(Command::RecordAudit(entry)).await?;
        match resp {
            CommandResponse::Ok => Ok(()),
            CommandResponse::Error(e) => Err(LatticeError::Internal(e)),
            _ => Err(LatticeError::Internal("Unexpected response".into())),
        }
    }

    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>, LatticeError> {
        let state = self.state.read().await;
        Ok(state.query_audit(filter))
    }

    async fn archive_info(&self) -> Result<Vec<AuditArchiveInfo>, LatticeError> {
        let state = self.state.read().await;
        Ok(state
            .archive_summary()
            .iter()
            .map(|a| AuditArchiveInfo {
                object_key: a.object_key.clone(),
                entry_count: a.entry_count,
                first_timestamp: a.first_timestamp,
                last_timestamp: a.last_timestamp,
            })
            .collect())
    }

    async fn total_entry_count(&self) -> Result<usize, LatticeError> {
        let state = self.state.read().await;
        Ok(state.total_audit_entry_count())
    }
}

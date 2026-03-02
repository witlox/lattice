//! State machine commands — all mutations that go through Raft consensus.

use chrono::{DateTime, Utc};
use lattice_common::traits::AuditEntry;
use lattice_common::types::{
    AllocId, Allocation, AllocationState, CostWeights, IsolationLevel, Node, NodeId, NodeOwnership,
    NodeState, Tenant, TenantId, TenantQuota, TopologyModel, VCluster, VClusterId,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A command to be applied to the Raft state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum Command {
    // ── Allocation commands ─────────────────────────────────
    SubmitAllocation(Allocation),
    UpdateAllocationState {
        id: AllocId,
        state: AllocationState,
        message: Option<String>,
        exit_code: Option<i32>,
    },
    AssignNodes {
        id: AllocId,
        nodes: Vec<NodeId>,
    },

    // ── Node commands ───────────────────────────────────────
    RegisterNode(Node),
    UpdateNodeState {
        id: NodeId,
        state: NodeState,
        reason: Option<String>,
    },
    ClaimNode {
        id: NodeId,
        ownership: NodeOwnership,
    },
    ReleaseNode {
        id: NodeId,
    },
    RecordHeartbeat {
        id: NodeId,
        timestamp: DateTime<Utc>,
    },

    // ── Tenant commands ─────────────────────────────────────
    CreateTenant(Tenant),
    UpdateTenant {
        id: TenantId,
        quota: Option<TenantQuota>,
        isolation_level: Option<IsolationLevel>,
    },

    // ── VCluster commands ───────────────────────────────────
    CreateVCluster(VCluster),
    UpdateVCluster {
        id: VClusterId,
        cost_weights: Option<CostWeights>,
        allow_borrowing: Option<bool>,
        allow_lending: Option<bool>,
    },

    // ── Topology commands ───────────────────────────────────
    UpdateTopology(TopologyModel),

    // ── Audit commands ──────────────────────────────────────
    RecordAudit(AuditEntry),
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::SubmitAllocation(a) => write!(f, "SubmitAllocation({})", a.id),
            Command::UpdateAllocationState { id, state, .. } => {
                write!(f, "UpdateAllocationState({id}, {state:?})")
            }
            Command::AssignNodes { id, nodes } => {
                write!(f, "AssignNodes({id}, {} nodes)", nodes.len())
            }
            Command::RegisterNode(n) => write!(f, "RegisterNode({})", n.id),
            Command::UpdateNodeState { id, state, .. } => {
                write!(f, "UpdateNodeState({id}, {state:?})")
            }
            Command::ClaimNode { id, .. } => write!(f, "ClaimNode({id})"),
            Command::ReleaseNode { id } => write!(f, "ReleaseNode({id})"),
            Command::RecordHeartbeat { id, .. } => write!(f, "RecordHeartbeat({id})"),
            Command::CreateTenant(t) => write!(f, "CreateTenant({})", t.id),
            Command::UpdateTenant { id, .. } => write!(f, "UpdateTenant({id})"),
            Command::CreateVCluster(vc) => write!(f, "CreateVCluster({})", vc.id),
            Command::UpdateVCluster { id, .. } => write!(f, "UpdateVCluster({id})"),
            Command::UpdateTopology(_) => write!(f, "UpdateTopology"),
            Command::RecordAudit(e) => write!(f, "RecordAudit({:?})", e.action),
        }
    }
}

/// Response from applying a command to the state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandResponse {
    Ok,
    AllocationId(AllocId),
    NodeId(NodeId),
    TenantId(TenantId),
    VClusterId(VClusterId),
    Error(String),
}

impl fmt::Display for CommandResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandResponse::Ok => write!(f, "Ok"),
            CommandResponse::AllocationId(id) => write!(f, "AllocationId({id})"),
            CommandResponse::NodeId(id) => write!(f, "NodeId({id})"),
            CommandResponse::TenantId(id) => write!(f, "TenantId({id})"),
            CommandResponse::VClusterId(id) => write!(f, "VClusterId({id})"),
            CommandResponse::Error(e) => write!(f, "Error({e})"),
        }
    }
}

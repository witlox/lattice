//! Trait implementations connecting lattice types to hpc-scheduler-core.
//!
//! This module is gated behind the `scheduler-core` feature flag and
//! implements [`hpc_scheduler_core::Job`] for [`Allocation`] and
//! [`hpc_scheduler_core::ComputeNode`] for [`Node`].

use chrono::{DateTime, Utc};
use uuid::Uuid;

use hpc_scheduler_core::traits::{ComputeNode, Job};
use hpc_scheduler_core::types::{
    CheckpointKind, MemoryDomainInfo, MemoryDomainKind, MemoryInterconnectInfo, MemoryLinkKind,
    MemoryTopologyInfo, NodeConstraints, TopologyPreference,
};

use crate::types::*;

// ─── Job for Allocation ──────────────────────────────────────

impl Job for Allocation {
    fn id(&self) -> Uuid {
        self.id
    }

    fn tenant_id(&self) -> &str {
        &self.tenant
    }

    fn node_count_min(&self) -> u32 {
        match self.resources.nodes {
            NodeCount::Exact(n) => n,
            NodeCount::Range { min, .. } => min,
        }
    }

    fn node_count_max(&self) -> Option<u32> {
        match self.resources.nodes {
            NodeCount::Exact(_) => None,
            NodeCount::Range { max, .. } => Some(max),
        }
    }

    fn walltime(&self) -> Option<chrono::Duration> {
        match &self.lifecycle.lifecycle_type {
            LifecycleType::Bounded { walltime } => Some(*walltime),
            LifecycleType::Unbounded | LifecycleType::Reactive { .. } => None,
        }
    }

    fn preemption_class(&self) -> u8 {
        self.lifecycle.preemption_class
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn started_at(&self) -> Option<DateTime<Utc>> {
        self.started_at
    }

    fn assigned_nodes(&self) -> &[String] {
        &self.assigned_nodes
    }

    fn checkpoint_kind(&self) -> CheckpointKind {
        match self.checkpoint {
            CheckpointStrategy::Auto => CheckpointKind::Auto,
            CheckpointStrategy::Manual => CheckpointKind::Manual,
            CheckpointStrategy::None => CheckpointKind::None,
        }
    }

    fn is_running(&self) -> bool {
        self.state == AllocationState::Running
    }

    fn is_sensitive(&self) -> bool {
        self.lifecycle.preemption_class >= 10
            || self
                .tags
                .get("workload_class")
                .is_some_and(|v| v == "sensitive")
    }

    fn prefer_same_numa(&self) -> bool {
        self.resources.constraints.prefer_same_numa
    }

    fn topology_preference(&self) -> Option<TopologyPreference> {
        self.resources
            .constraints
            .topology
            .as_ref()
            .map(|h| match h {
                TopologyHint::Tight => TopologyPreference::Tight,
                TopologyHint::Spread => TopologyPreference::Spread,
                TopologyHint::Any => TopologyPreference::Any,
            })
    }

    fn constraints(&self) -> NodeConstraints {
        NodeConstraints {
            gpu_type: self.resources.constraints.gpu_type.clone(),
            features: self.resources.constraints.features.clone(),
            require_unified_memory: self.resources.constraints.require_unified_memory,
            allow_cxl_memory: self.resources.constraints.allow_cxl_memory,
        }
    }
}

// ─── ComputeNode for Node ────────────────────────────────────

impl ComputeNode for Node {
    fn id(&self) -> &str {
        &self.id
    }

    fn group(&self) -> u32 {
        self.group
    }

    fn is_available(&self) -> bool {
        self.state.is_operational() && self.owner.is_none()
    }

    fn conformance_fingerprint(&self) -> Option<&str> {
        self.conformance_fingerprint.as_deref()
    }

    fn gpu_type(&self) -> Option<&str> {
        self.capabilities.gpu_type.as_deref()
    }

    fn features(&self) -> &[String] {
        &self.capabilities.features
    }

    fn cpu_cores(&self) -> u32 {
        self.capabilities.cpu_cores
    }

    fn gpu_count(&self) -> u32 {
        self.capabilities.gpu_count
    }

    fn memory_topology(&self) -> Option<MemoryTopologyInfo> {
        self.capabilities
            .memory_topology
            .as_ref()
            .map(|topo| MemoryTopologyInfo {
                domains: topo
                    .domains
                    .iter()
                    .map(|d| MemoryDomainInfo {
                        id: d.id,
                        domain_type: match d.domain_type {
                            MemoryDomainType::Dram => MemoryDomainKind::Dram,
                            MemoryDomainType::Hbm => MemoryDomainKind::Hbm,
                            MemoryDomainType::CxlAttached => MemoryDomainKind::CxlAttached,
                            MemoryDomainType::Unified => MemoryDomainKind::Unified,
                        },
                        capacity_bytes: d.capacity_bytes,
                        numa_node: d.numa_node,
                        attached_cpus: d.attached_cpus.clone(),
                        attached_gpus: d.attached_gpus.clone(),
                    })
                    .collect(),
                interconnects: topo
                    .interconnects
                    .iter()
                    .map(|i| MemoryInterconnectInfo {
                        domain_a: i.domain_a,
                        domain_b: i.domain_b,
                        link_type: match i.link_type {
                            MemoryLinkType::NumaLink => MemoryLinkKind::NumaLink,
                            MemoryLinkType::CxlSwitch => MemoryLinkKind::CxlSwitch,
                            MemoryLinkType::CoherentFabric => MemoryLinkKind::CoherentFabric,
                        },
                        bandwidth_gbps: i.bandwidth_gbps,
                        latency_ns: i.latency_ns,
                    })
                    .collect(),
                total_capacity_bytes: topo.total_capacity_bytes,
            })
    }
}

// ─── Type conversions ────────────────────────────────────────

/// Convert lattice CostWeights to hpc-scheduler-core CostWeights.
pub fn to_core_weights(w: &CostWeights) -> hpc_scheduler_core::CostWeights {
    hpc_scheduler_core::CostWeights {
        priority: w.priority,
        wait_time: w.wait_time,
        fair_share: w.fair_share,
        topology: w.topology,
        data_readiness: w.data_readiness,
        backlog: w.backlog,
        energy: w.energy,
        checkpoint_efficiency: w.checkpoint_efficiency,
        conformance: w.conformance,
    }
}

/// Convert lattice TopologyModel to hpc-scheduler-core TopologyModel.
pub fn to_core_topology(t: &crate::types::TopologyModel) -> hpc_scheduler_core::TopologyModel {
    hpc_scheduler_core::TopologyModel {
        groups: t
            .groups
            .iter()
            .map(|g| hpc_scheduler_core::TopologyGroup {
                id: g.id,
                nodes: g.nodes.clone(),
                adjacent_groups: g.adjacent_groups.clone(),
            })
            .collect(),
    }
}

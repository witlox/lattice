//! Shared types for HPC scheduling algorithms.

/// Checkpoint behavior for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CheckpointKind {
    /// Scheduler-coordinated automatic checkpoint.
    Auto,
    /// Application-managed checkpoint.
    Manual,
    /// No checkpoint support.
    None,
}

/// Topology placement preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TopologyPreference {
    /// Pack into fewest topology groups (best for latency-sensitive HPC).
    Tight,
    /// Spread across groups (best for bisection bandwidth).
    Spread,
    /// No preference.
    Any,
}

/// Intra-node memory topology: NUMA domains, CXL tiers, unified memory.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemoryTopologyInfo {
    pub domains: Vec<MemoryDomainInfo>,
    pub interconnects: Vec<MemoryInterconnectInfo>,
    pub total_capacity_bytes: u64,
}

/// A memory domain (NUMA node, HBM bank, CXL-attached pool, or unified).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemoryDomainInfo {
    pub id: u32,
    pub domain_type: MemoryDomainKind,
    pub capacity_bytes: u64,
    pub numa_node: Option<u32>,
    pub attached_cpus: Vec<u32>,
    pub attached_gpus: Vec<u32>,
}

/// Type of memory in a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MemoryDomainKind {
    Dram,
    Hbm,
    CxlAttached,
    Unified,
}

/// Interconnect link between two memory domains.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemoryInterconnectInfo {
    pub domain_a: u32,
    pub domain_b: u32,
    pub link_type: MemoryLinkKind,
    pub bandwidth_gbps: f64,
    pub latency_ns: u64,
}

/// Type of link between memory domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MemoryLinkKind {
    NumaLink,
    CxlSwitch,
    CoherentFabric,
}

/// Cluster topology model: groups of nodes with adjacency information.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TopologyModel {
    pub groups: Vec<TopologyGroup>,
}

/// A topology group (e.g. dragonfly group, switch domain).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TopologyGroup {
    pub id: u32,
    pub nodes: Vec<String>,
    /// Adjacent groups (lower inter-group hop cost).
    pub adjacent_groups: Vec<u32>,
}

/// Weights for the composite cost function. Weights are relative;
/// normalization is optional — the solver uses them as-is.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CostWeights {
    pub priority: f64,
    pub wait_time: f64,
    pub fair_share: f64,
    pub topology: f64,
    pub data_readiness: f64,
    pub backlog: f64,
    pub energy: f64,
    pub checkpoint_efficiency: f64,
    pub conformance: f64,
}

impl Default for CostWeights {
    fn default() -> Self {
        Self {
            priority: 0.20,
            wait_time: 0.20,
            fair_share: 0.20,
            topology: 0.15,
            data_readiness: 0.10,
            backlog: 0.05,
            energy: 0.00,
            checkpoint_efficiency: 0.00,
            conformance: 0.10,
        }
    }
}

/// Hardware constraints for node selection.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NodeConstraints {
    pub gpu_type: Option<String>,
    pub features: Vec<String>,
    pub require_unified_memory: bool,
    pub allow_cxl_memory: bool,
}

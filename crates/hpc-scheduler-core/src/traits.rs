//! Trait abstractions for HPC scheduling.
//!
//! Implement [`Job`] for your workload type and [`ComputeNode`] for your
//! node type to use the scheduling algorithms in this crate.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::{CheckpointKind, MemoryTopologyInfo, NodeConstraints, TopologyPreference};

/// A schedulable work unit (job, allocation, pod, etc.).
///
/// Implementing this trait allows your workload type to be scored, placed,
/// preempted, and tracked by the scheduling algorithms in this crate.
pub trait Job {
    /// Unique identifier.
    fn id(&self) -> Uuid;
    /// Tenant/account that owns this job.
    fn tenant_id(&self) -> &str;
    /// Minimum number of nodes required.
    fn node_count_min(&self) -> u32;
    /// Maximum number of nodes (for elastic jobs); `None` for fixed-size.
    fn node_count_max(&self) -> Option<u32>;
    /// Wall-clock time limit; `None` for unbounded/service jobs.
    fn walltime(&self) -> Option<chrono::Duration>;
    /// Preemption priority class (0 = lowest, higher = harder to preempt).
    fn preemption_class(&self) -> u8;
    /// When the job was submitted.
    fn created_at(&self) -> DateTime<Utc>;
    /// When the job started running; `None` if not yet started.
    fn started_at(&self) -> Option<DateTime<Utc>>;
    /// Nodes currently assigned to this job.
    fn assigned_nodes(&self) -> &[String];
    /// Checkpoint strategy.
    fn checkpoint_kind(&self) -> CheckpointKind;
    /// Whether the job is currently running.
    fn is_running(&self) -> bool;
    /// Whether the job requires sensitive/isolated treatment (never preempted).
    fn is_sensitive(&self) -> bool;
    /// Whether the job prefers NUMA-local memory placement.
    fn prefer_same_numa(&self) -> bool;
    /// Topology placement preference (tight/spread/any).
    fn topology_preference(&self) -> Option<TopologyPreference>;
    /// Hardware constraints for node filtering.
    fn constraints(&self) -> NodeConstraints;
}

/// A compute node in the cluster.
///
/// Implementing this trait allows your node type to participate in
/// topology-aware placement, conformance grouping, and constraint filtering.
pub trait ComputeNode {
    /// Unique node identifier (e.g. xname).
    fn id(&self) -> &str;
    /// Topology group index (dragonfly group, switch domain, etc.).
    fn group(&self) -> u32;
    /// Whether this node is available for scheduling (operational + unowned).
    fn is_available(&self) -> bool;
    /// Conformance fingerprint (hash of OS, kernel, driver versions).
    fn conformance_fingerprint(&self) -> Option<&str>;
    /// GPU type string (e.g. "GH200", "MI300X").
    fn gpu_type(&self) -> Option<&str>;
    /// Feature tags supported by this node.
    fn features(&self) -> &[String];
    /// Number of CPU cores.
    fn cpu_cores(&self) -> u32;
    /// Number of GPUs.
    fn gpu_count(&self) -> u32;
    /// Memory topology information; `None` if unknown.
    fn memory_topology(&self) -> Option<MemoryTopologyInfo>;
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ─── Identifiers ────────────────────────────────────────────

pub type AllocId = Uuid;
pub type NodeId = String; // xname-style: x1000c0s0b0n0
pub type TenantId = String;
pub type VClusterId = String;
pub type GroupId = u32; // dragonfly group index
pub type UserId = String; // OIDC subject

// ─── Allocation ─────────────────────────────────────────────

/// The universal work unit. Replaces both Slurm jobs and K8s pods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub id: AllocId,
    pub tenant: TenantId,
    pub project: String,
    pub vcluster: VClusterId,
    pub user: UserId,
    pub tags: HashMap<String, String>,

    // Type (single or task group)
    pub allocation_type: AllocationType,

    // What to run
    pub environment: Environment,
    pub entrypoint: String,

    // Resources
    pub resources: ResourceRequest,

    // Lifecycle
    pub lifecycle: Lifecycle,

    // Requeue
    pub requeue_policy: RequeuePolicy,
    pub max_requeue: u32,

    // Data
    pub data: DataRequirements,

    // Networking
    pub connectivity: Connectivity,

    // Dependencies
    pub depends_on: Vec<Dependency>,

    // Checkpointing
    pub checkpoint: CheckpointStrategy,

    // Telemetry
    pub telemetry_mode: TelemetryMode,

    // State (managed by scheduler, not set by user)
    pub state: AllocationState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub assigned_nodes: Vec<NodeId>,

    /// DAG this allocation belongs to (set by scheduler on DAG submission)
    pub dag_id: Option<String>,
    /// Process exit code (set on completion/failure)
    pub exit_code: Option<i32>,
    /// Human-readable status message (failure reason, cancellation note, etc.)
    pub message: Option<String>,
    /// Number of times this allocation has been requeued
    pub requeue_count: u32,
    /// Number of times this allocation has been preempted
    pub preempted_count: u32,
    /// Whether to resume from checkpoint on next scheduling
    pub resume_from_checkpoint: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationState {
    /// Submitted, waiting in queue
    Pending,
    /// Data being staged
    Staging,
    /// Running on assigned nodes
    Running,
    /// Checkpoint in progress (preemption pending)
    Checkpointing,
    /// Suspended (checkpointed, waiting for resources)
    Suspended,
    /// Completed successfully
    Completed,
    /// Failed
    Failed,
    /// Cancelled by user
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AllocationType {
    /// Single allocation
    Single,
    /// Task group (job array equivalent)
    TaskGroup {
        range_start: u32,
        range_end: u32,
        step: u32,
        max_concurrent: u32,
    },
}

// ─── Requeue Policy ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequeuePolicy {
    /// Allocation fails permanently on any node failure.
    Never,
    /// Requeue only on node-side failures (hardware, agent crash, partition).
    OnNodeFailure,
    /// Requeue on any failure including application crash.
    Always,
}

// ─── Environment ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    /// uenv name/version (e.g., "prgenv-gnu/24.11:v1")
    pub uenv: Option<String>,
    /// uenv view to activate (e.g., "default", "spack")
    pub view: Option<String>,
    /// OCI image (alternative to uenv, for Sarus)
    pub image: Option<String>,
    /// Additional uenv for tools (mounted at /user-tools)
    pub tools_uenv: Option<String>,
    /// For sensitive: require signed images
    pub sign_required: bool,
    /// Require vulnerability scan before scheduling (sensitive-workloads)
    pub scan_required: bool,
    /// Restrict to approved base images only (sensitive-workloads)
    pub approved_bases_only: bool,
}

// ─── Resources ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequest {
    /// Number of nodes (exact or range)
    pub nodes: NodeCount,
    /// Hardware constraints
    pub constraints: ResourceConstraints,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeCount {
    Exact(u32),
    /// Elastic node range. Invariants: min >= 1, max >= min.
    Range {
        min: u32,
        max: u32,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceConstraints {
    pub gpu_type: Option<String>,
    pub features: Vec<String>,
    /// Topology hint: "tight" (fewest groups), "spread" (max bandwidth), "any"
    pub topology: Option<TopologyHint>,
    /// Feature count requirements (e.g., at least 4 nodes with "nvme_scratch")
    pub feature_counts: HashMap<String, u32>,
    /// Require nodes with unified memory (GH200/MI300A)
    #[serde(default)]
    pub require_unified_memory: bool,
    /// Prefer allocating CPUs+GPUs within the same NUMA domain
    #[serde(default)]
    pub prefer_same_numa: bool,
    /// Allow CXL-attached memory to satisfy capacity requirements
    #[serde(default)]
    pub allow_cxl_memory: bool,
    /// Memory binding policy (numactl)
    pub memory_policy: Option<MemoryPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TopologyHint {
    Tight,  // pack into fewest dragonfly groups
    Spread, // spread across groups for max bisection bandwidth
    Any,    // no preference
}

// ─── Lifecycle ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lifecycle {
    pub lifecycle_type: LifecycleType,
    pub preemption_class: u8, // 0 = lowest priority, higher = harder to preempt
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LifecycleType {
    /// Traditional batch job with walltime
    Bounded { walltime: chrono::Duration },
    /// Long-running service (inference endpoint, monitoring)
    Unbounded,
    /// Autoscaling based on metrics
    Reactive {
        min_nodes: u32,
        max_nodes: u32,
        metric: String,
        target: String,
    },
}

// ─── Data ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataRequirements {
    pub mounts: Vec<DataMount>,
    /// Use sane defaults (home dir, scratch, output dir)
    pub use_defaults: bool,
    /// Per-node scratch size hint
    pub scratch_per_node: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataMount {
    pub source: String, // s3://... or nfs://...
    pub target: String, // mount point inside allocation
    pub access: DataAccess,
    pub tier_hint: Option<StorageTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StorageTier {
    Hot,
    Warm,
    Cold,
}

// ─── Connectivity ───────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Connectivity {
    /// Shared network domain name — allocations in the same domain can communicate
    pub network_domain: Option<String>,
    /// Exposed endpoints (for services)
    pub expose: Vec<ServiceEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    pub name: String,
    pub port: u16,
    pub protocol: Option<String>,
}

// ─── Network Domain ────────────────────────────────────────

/// A network domain groups allocations that need L3 reachability via Slingshot VNI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkDomain {
    pub name: String,
    pub tenant: TenantId,
    /// Assigned Slingshot VNI
    pub vni: u32,
    pub state: NetworkDomainState,
    /// Allocation IDs currently in this domain
    pub member_allocations: Vec<AllocId>,
    pub created_at: DateTime<Utc>,
    /// Grace deadline after last member completes (for DAG domain persistence)
    pub grace_deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkDomainState {
    /// Active, accepting new members
    Active,
    /// Last member completed, grace timer running
    Draining,
    /// VNI released back to pool
    Released,
}

// ─── Dependencies ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// Reference to another allocation (by ID or name)
    pub ref_id: String,
    /// Condition for dependency satisfaction
    pub condition: DependencyCondition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DependencyCondition {
    /// afterok — only if dependency succeeded
    Success,
    /// afternotok — only if dependency failed
    Failure,
    /// afterany — regardless of outcome
    Any,
    /// aftercorr — corresponding task in task group
    Corresponding,
    /// singleton — only one allocation with this name runs at a time
    Mutex,
}

// ─── Checkpointing ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CheckpointStrategy {
    /// Scheduler decides based on cost function
    Auto,
    /// Application manages its own checkpointing
    Manual,
    /// Non-checkpointable (treated as non-preemptible)
    None,
}

// ─── Telemetry ──────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum TelemetryMode {
    /// 30s aggregation, bicubic smoothing, low overhead
    #[default]
    Prod,
    /// 1s raw streams, full profiling, higher overhead
    Debug { duration_seconds: u64 },
    /// Access logging for compliance, moderate overhead
    Audit,
}

// ─── Observability: Attach ─────────────────────────────────

/// An active interactive terminal session attached to a running allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachSession {
    pub session_id: Uuid,
    pub allocation_id: AllocId,
    pub node_id: NodeId,
    pub user: UserId,
    pub command: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

// ─── Observability: Logs ───────────────────────────────────

/// A single log entry from an allocation's stdout/stderr.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub node_id: NodeId,
    pub stream: LogStream,
    pub data: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

/// Which output stream a log entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// Configuration for log capture on a running allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Size of the per-allocation ring buffer on each node (bytes).
    pub ring_buffer_size: u64,
    /// Whether to persist logs to S3.
    pub s3_persistence: bool,
    /// Retention duration for persisted logs.
    pub retention: Option<chrono::Duration>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            ring_buffer_size: 64 * 1024 * 1024, // 64 MB
            s3_persistence: true,
            retention: None, // use system default
        }
    }
}

// ─── Observability: Metrics ────────────────────────────────

/// Per-node metrics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetricsSnapshot {
    pub node_id: NodeId,
    pub timestamp: DateTime<Utc>,
    pub cpu_utilization: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub network_tx_bytes_per_sec: f64,
    pub network_rx_bytes_per_sec: f64,
    pub io_read_bytes_per_sec: f64,
    pub io_write_bytes_per_sec: f64,
    pub io_latency_p99_us: f64,
    pub gpus: Vec<GpuMetricsSnapshot>,
}

/// Per-GPU metrics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuMetricsSnapshot {
    pub index: u32,
    pub utilization: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub power_draw_watts: f64,
    pub temperature_celsius: f64,
    pub ecc_errors: u64,
}

/// Aggregated metrics across all nodes in an allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationMetricsSummary {
    pub allocation_id: AllocId,
    pub timestamp: DateTime<Utc>,
    pub gpu_utilization_mean: f64,
    pub cpu_utilization_mean: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub gpu_memory_used_bytes: u64,
    pub gpu_memory_total_bytes: u64,
    pub network_tx_bytes_per_sec: f64,
    pub network_rx_bytes_per_sec: f64,
    pub io_read_bytes_per_sec: f64,
    pub io_write_bytes_per_sec: f64,
    pub io_latency_p99_us: f64,
}

// ─── Observability: Diagnostics ────────────────────────────

/// Network diagnostics for a running allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkDiagnostics {
    pub group_span: u32,
    pub groups: Vec<GroupId>,
    pub csig_congestion_avg: f64,
    pub inter_node_bandwidth_gbps: f64,
    pub target_bandwidth_gbps: f64,
    pub node_pairs: Vec<NodePairBandwidth>,
    pub nvlink_throughput_gbps: f64,
    pub network_errors: u64,
}

/// Measured bandwidth between a pair of nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePairBandwidth {
    pub source_node: NodeId,
    pub target_node: NodeId,
    pub bandwidth_gbps: f64,
    pub latency_us: f64,
}

/// Storage diagnostics for a running allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDiagnostics {
    pub mounts: Vec<MountDiagnostics>,
}

/// Per-mount storage diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountDiagnostics {
    pub mount_path: String,
    pub mount_type: String,
    pub read_throughput_gbps: f64,
    pub write_throughput_gbps: f64,
    pub qos_floor_gbps: f64,
    pub latency_p50_us: f64,
    pub latency_p95_us: f64,
    pub latency_p99_us: f64,
    pub iops_read: f64,
    pub iops_write: f64,
    pub health: String,
}

// ─── Observability: Alerts ─────────────────────────────────

/// A threshold alert generated by a node agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricAlert {
    pub node_id: NodeId,
    pub metric_name: String,
    pub current_value: f64,
    pub threshold: f64,
    pub severity: AlertSeverity,
    pub message: String,
    pub timestamp: DateTime<Utc>,
}

/// Severity level for metric alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

// ─── Node ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub group: GroupId,
    pub capabilities: NodeCapabilities,
    pub state: NodeState,
    pub owner: Option<NodeOwnership>,
    /// Conformance fingerprint (hash of OS image, kernel, driver versions)
    pub conformance_fingerprint: Option<String>,
    /// Last heartbeat received from this node's agent
    pub last_heartbeat: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    pub gpu_type: Option<String>,
    pub gpu_count: u32,
    pub cpu_cores: u32,
    pub memory_gb: u64,
    pub features: Vec<String>,
    /// Detailed GPU topology (interconnect, NIC affinity)
    pub gpu_topology: Option<GpuTopology>,
    /// Memory topology (NUMA, CXL, unified memory)
    pub memory_topology: Option<MemoryTopology>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    /// Node exists in inventory but has never reported
    Unknown,
    /// OpenCHAMI booting/reimaging the node
    Booting,
    /// Healthy, agent reporting, available for scheduling
    Ready,
    /// Heartbeat missed or minor issue detected
    Degraded { reason: String },
    /// Confirmed failure, grace period expired
    Down { reason: String },
    /// Operator or scheduler requested drain, waiting for allocations to finish
    Draining,
    /// All allocations completed after drain
    Drained,
    /// Boot failure or unrecoverable hardware error
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOwnership {
    pub tenant: TenantId,
    pub vcluster: VClusterId,
    pub allocation: AllocId,
    /// For sensitive: the specific user who claimed this node
    pub claimed_by: Option<UserId>,
    /// Home vCluster (permanent assignment) vs current (may be borrowed)
    pub is_borrowed: bool,
}

// ─── Tenant ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    pub quota: TenantQuota,
    pub isolation_level: IsolationLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantQuota {
    /// Maximum nodes this tenant can use simultaneously
    pub max_nodes: u32,
    /// Target fair-share fraction (0.0 - 1.0)
    pub fair_share_target: f64,
    /// GPU-hours budget (None = unlimited)
    pub gpu_hours_budget: Option<f64>,
    /// Maximum concurrent allocations (hard limit per quota-enforcement)
    pub max_concurrent_allocations: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IsolationLevel {
    /// Standard HPC: shared nodes possible, elastic borrowing
    Standard,
    /// Strict: dedicated nodes, no sharing, audit logging
    Strict,
}

// ─── VCluster ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VCluster {
    pub id: VClusterId,
    pub name: String,
    pub tenant: TenantId,
    pub scheduler_type: SchedulerType,
    pub cost_weights: CostWeights,
    /// Nodes permanently assigned to this vCluster
    pub dedicated_nodes: Vec<NodeId>,
    /// Can borrow from other vClusters' idle nodes?
    pub allow_borrowing: bool,
    /// Can lend idle nodes to other vClusters?
    pub allow_lending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulerType {
    /// Priority + backfill + topology-aware (traditional HPC)
    HpcBackfill,
    /// Bin-packing + autoscale (services)
    ServiceBinPack,
    /// User-claim reservation (sensitive)
    SensitiveReservation,
    /// FIFO, short-lived (interactive sessions)
    InteractiveFifo,
}

/// Weights for the composite cost function. Weights are relative;
/// normalization (sum to 1.0) is optional — the solver normalizes internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        // Default: balanced HPC profile
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

// ─── GPU Topology ───────────────────────────────────────────

/// Intra-node GPU topology: devices, interconnect links, and NIC affinity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuTopology {
    pub devices: Vec<GpuDevice>,
    /// Mapping of GPU index → closest NIC index (for Slingshot/UE affinity)
    pub nic_affinity: HashMap<u32, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuDevice {
    pub index: u32,
    pub vendor: GpuVendor,
    pub model: String,
    pub memory_bytes: u64,
    pub links: Vec<GpuLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuLink {
    pub peer_index: u32,
    pub link_type: GpuLinkType,
    pub bandwidth_gbps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuLinkType {
    NVLink,
    NVSwitch,
    InfinityFabric,
    PCIe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
}

// ─── Memory Topology ────────────────────────────────────────

/// Intra-node memory topology: NUMA domains, CXL tiers, and unified memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryTopology {
    pub domains: Vec<MemoryDomain>,
    pub interconnects: Vec<MemoryInterconnect>,
    pub total_capacity_bytes: u64,
}

/// A memory domain (NUMA node, HBM bank, CXL-attached pool, or unified).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDomain {
    pub id: u32,
    pub domain_type: MemoryDomainType,
    pub capacity_bytes: u64,
    pub numa_node: Option<u32>,
    pub attached_cpus: Vec<u32>,
    pub attached_gpus: Vec<u32>,
}

/// Interconnect link between two memory domains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInterconnect {
    pub domain_a: u32,
    pub domain_b: u32,
    pub link_type: MemoryLinkType,
    pub bandwidth_gbps: f64,
    pub latency_ns: u64,
}

/// Type of memory in a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryDomainType {
    Dram,
    Hbm,
    CxlAttached,
    Unified,
}

/// Type of link between memory domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryLinkType {
    NumaLink,
    CxlSwitch,
    CoherentFabric,
}

/// Memory binding policy for numactl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryPolicy {
    Local,
    Interleave,
    Preferred,
    Bind,
}

// ─── Topology ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyModel {
    pub groups: Vec<TopologyGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyGroup {
    pub id: GroupId,
    pub nodes: Vec<NodeId>,
    /// Adjacent groups (lower inter-group hop cost)
    pub adjacent_groups: Vec<GroupId>,
}

// ─── State Machine Validation ──────────────────────────────

impl AllocationState {
    /// Returns true if transitioning from `self` to `target` is a valid state change.
    ///
    /// Valid transitions:
    /// - Pending → Staging, Running, Cancelled, Failed
    /// - Staging → Running, Failed, Cancelled
    /// - Running → Checkpointing, Completed, Failed, Cancelled
    /// - Checkpointing → Suspended, Failed, Cancelled
    /// - Suspended → Pending (requeue), Cancelled, Failed
    /// - Terminal states (Completed, Failed, Cancelled) → nothing
    pub fn can_transition_to(&self, target: &AllocationState) -> bool {
        matches!(
            (self, target),
            (AllocationState::Pending, AllocationState::Staging)
                | (AllocationState::Pending, AllocationState::Running)
                | (AllocationState::Pending, AllocationState::Cancelled)
                | (AllocationState::Pending, AllocationState::Failed)
                | (AllocationState::Staging, AllocationState::Running)
                | (AllocationState::Staging, AllocationState::Failed)
                | (AllocationState::Staging, AllocationState::Cancelled)
                | (AllocationState::Running, AllocationState::Checkpointing)
                | (AllocationState::Running, AllocationState::Completed)
                | (AllocationState::Running, AllocationState::Failed)
                | (AllocationState::Running, AllocationState::Cancelled)
                | (AllocationState::Checkpointing, AllocationState::Suspended)
                | (AllocationState::Checkpointing, AllocationState::Failed)
                | (AllocationState::Checkpointing, AllocationState::Cancelled)
                | (AllocationState::Suspended, AllocationState::Pending)
                | (AllocationState::Suspended, AllocationState::Cancelled)
                | (AllocationState::Suspended, AllocationState::Failed)
        )
    }

    /// Returns true if this is a terminal state (no further transitions possible).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AllocationState::Completed | AllocationState::Failed | AllocationState::Cancelled
        )
    }
}

impl NodeState {
    /// Returns true if transitioning from `self` to `target` is a valid state change.
    ///
    /// Valid transitions:
    /// - Unknown → Booting, Failed
    /// - Booting → Ready, Failed
    /// - Ready → Degraded, Draining, Down
    /// - Degraded → Ready, Down, Draining
    /// - Down → Booting (reimaging), Failed
    /// - Draining → Drained
    /// - Drained → Ready (undrain), Booting (reimage)
    /// - Failed → Booting (reimage)
    pub fn can_transition_to(&self, target: &NodeState) -> bool {
        matches!(
            (self, target),
            (NodeState::Unknown, NodeState::Booting)
                | (NodeState::Unknown, NodeState::Failed { .. })
                | (NodeState::Booting, NodeState::Ready)
                | (NodeState::Booting, NodeState::Failed { .. })
                | (NodeState::Ready, NodeState::Degraded { .. })
                | (NodeState::Ready, NodeState::Draining)
                | (NodeState::Ready, NodeState::Down { .. })
                | (NodeState::Degraded { .. }, NodeState::Ready)
                | (NodeState::Degraded { .. }, NodeState::Down { .. })
                | (NodeState::Degraded { .. }, NodeState::Draining)
                | (NodeState::Down { .. }, NodeState::Booting)
                | (NodeState::Down { .. }, NodeState::Failed { .. })
                | (NodeState::Draining, NodeState::Drained)
                | (NodeState::Drained, NodeState::Ready)
                | (NodeState::Drained, NodeState::Booting)
                | (NodeState::Failed { .. }, NodeState::Booting)
        )
    }

    /// Returns true if the node can accept workloads.
    pub fn is_operational(&self) -> bool {
        matches!(self, NodeState::Ready | NodeState::Degraded { .. })
    }
}

impl NetworkDomainState {
    /// Returns true if transitioning from `self` to `target` is a valid state change.
    ///
    /// Valid transitions:
    /// - Active → Draining
    /// - Draining → Active (new member joins), Released (grace expired)
    /// - Released → nothing (terminal)
    pub fn can_transition_to(&self, target: &NetworkDomainState) -> bool {
        matches!(
            (self, target),
            (NetworkDomainState::Active, NetworkDomainState::Draining)
                | (NetworkDomainState::Draining, NetworkDomainState::Active)
                | (NetworkDomainState::Draining, NetworkDomainState::Released)
        )
    }
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── AllocationState transition tests ──

    #[test]
    fn pending_can_transition_to_staging() {
        assert!(AllocationState::Pending.can_transition_to(&AllocationState::Staging));
    }

    #[test]
    fn pending_can_transition_to_running() {
        assert!(AllocationState::Pending.can_transition_to(&AllocationState::Running));
    }

    #[test]
    fn pending_can_transition_to_cancelled() {
        assert!(AllocationState::Pending.can_transition_to(&AllocationState::Cancelled));
    }

    #[test]
    fn pending_can_transition_to_failed() {
        assert!(AllocationState::Pending.can_transition_to(&AllocationState::Failed));
    }

    #[test]
    fn pending_cannot_transition_to_completed() {
        assert!(!AllocationState::Pending.can_transition_to(&AllocationState::Completed));
    }

    #[test]
    fn pending_cannot_transition_to_checkpointing() {
        assert!(!AllocationState::Pending.can_transition_to(&AllocationState::Checkpointing));
    }

    #[test]
    fn staging_can_transition_to_running() {
        assert!(AllocationState::Staging.can_transition_to(&AllocationState::Running));
    }

    #[test]
    fn staging_can_transition_to_failed() {
        assert!(AllocationState::Staging.can_transition_to(&AllocationState::Failed));
    }

    #[test]
    fn staging_can_transition_to_cancelled() {
        assert!(AllocationState::Staging.can_transition_to(&AllocationState::Cancelled));
    }

    #[test]
    fn running_can_transition_to_checkpointing() {
        assert!(AllocationState::Running.can_transition_to(&AllocationState::Checkpointing));
    }

    #[test]
    fn running_can_transition_to_completed() {
        assert!(AllocationState::Running.can_transition_to(&AllocationState::Completed));
    }

    #[test]
    fn running_can_transition_to_failed() {
        assert!(AllocationState::Running.can_transition_to(&AllocationState::Failed));
    }

    #[test]
    fn running_can_transition_to_cancelled() {
        assert!(AllocationState::Running.can_transition_to(&AllocationState::Cancelled));
    }

    #[test]
    fn running_cannot_transition_to_pending() {
        assert!(!AllocationState::Running.can_transition_to(&AllocationState::Pending));
    }

    #[test]
    fn checkpointing_can_transition_to_suspended() {
        assert!(AllocationState::Checkpointing.can_transition_to(&AllocationState::Suspended));
    }

    #[test]
    fn checkpointing_can_transition_to_failed() {
        assert!(AllocationState::Checkpointing.can_transition_to(&AllocationState::Failed));
    }

    #[test]
    fn checkpointing_can_transition_to_cancelled() {
        assert!(AllocationState::Checkpointing.can_transition_to(&AllocationState::Cancelled));
    }

    #[test]
    fn suspended_can_transition_to_pending() {
        assert!(AllocationState::Suspended.can_transition_to(&AllocationState::Pending));
    }

    #[test]
    fn suspended_can_transition_to_cancelled() {
        assert!(AllocationState::Suspended.can_transition_to(&AllocationState::Cancelled));
    }

    #[test]
    fn suspended_can_transition_to_failed() {
        assert!(AllocationState::Suspended.can_transition_to(&AllocationState::Failed));
    }

    #[test]
    fn completed_is_terminal() {
        assert!(AllocationState::Completed.is_terminal());
    }

    #[test]
    fn failed_is_terminal() {
        assert!(AllocationState::Failed.is_terminal());
    }

    #[test]
    fn cancelled_is_terminal() {
        assert!(AllocationState::Cancelled.is_terminal());
    }

    #[test]
    fn running_is_not_terminal() {
        assert!(!AllocationState::Running.is_terminal());
    }

    #[test]
    fn terminal_states_cannot_transition_to_anything() {
        let terminals = [
            AllocationState::Completed,
            AllocationState::Failed,
            AllocationState::Cancelled,
        ];
        let all_states = [
            AllocationState::Pending,
            AllocationState::Staging,
            AllocationState::Running,
            AllocationState::Checkpointing,
            AllocationState::Suspended,
            AllocationState::Completed,
            AllocationState::Failed,
            AllocationState::Cancelled,
        ];

        for terminal in &terminals {
            for target in &all_states {
                assert!(
                    !terminal.can_transition_to(target),
                    "{terminal:?} should not transition to {target:?}"
                );
            }
        }
    }

    // ── NodeState transition tests ──

    #[test]
    fn unknown_can_transition_to_booting() {
        assert!(NodeState::Unknown.can_transition_to(&NodeState::Booting));
    }

    #[test]
    fn unknown_can_transition_to_failed() {
        assert!(NodeState::Unknown.can_transition_to(&NodeState::Failed {
            reason: "hw error".into()
        }));
    }

    #[test]
    fn booting_can_transition_to_ready() {
        assert!(NodeState::Booting.can_transition_to(&NodeState::Ready));
    }

    #[test]
    fn booting_can_transition_to_failed() {
        assert!(NodeState::Booting.can_transition_to(&NodeState::Failed {
            reason: "boot failed".into()
        }));
    }

    #[test]
    fn ready_can_transition_to_degraded() {
        assert!(NodeState::Ready.can_transition_to(&NodeState::Degraded {
            reason: "heartbeat missed".into()
        }));
    }

    #[test]
    fn ready_can_transition_to_draining() {
        assert!(NodeState::Ready.can_transition_to(&NodeState::Draining));
    }

    #[test]
    fn ready_can_transition_to_down() {
        assert!(NodeState::Ready.can_transition_to(&NodeState::Down {
            reason: "operator".into()
        }));
    }

    #[test]
    fn degraded_can_transition_to_ready() {
        assert!(NodeState::Degraded {
            reason: "fixed".into()
        }
        .can_transition_to(&NodeState::Ready));
    }

    #[test]
    fn degraded_can_transition_to_down() {
        assert!(NodeState::Degraded {
            reason: "worsened".into()
        }
        .can_transition_to(&NodeState::Down {
            reason: "confirmed".into()
        }));
    }

    #[test]
    fn degraded_can_transition_to_draining() {
        assert!(NodeState::Degraded {
            reason: "draining".into()
        }
        .can_transition_to(&NodeState::Draining));
    }

    #[test]
    fn down_can_transition_to_booting() {
        assert!(NodeState::Down {
            reason: "reimage".into()
        }
        .can_transition_to(&NodeState::Booting));
    }

    #[test]
    fn down_can_transition_to_failed() {
        assert!(NodeState::Down {
            reason: "unrecoverable".into()
        }
        .can_transition_to(&NodeState::Failed {
            reason: "hw".into()
        }));
    }

    #[test]
    fn draining_can_transition_to_drained() {
        assert!(NodeState::Draining.can_transition_to(&NodeState::Drained));
    }

    #[test]
    fn draining_cannot_transition_to_ready() {
        assert!(!NodeState::Draining.can_transition_to(&NodeState::Ready));
    }

    #[test]
    fn drained_can_transition_to_ready() {
        assert!(NodeState::Drained.can_transition_to(&NodeState::Ready));
    }

    #[test]
    fn drained_can_transition_to_booting() {
        assert!(NodeState::Drained.can_transition_to(&NodeState::Booting));
    }

    #[test]
    fn failed_node_can_transition_to_booting() {
        assert!(NodeState::Failed {
            reason: "reimage".into()
        }
        .can_transition_to(&NodeState::Booting));
    }

    #[test]
    fn ready_is_operational() {
        assert!(NodeState::Ready.is_operational());
    }

    #[test]
    fn degraded_is_operational() {
        assert!(NodeState::Degraded {
            reason: "minor".into()
        }
        .is_operational());
    }

    #[test]
    fn down_is_not_operational() {
        assert!(!NodeState::Down {
            reason: "down".into()
        }
        .is_operational());
    }

    #[test]
    fn draining_is_not_operational() {
        assert!(!NodeState::Draining.is_operational());
    }

    // ── NetworkDomainState transition tests ──

    #[test]
    fn active_can_transition_to_draining() {
        assert!(NetworkDomainState::Active.can_transition_to(&NetworkDomainState::Draining));
    }

    #[test]
    fn active_cannot_transition_to_released() {
        assert!(!NetworkDomainState::Active.can_transition_to(&NetworkDomainState::Released));
    }

    #[test]
    fn draining_can_transition_to_active() {
        assert!(NetworkDomainState::Draining.can_transition_to(&NetworkDomainState::Active));
    }

    #[test]
    fn draining_can_transition_to_released() {
        assert!(NetworkDomainState::Draining.can_transition_to(&NetworkDomainState::Released));
    }

    #[test]
    fn released_cannot_transition_to_anything() {
        assert!(!NetworkDomainState::Released.can_transition_to(&NetworkDomainState::Active));
        assert!(!NetworkDomainState::Released.can_transition_to(&NetworkDomainState::Draining));
        assert!(!NetworkDomainState::Released.can_transition_to(&NetworkDomainState::Released));
    }

    // ── Serialization round-trip tests ──

    #[test]
    fn allocation_state_serde_roundtrip() {
        let states = [
            AllocationState::Pending,
            AllocationState::Staging,
            AllocationState::Running,
            AllocationState::Checkpointing,
            AllocationState::Suspended,
            AllocationState::Completed,
            AllocationState::Failed,
            AllocationState::Cancelled,
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let deser: AllocationState = serde_json::from_str(&json).unwrap();
            assert_eq!(*state, deser, "roundtrip failed for {state:?}");
        }
    }

    #[test]
    fn node_state_serde_roundtrip() {
        let states = [
            NodeState::Unknown,
            NodeState::Booting,
            NodeState::Ready,
            NodeState::Degraded {
                reason: "test".into(),
            },
            NodeState::Down {
                reason: "test".into(),
            },
            NodeState::Draining,
            NodeState::Drained,
            NodeState::Failed {
                reason: "test".into(),
            },
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let deser: NodeState = serde_json::from_str(&json).unwrap();
            assert_eq!(*state, deser, "roundtrip failed for {state:?}");
        }
    }

    #[test]
    fn cost_weights_default_sums_to_one() {
        let w = CostWeights::default();
        let sum = w.priority
            + w.wait_time
            + w.fair_share
            + w.topology
            + w.data_readiness
            + w.backlog
            + w.energy
            + w.checkpoint_efficiency
            + w.conformance;
        assert!(
            (sum - 1.0).abs() < 1e-10,
            "default weights sum to {sum}, expected 1.0"
        );
    }

    #[test]
    fn cost_weights_serde_roundtrip() {
        let w = CostWeights::default();
        let json = serde_json::to_string(&w).unwrap();
        let deser: CostWeights = serde_json::from_str(&json).unwrap();
        assert!((deser.priority - w.priority).abs() < f64::EPSILON);
        assert!((deser.topology - w.topology).abs() < f64::EPSILON);
    }

    // ── Memory topology tests ──

    #[test]
    fn memory_domain_type_serde_roundtrip() {
        let types = [
            MemoryDomainType::Dram,
            MemoryDomainType::Hbm,
            MemoryDomainType::CxlAttached,
            MemoryDomainType::Unified,
        ];
        for t in &types {
            let json = serde_json::to_string(t).unwrap();
            let deser: MemoryDomainType = serde_json::from_str(&json).unwrap();
            assert_eq!(*t, deser, "roundtrip failed for {t:?}");
        }
    }

    #[test]
    fn memory_link_type_serde_roundtrip() {
        let types = [
            MemoryLinkType::NumaLink,
            MemoryLinkType::CxlSwitch,
            MemoryLinkType::CoherentFabric,
        ];
        for t in &types {
            let json = serde_json::to_string(t).unwrap();
            let deser: MemoryLinkType = serde_json::from_str(&json).unwrap();
            assert_eq!(*t, deser, "roundtrip failed for {t:?}");
        }
    }

    #[test]
    fn memory_policy_serde_roundtrip() {
        let policies = [
            MemoryPolicy::Local,
            MemoryPolicy::Interleave,
            MemoryPolicy::Preferred,
            MemoryPolicy::Bind,
        ];
        for p in &policies {
            let json = serde_json::to_string(p).unwrap();
            let deser: MemoryPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(*p, deser, "roundtrip failed for {p:?}");
        }
    }

    #[test]
    fn memory_topology_serde_roundtrip() {
        let topo = MemoryTopology {
            domains: vec![
                MemoryDomain {
                    id: 0,
                    domain_type: MemoryDomainType::Dram,
                    capacity_bytes: 128 * 1024 * 1024 * 1024,
                    numa_node: Some(0),
                    attached_cpus: vec![0, 1, 2, 3],
                    attached_gpus: vec![0, 1],
                },
                MemoryDomain {
                    id: 1,
                    domain_type: MemoryDomainType::CxlAttached,
                    capacity_bytes: 256 * 1024 * 1024 * 1024,
                    numa_node: None,
                    attached_cpus: vec![],
                    attached_gpus: vec![],
                },
            ],
            interconnects: vec![MemoryInterconnect {
                domain_a: 0,
                domain_b: 1,
                link_type: MemoryLinkType::CxlSwitch,
                bandwidth_gbps: 64.0,
                latency_ns: 200,
            }],
            total_capacity_bytes: 384 * 1024 * 1024 * 1024,
        };
        let json = serde_json::to_string(&topo).unwrap();
        let deser: MemoryTopology = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.domains.len(), 2);
        assert_eq!(deser.interconnects.len(), 1);
        assert_eq!(deser.total_capacity_bytes, topo.total_capacity_bytes);
    }

    #[test]
    fn resource_constraints_default_has_memory_fields() {
        let rc = ResourceConstraints::default();
        assert!(!rc.require_unified_memory);
        assert!(!rc.prefer_same_numa);
        assert!(!rc.allow_cxl_memory);
        assert!(rc.memory_policy.is_none());
    }

    #[test]
    fn resource_constraints_memory_serde_roundtrip() {
        let rc = ResourceConstraints {
            require_unified_memory: true,
            prefer_same_numa: true,
            allow_cxl_memory: false,
            memory_policy: Some(MemoryPolicy::Interleave),
            ..Default::default()
        };
        let json = serde_json::to_string(&rc).unwrap();
        let deser: ResourceConstraints = serde_json::from_str(&json).unwrap();
        assert!(deser.require_unified_memory);
        assert!(deser.prefer_same_numa);
        assert!(!deser.allow_cxl_memory);
        assert_eq!(deser.memory_policy, Some(MemoryPolicy::Interleave));
    }

    // ── Property-based tests ──

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_allocation_state() -> impl Strategy<Value = AllocationState> {
            prop_oneof![
                Just(AllocationState::Pending),
                Just(AllocationState::Staging),
                Just(AllocationState::Running),
                Just(AllocationState::Checkpointing),
                Just(AllocationState::Suspended),
                Just(AllocationState::Completed),
                Just(AllocationState::Failed),
                Just(AllocationState::Cancelled),
            ]
        }

        fn arb_memory_domain_type() -> impl Strategy<Value = MemoryDomainType> {
            prop_oneof![
                Just(MemoryDomainType::Dram),
                Just(MemoryDomainType::Hbm),
                Just(MemoryDomainType::CxlAttached),
                Just(MemoryDomainType::Unified),
            ]
        }

        fn arb_memory_policy() -> impl Strategy<Value = MemoryPolicy> {
            prop_oneof![
                Just(MemoryPolicy::Local),
                Just(MemoryPolicy::Interleave),
                Just(MemoryPolicy::Preferred),
                Just(MemoryPolicy::Bind),
            ]
        }

        proptest! {
            #[test]
            fn memory_domain_type_roundtrip(dt in arb_memory_domain_type()) {
                let json = serde_json::to_string(&dt).unwrap();
                let back: MemoryDomainType = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(dt, back);
            }

            #[test]
            fn memory_policy_roundtrip(p in arb_memory_policy()) {
                let json = serde_json::to_string(&p).unwrap();
                let back: MemoryPolicy = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(p, back);
            }

            #[test]
            fn terminal_states_block_all_transitions(target in arb_allocation_state()) {
                let terminals = [
                    AllocationState::Completed,
                    AllocationState::Failed,
                    AllocationState::Cancelled,
                ];
                for terminal in &terminals {
                    prop_assert!(!terminal.can_transition_to(&target));
                }
            }

            #[test]
            fn no_self_transitions(state in arb_allocation_state()) {
                prop_assert!(!state.can_transition_to(&state));
            }

            #[test]
            fn node_count_range_min_le_max(min in 1u32..1000, max in 1u32..1000) {
                prop_assume!(min <= max);
                let _nc = NodeCount::Range { min, max };
                // Construction succeeds without panic
            }
        }
    }
}

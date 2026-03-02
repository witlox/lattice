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

    // What to run
    pub environment: Environment,
    pub entrypoint: String,

    // Resources
    pub resources: ResourceRequest,

    // Lifecycle
    pub lifecycle: Lifecycle,

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// For medical: require signed images
    pub sign_required: bool,
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
    Range { min: u32, max: u32 },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceConstraints {
    pub gpu_type: Option<String>,
    pub features: Vec<String>,
    /// Topology hint: "tight" (fewest groups), "spread" (max bandwidth), "any"
    pub topology: Option<TopologyHint>,
    /// Feature count requirements (e.g., at least 4 nodes with "nvme_scratch")
    pub feature_counts: HashMap<String, u32>,
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
    pub source: String,        // s3://... or nfs://...
    pub target: String,        // mount point inside allocation
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TelemetryMode {
    /// 30s aggregation, bicubic smoothing, low overhead
    Prod,
    /// 1s raw streams, full profiling, higher overhead
    Debug { duration_seconds: u64 },
    /// Access logging for compliance, moderate overhead
    Audit,
}

impl Default for TelemetryMode {
    fn default() -> Self {
        Self::Prod
    }
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
    pub health: NodeHealth,
    pub owner: Option<NodeOwnership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    pub gpu_type: Option<String>,
    pub gpu_count: u32,
    pub cpu_cores: u32,
    pub memory_gb: u64,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeHealth {
    Healthy,
    Degraded { reason: String },
    Down { reason: String },
    Draining,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOwnership {
    pub tenant: TenantId,
    pub vcluster: VClusterId,
    pub allocation: AllocId,
    /// For medical: the specific user who claimed this node
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulerType {
    /// Priority + backfill + topology-aware (traditional HPC)
    HpcBackfill,
    /// Bin-packing + autoscale (services)
    ServiceBinPack,
    /// User-claim reservation (medical)
    MedicalReservation,
    /// FIFO, short-lived (interactive sessions)
    InteractiveFifo,
}

/// Weights for the composite cost function. Must sum to ~1.0.
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
}

impl Default for CostWeights {
    fn default() -> Self {
        // Default: balanced HPC profile
        Self {
            priority: 0.20,
            wait_time: 0.25,
            fair_share: 0.25,
            topology: 0.15,
            data_readiness: 0.10,
            backlog: 0.05,
            energy: 0.00,
            checkpoint_efficiency: 0.00,
        }
    }
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

# Shared Kernel Data Models (lattice-common)

Types shared across all modules. These are the canonical representations — modules must NOT define competing versions.

## Identity Types

| Type | Underlying | Format | Example |
|---|---|---|---|
| AllocId | UUID (v4) | `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` | `a1b2c3d4-...` |
| NodeId | String | xname: `x{cab}c{chas}s{slot}b{board}n{node}` | `x1000c0s0b0n0` |
| TenantId | String | Lowercase alphanumeric + hyphen | `physics-dept` |
| VClusterId | String | Lowercase alphanumeric + hyphen | `hpc-batch` |
| ProjectId | String | Lowercase alphanumeric + hyphen | `simulation-2026` |
| UserId | String | OIDC subject claim | `auth0\|abc123` |
| DagId | UUID (v4) | Same as AllocId | - |
| LaunchId | UUID (v4) | MPI launch identifier | - |

## Allocation

The universal work unit. Root aggregate for the Scheduling context.

```
Allocation {
  id: AllocId,
  tenant: TenantId,
  project: ProjectId,
  vcluster: VClusterId,           // Immutable after creation (INV-C4)
  user: UserId,
  name: Option<String>,
  tags: HashMap<String, String>,

  // What to run
  environment: Environment {
    uenv: Option<String>,         // SquashFS image ref
    view: Option<String>,         // uenv view name
    image: Option<String>,        // OCI image ref (Sarus)
    tools_uenv: Option<String>,   // Additional tools overlay
    sign_required: bool,          // Sensitive: signed images only
  },
  entrypoint: String,
  args: Vec<String>,
  env_vars: HashMap<String, String>,

  // Resources
  resources: ResourceConstraints {
    min_nodes: u32,
    max_nodes: Option<u32>,       // Reactive: autoscale range
    gpu_type: Option<String>,     // e.g., "GH200", "MI300X"
    gpu_count_per_node: Option<u32>,
    features: Vec<String>,        // Required node features
    topology_hint: Option<TopologyHint>,  // Same group preference
    require_unified_memory: bool,
    prefer_same_numa: bool,
    allow_cxl_memory: bool,
    memory_policy: Option<MemoryPolicy>,
  },

  // Lifecycle
  lifecycle: Lifecycle,           // Bounded | Unbounded | Reactive
  state: AllocationState,
  walltime: Option<Duration>,     // Bounded only
  preemption_class: u8,           // 0-10, set by Tenant contract

  // Composition
  dag_id: Option<DagId>,
  task_group_id: Option<AllocId>,
  task_index: Option<u32>,

  // Network
  network_domain: Option<String>, // Tenant-scoped domain name

  // Data
  data_mounts: Vec<DataMount>,
  scratch_per_node: Option<u64>,  // Bytes

  // Checkpointing
  checkpoint_strategy: CheckpointStrategy,
  checkpoint_path: Option<String>,

  // Policy
  requeue_policy: RequeuePolicy,
  max_requeue: u32,
  requeue_count: u32,

  // Scheduling metadata
  assigned_nodes: Vec<NodeId>,
  created_at: Timestamp,
  started_at: Option<Timestamp>,
  completed_at: Option<Timestamp>,
  exit_code: Option<i32>,
  state_message: Option<String>,

  // Telemetry
  telemetry_mode: TelemetryMode,

  // Sensitive
  sensitive: bool,
}
```

## Allocation State Machine

```
Pending ──► Staging ──► Running ──► Completed
  │            │          │  │          ▲
  │            │          │  └──► Checkpointing ──► Suspended
  │            │          │                            │
  │            │          └──► Failed                  │
  │            │                                       │
  │            └──► Failed (prologue)                  │
  │                                                    │
  └──► Cancelled                                       │
                                                       │
  Suspended ──► Pending (requeue)                      │
```

| State | Entry Condition | Exit Condition |
|---|---|---|
| Pending | Submission accepted, dependencies met | Scheduler places it |
| Staging | Nodes assigned, prologue starts | Prologue completes |
| Running | Entrypoint started | Natural exit, walltime, preemption, cancel |
| Checkpointing | Checkpoint hint received | Checkpoint completes or times out |
| Suspended | Checkpoint completed successfully | Requeued (returns to Pending) |
| Completed | Process exits 0 | Terminal |
| Failed | Non-zero exit, prologue fail, timeout | Terminal (may requeue) |
| Cancelled | User/admin cancel request | Terminal |

## Node

```
Node {
  id: NodeId,                     // xname
  state: NodeState,
  state_reason: Option<String>,

  capabilities: NodeCapabilities {
    gpu_type: Option<String>,
    gpu_count: u32,
    cpu_cores: u32,
    memory_gb: u64,
    features: Vec<String>,
    memory_topology: Option<MemoryTopology>,
  },

  ownership: Option<NodeOwnership>,
  conformance_fingerprint: Option<String>,  // SHA-256
  topology_position: Option<TopologyPosition>,
  last_heartbeat: Option<Timestamp>,
  heartbeat_sequence: u64,
}
```

## Node State Machine

```
Unknown ──► Booting ──► Ready ◄──► Degraded ──► Down
                │         │                       ▲
                │         └──► Draining ──► Drained ──► Ready
                └──► Failed
```

## NodeOwnership

```
NodeOwnership {
  tenant: TenantId,
  vcluster: VClusterId,
  allocation: AllocId,
  claimed_by: Option<UserId>,     // Sensitive: claiming user
  is_borrowed: bool,
  is_sensitive: bool,
}
```

## Tenant

```
Tenant {
  id: TenantId,
  name: String,
  isolation_level: IsolationLevel, // Standard | Strict

  // Hard quotas (Raft-enforced, INV-S2)
  max_nodes: u32,
  max_concurrent_allocations: u32,
  sensitive_pool_size: Option<u32>,

  // Soft quotas (scheduler-enforced, INV-E6)
  gpu_hours_budget: Option<f64>,
  node_hours_budget: Option<f64>,
  fair_share_target: Option<f64>,
  burst_allowance: Option<f64>,
}
```

## VCluster

```
VCluster {
  id: VClusterId,
  tenant: TenantId,
  name: String,
  scheduler_type: SchedulerType,  // HpcBackfill | ServiceBinPack | SensitiveReservation | InteractiveFifo
  cost_weights: CostWeights,
  base_allocation: u32,           // Guaranteed node count
  allow_borrowing: bool,
  allow_lending: bool,
  dedicated_nodes: bool,          // Sensitive: true
}
```

## CostWeights

```
CostWeights {
  priority: f64,        // f1
  wait_time: f64,       // f2
  fair_share: f64,      // f3
  topology: f64,        // f4
  data_readiness: f64,  // f5
  backlog: f64,         // f6
  energy: f64,          // f7
  checkpoint: f64,      // f8
  conformance: f64,     // f9
}
```

## TopologyModel

```
TopologyModel {
  groups: Vec<DragonFlyGroup>,
}

DragonFlyGroup {
  id: String,
  nodes: Vec<NodeId>,
}
```

## MemoryTopology

```
MemoryTopology {
  domains: Vec<MemoryDomain>,
  interconnects: Vec<MemoryInterconnect>,
  total_capacity_bytes: u64,
}

MemoryDomain {
  id: u32,
  domain_type: MemoryDomainType,  // Dram | Hbm | CxlAttached | Unified
  capacity_bytes: u64,
  numa_node: Option<u32>,
  attached_cpus: Vec<u32>,
  attached_gpus: Vec<u32>,
}

MemoryInterconnect {
  domain_a: u32,
  domain_b: u32,
  link_type: MemoryLinkType,      // NumaLink | CxlSwitch | CoherentFabric
  bandwidth_gbps: f64,
  latency_ns: u64,
}
```

## NetworkDomain

```
NetworkDomain {
  name: String,
  tenant: TenantId,
  vni: u32,                       // 1000-4095 (INV-C3)
  allocations: Vec<AllocId>,
  grace_period_end: Option<Timestamp>,
}
```

## AuditEntry

```
AuditEntry {
  id: UUID,
  timestamp: Timestamp,
  user: UserId,
  tenant: TenantId,
  action: AuditAction,
  allocation_id: Option<AllocId>,
  node_ids: Vec<NodeId>,
  details: String,
}

AuditAction {
  Claim | Release | Attach | Detach | DataAccess |
  LogAccess | MetricAccess | Checkpoint | WipeStarted |
  WipeCompleted | WipeFailed | GpuClearCompleted
}
```

## Enumerations

```
Lifecycle           = Bounded { walltime } | Unbounded | Reactive { min, max, metric, threshold }
CheckpointStrategy  = Auto | Manual | None
RequeuePolicy       = Never | OnNodeFailure | Always
TelemetryMode       = Prod | Debug | Audit
IsolationLevel      = Standard | Strict
SchedulerType       = HpcBackfill | ServiceBinPack | SensitiveReservation | InteractiveFifo
MemoryPolicy        = Local | Interleave | Preferred | Bind
TopologyHint        = SameGroup | MinGroups | None
```

## DataMount

```
DataMount {
  source: String,           // e.g., "vast://datasets/imagenet"
  target: String,           // e.g., "/data/imagenet"
  read_only: bool,
  qos: Option<DataQos>,    // ReadWrite | ReadOnly | Archive
}
```

# System Architecture

## Overview

Lattice is a seven-layer architecture where each layer has a clear responsibility and communicates with adjacent layers via defined interfaces.

```
┌─ User Plane ───────────────────────────────────────────────────┐
│  FirecREST API Gateway (OIDC/SAML)                             │
│  ├── Job lifecycle (submit, monitor, cancel)                   │
│  ├── Interactive sessions (WebSocket terminal)                 │
│  ├── Data management (stage, browse, transfer)                 │
│  ├── uenv management (list, pull, test)                        │
│  ├── Observability (attach, logs, metrics, diagnostics)        │
│  └── Sensitive: user-level node claim/release                    │
└───────────────────────────┬────────────────────────────────────┘
                            │
┌─ Software Plane ──────────┴────────────────────────────────────┐
│  Default: uenv (squashfs + mount namespace)                    │
│  Optional: OCI/Sarus (isolation, third-party images)           │
│  Registry: JFrog/Nexus → S3 backing (VAST hot tier)            │
│  Node-local NVMe image cache (optional)                        │
│  Sensitive: signed images only, vulnerability-scanned            │
└───────────────────────────┬────────────────────────────────────┘
                            │
┌─ Scheduling Plane ────────┴────────────────────────────────────┐
│  Quorum (Raft, 3-5 replicas)                                   │
│  Strong: (1) node ownership  (2) sensitive audit log             │
│  Eventual: job queues, telemetry, quotas                       │
│                                                                │
│  vCluster Schedulers:                                          │
│  ├── HPC: backfill + dragonfly group packing                   │
│  ├── Service: bin-pack + autoscale                             │
│  ├── Sensitive: user-claim reservation, dedicated nodes          │
│  └── Interactive: FIFO, short-lived, node-sharing via Sarus    │
└───────────────────────────┬────────────────────────────────────┘
                            │
┌─ Data Plane ──────────────┴────────────────────────────────────┐
│  Hot:  VAST (NFS + S3, single flash tier)                      │
│    ├── Home dirs, scratch, active datasets (NFS)               │
│    ├── Checkpoints, image cache, objects (S3)                  │
│    ├── Scheduler integration: QoS, pre-staging, snapshots      │
│    └── Sensitive: encrypted view, audit-logged, dedicated pool   │
│  Warm: Capacity store (S3-compat, cost-optimized)              │
│  Cold: Tape archive (S3-compat, regulatory retention)          │
│  Data mover: pre-stages during queue wait, policy-driven       │
└───────────────────────────┬────────────────────────────────────┘
                            │
┌─ Network Fabric ──────────┴────────────────────────────────────┐
│  Slingshot (current) / Ultra Ethernet (future path)            │
│  ├── libfabric abstraction for workload communication          │
│  ├── VNI-based network domains (job isolation)                 │
│  ├── Traffic classes: compute | management | telemetry         │
│  ├── CSIG for in-band congestion telemetry                     │
│  └── Sensitive: encrypted RDMA, dedicated VNI                    │
└───────────────────────────┬────────────────────────────────────┘
                            │
┌─ Node Plane ──────────────┴────────────────────────────────────┐
│  Node Agent (per node)                                         │
│  ├── squashfs-mount (uenv delivery)                            │
│  ├── Sarus (OCI container runtime, when needed)                │
│  ├── eBPF telemetry + CSIG tap                                 │
│  ├── Node-local NVMe (optional): scratch + image cache         │
│  ├── Conformance fingerprint (driver/firmware/kernel hash)     │
│  └── Health reporting → OpenCHAMI SMD                          │
└───────────────────────────┬────────────────────────────────────┘
                            │
┌─ Infrastructure Plane ────┴────────────────────────────────────┐
│  OpenCHAMI                                                     │
│  ├── Magellan: Redfish BMC discovery & inventory               │
│  ├── SMD: State Management Daemon (hardware lifecycle)         │
│  ├── BSS: Boot Script Service (image selection per node)       │
│  ├── OPAAL: Authentication & identity                          │
│  ├── Cloud-init: per-node config injection                     │
│  └── Manta CLI: admin tooling                                  │
└────────────────────────────────────────────────────────────────┘
```

## Component Interactions

### Allocation Lifecycle

```
1. User/Agent → FirecREST → lattice-api (Intent API or Compat API)
2. lattice-api validates request, resolves uenv, creates Allocation object
3. Allocation placed in vCluster scheduler's queue (eventually consistent)
4. vCluster scheduler runs scheduling cycle:
   a. Scores pending allocations with cost function
   b. Solves knapsack: maximize value subject to resource constraints
   c. Proposes allocation → quorum
5. Quorum validates (node ownership, quotas, sensitive isolation)
6. Quorum commits: node ownership updated (strong consistency)
7. Quorum notifies node agents of new allocation
8. Node agents:
   a. Pull uenv squashfs image (from cache or registry)
   b. Mount via squashfs-mount
   c. Start processes in mount namespace
   d. Begin log capture (ring buffer + S3 persistence)
   e. Accept attach sessions (if user connects)
   f. Report health/telemetry
8.5. During execution, users can:
   - Attach interactive terminal (nsenter into allocation namespace)
   - Stream logs (live tail from ring buffer or historical from S3)
   - Query metrics (lattice top → TSDB) or stream them (lattice watch → node agents)
   - View diagnostics (network health, storage performance)
   - Compare metrics across allocations (TSDB multi-query)
9. On completion: node agents report, quorum releases nodes
```

### Preemption Flow

```
1. Higher-priority allocation arrives, needs nodes currently in use
2. Scheduler evaluates: which running allocations are cheapest to preempt?
   → checkpoint_efficiency score from cost function
3. Checkpoint broker sends CHECKPOINT_HINT to target allocation's node agents
4. Application checkpoints (or: timeout → forced preemption)
5. Nodes released, reassigned to higher-priority allocation
6. Preempted allocation re-queued, will resume from checkpoint when resources available
```

### Federation Flow (when enabled)

```
1. User at Site A submits allocation targeting Site B
2. Site A's federation broker signs request with Sovra token
3. Request arrives at Site B's federation broker
4. Site B verifies Sovra token, checks policy (OPA)
5. If accepted: allocation enters Site B's scheduling plane
6. Site B's local quorum manages the allocation entirely
7. Results/logs accessible to user at Site A via federation catalog
```

## Topology Model

The scheduler maintains a model of the Slingshot dragonfly topology:

```
System
├── Group 0 (electrical group, ~hundreds of nodes)
│   ├── Switch 0
│   │   ├── Node 0..N
│   │   └── ...
│   └── Switch M
├── Group 1
│   └── ...
└── Group K
    └── ...

Intra-group: electrical, low latency, high bandwidth
Inter-group: optical, higher latency, potential congestion
```

Scheduling rule: pack jobs into fewest groups possible. Jobs below group size → single group. Large jobs → minimize group span, prefer adjacent groups. Network-sensitive jobs (NCCL) get stricter placement constraints.

## State Machine

The quorum manages a replicated state machine with the following state:

```
GlobalState {
    nodes: Map<NodeId, NodeState>,        // ownership, health, capabilities
    allocations: Map<AllocId, Allocation>, // all active allocations
    tenants: Map<TenantId, TenantState>,  // quotas, fair-share counters
    vclusters: Map<VClusterId, VClusterConfig>, // scheduler configs
    topology: TopologyModel,              // dragonfly group structure
    sensitive_audit: AppendOnlyLog<AuditEvent>, // strong consistency
}

NodeState {
    owner: Option<(TenantId, VClusterId, AllocId)>,
    health: NodeHealth,
    capabilities: NodeCapabilities,  // GPU type, memory, features
    group: GroupId,                  // topology position
    conformance_group: ConformanceGroupId, // fingerprint of driver/firmware/kernel
}
```

Transitions are proposed by vCluster schedulers and validated by the quorum before commit. Only node ownership changes and sensitive audit events require Raft consensus; everything else is eventually consistent.

**Note:** Observability data (logs, metrics, attach sessions, diagnostics) is NOT stored in the Raft state machine. This data lives in the TSDB, S3, and node agent memory. Only sensitive audit events *about* observability actions (e.g., "Dr. X attached to allocation Y") flow through Raft consensus (per ADR-004).

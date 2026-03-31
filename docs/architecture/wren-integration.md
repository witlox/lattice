# Wren–Lattice Bi-Directional Scheduler Integration

## Status

Proposal — not yet implemented.

## Context

[Wren](https://github.com/miguelgila/wren) is a lightweight, Slurm-inspired HPC job scheduler for Kubernetes, written in Rust. It provides gang scheduling, topology-aware placement, backfill, fair-share, and MPI bootstrap. Wren has two execution backends: **Container** (K8s pods) and **Reaper** (bare-metal via ReaperPod CRDs).

Lattice is a distributed workload scheduler for HPC infrastructure with Raft consensus, multi-policy vCluster scheduling, a 9-factor knapsack cost function, and a per-node agent model.

**Goal**: Enable Wren clusters and Lattice clusters to share resources bi-directionally — Wren can schedule onto Lattice-managed nodes, and Lattice can schedule onto Wren-managed nodes — without either system becoming subordinate to the other.

## Design Principles

1. **Sovereign schedulers** — each scheduler retains full authority over its nodes. Cross-scheduling is always a request, never a command.
2. **No shared state** — no shared database, no shared Raft log. State synchronization happens through a well-defined protocol.
3. **Graceful degradation** — if the bridge fails, both schedulers continue operating on their own nodes.
4. **Feature-gated** — the entire integration is behind `#[cfg(feature = "wren")]`, zero cost when disabled.

---

## Architecture Overview

```
┌──────────────────────────────┐     ┌──────────────────────────────┐
│         WREN CLUSTER         │     │       LATTICE CLUSTER        │
│                              │     │                              │
│  ┌────────────────────────┐  │     │  ┌────────────────────────┐  │
│  │   wren-controller      │  │     │  │   lattice-api          │  │
│  │   (K8s reconciler)     │  │     │  │   (gRPC + REST)        │  │
│  └──────────┬─────────────┘  │     │  └──────────┬─────────────┘  │
│             │                │     │             │                │
│  ┌──────────▼─────────────┐  │     │  ┌──────────▼─────────────┐  │
│  │   wren-scheduler       │  │     │  │   lattice-scheduler    │  │
│  │   (gang + backfill)    │  │     │  │   (knapsack + vCluster)│  │
│  └──────────┬─────────────┘  │     │  └──────────┬─────────────┘  │
│             │                │     │             │                │
│  ┌──────────▼─────────────┐  │     │  ┌──────────▼─────────────┐  │
│  │  BridgeAgent           │◄─┼─gRPC─┼─►  BridgeAgent           │  │
│  │  (wren side)           │  │     │  │  (lattice side)        │  │
│  └──────────┬─────────────┘  │     │  └──────────┬─────────────┘  │
│             │                │     │             │                │
│  ┌──────────▼─────────────┐  │     │  ┌──────────▼─────────────┐  │
│  │  Nodes                 │  │     │  │  Nodes                 │  │
│  │  ┌───────┐ ┌───────┐   │  │     │  │  ┌───────┐ ┌───────┐   │  │
│  │  │Reaper │ │Reaper │   │  │     │  │  │NodeAgt│ │NodeAgt│   │  │
│  │  │Process│ │Process│   │  │     │  │  │       │ │       │   │  │
│  │  └───┬───┘ └───┬───┘   │  │     │  │  └───┬───┘ └───┬───┘   │  │
│  │      │         │        │  │     │  │      │         │        │  │
│  │  ┌───▼───┐ ┌───▼───┐   │  │     │  │  ┌───▼───┐ ┌───▼───┐   │  │
│  │  │Shadow │ │Shadow │   │  │     │  │  │Shadow │ │Shadow │   │  │
│  │  │NodeAgt│ │NodeAgt│   │  │     │  │  │Reaper │ │Reaper │   │  │
│  │  └───────┘ └───────┘   │  │     │  │  └───────┘ └───────┘   │  │
│  └─────────────────────────┘  │     │  └─────────────────────────┘  │
└──────────────────────────────┘     └──────────────────────────────┘
```

There are three layers to the integration:

1. **Bridge Protocol** — how the schedulers talk to each other
2. **Node-Level Sidecar Pairing** — how each node participates in both worlds
3. **Job Translation** — how work units map between systems

---

## Layer 1: Bridge Protocol

### BridgeAgent

A new component deployed alongside each scheduler. The two BridgeAgents communicate via a purpose-built gRPC service.

```protobuf
service SchedulerBridge {
  // Capacity advertisement (periodic, every 30s)
  rpc AdvertiseCapacity(CapacityAdvertisement) returns (CapacityAck);

  // Cross-schedule request (one scheduler asks the other to run a job)
  rpc RequestPlacement(PlacementRequest) returns (PlacementResponse);

  // Placement lifecycle
  rpc ReportStatus(StatusUpdate) returns (StatusAck);
  rpc CancelPlacement(CancelRequest) returns (CancelAck);

  // Heartbeat / liveness
  rpc Ping(PingRequest) returns (PongResponse);
}
```

### Capacity Advertisement

Each BridgeAgent periodically advertises **lendable capacity** — the subset of its nodes that the local scheduler has marked as available for cross-scheduling.

```protobuf
message CapacityAdvertisement {
  string site_id = 1;
  string scheduler_type = 2;  // "wren" or "lattice"
  repeated LendableNode nodes = 3;
  google.protobuf.Timestamp timestamp = 4;
}

message LendableNode {
  string node_id = 1;
  uint32 cpu_millis = 2;
  uint64 memory_bytes = 3;
  uint32 gpus = 4;
  string gpu_type = 5;
  string switch_group = 6;  // topology hint
  repeated string features = 7;
}
```

**Lattice side**: The lattice admin configures a `wren_bridge` section in the vCluster config. Idle nodes in that vCluster are advertised. The `max_federation_pct` cap from `FederationConfig` applies — reusing the existing policy.

**Wren side**: A `WrenQueue` with `lendable: true` marks its idle nodes as available for cross-scheduling.

### Placement Request

When a local scheduler cannot place a job (or determines a remote placement scores better), it sends a `PlacementRequest`:

```protobuf
message PlacementRequest {
  string request_id = 1;       // idempotency key
  string origin_site = 2;
  oneof job_definition {
    LatticeAllocationSpec lattice_spec = 3;
    WrenJobSpec wren_spec = 4;
  }
  BridgeJobSpec bridge_spec = 5;  // canonical translation (see Layer 3)
  uint32 node_count = 6;
  uint64 walltime_secs = 7;       // 0 = unbounded
  repeated string topology_hints = 8;
  uint64 ttl_secs = 9;            // offer expiry
}
```

The receiving BridgeAgent:
1. Validates the request (trusted origin, within capacity limits)
2. Translates the `bridge_spec` into a native job (Lattice `Allocation` or Wren `WrenJob`)
3. Submits it to the local scheduler
4. Returns `PlacementResponse` with accept/reject + assigned node IDs

**Decision authority stays local** — the remote scheduler only suggests. This mirrors Lattice's existing `FederationBroker::evaluate_offer` pattern.

### Synchronization via Knapsack Cost Function

When Lattice evaluates whether to accept a Wren placement request, it runs the request through the standard knapsack scoring with an additional cost factor:

```
f₁₀ = cross_scheduler_affinity
```

This factor penalizes cross-scheduled work relative to native work (configurable weight, default 0.0 = no penalty). It allows operators to prefer local scheduling while still enabling overflow.

The knapsack solver treats cross-scheduled placements as regular `Allocation` objects with a `tags["origin"] = "wren:<site_id>"` marker. They participate in the normal scoring, preemption, backfill, and fair-share — no special path.

On the Wren side, cross-scheduled work from Lattice enters as a regular `WrenJob` with annotation `lattice.origin: <site_id>`. Wren's gang scheduler + backfill handle it natively.

---

## Layer 2: Node-Level Sidecar Pairing

This is where the "each reaper spawns a lattice node agent" / "each lattice node spawns a reaper" idea lands.

### Option A: Shadow Sidecars (Recommended)

When a node is lent to the other scheduler, a **shadow sidecar** process starts on that node to provide the borrowing scheduler's node-level interface.

**Wren node lent to Lattice:**
```
Wren Node
├── ReaperProcess (native, always running)
└── ShadowNodeAgent (started by BridgeAgent when node is lent)
    ├── Sends heartbeats to Lattice quorum
    ├── Accepts StartAllocation / StopAllocation commands
    ├── Translates to local Reaper commands
    └── Reports health via Lattice protocol
```

The `ShadowNodeAgent` is a thin shim that implements Lattice's `NodeAgent` protocol but delegates actual execution to the Reaper process already running on the node. It does NOT replace the Reaper — it wraps it.

**Lattice node lent to Wren:**
```
Lattice Node
├── NodeAgent (native, always running)
└── ShadowReaper (started by BridgeAgent when node is lent)
    ├── Registers as ReaperPod target in Wren's K8s API
    ├── Accepts ReaperPod CRDs
    ├── Translates to local NodeAgent commands
    └── Reports pod status back to Wren controller
```

The `ShadowReaper` exposes the ReaperPod CRD interface that Wren expects but delegates to Lattice's NodeAgent for actual process lifecycle.

### Sidecar Lifecycle

```
BridgeAgent receives PlacementRequest
  → local scheduler accepts, assigns nodes [n1, n2]
  → BridgeAgent starts ShadowSidecar on each assigned node
  → ShadowSidecar registers with borrowing scheduler
  → Job executes via ShadowSidecar → native agent translation
  → Job completes → BridgeAgent reports StatusUpdate
  → ShadowSidecar deregisters and stops
  → Nodes return to local scheduler's pool
```

Sidecars are **ephemeral** — they exist only for the duration of a cross-scheduled placement. No permanent coupling.

### Option B: Permanent Dual-Agent (Not Recommended)

Every node runs both a Reaper and a NodeAgent permanently. This creates:
- Constant resource overhead on every node
- Complex conflict resolution when both schedulers try to use the same node
- Split-brain risk without a shared arbiter

Option A avoids all of this by only spawning the foreign agent when the node is actually lent.

---

## Layer 3: Shared Job Definition (BridgeJobSpec)

The two schedulers have different job models:

| Concept | Wren (`WrenJob`) | Lattice (`Allocation`) |
|---|---|---|
| Work unit | WrenJob CRD | Allocation struct |
| Node count | `spec.nodes` (exact) | `resources.nodes` (Exact or Range) |
| Time limit | `spec.walltime` (string) | `lifecycle.Bounded { walltime }` |
| Resources | cpu/mem/gpu per node | `ResourceRequest` + constraints |
| Topology | switch_group labels | dragonfly group model |
| Identity | WrenUser CRD (uid/gid) | OIDC UserId + tenant |
| Execution | Container or Reaper backend | uenv or Sarus |
| Dependencies | afterOk/afterAny/afterNotOk | DAG with 5 condition types |
| State | Pending→Running→Succeeded/Failed | Pending→Staging→Running→Completed/Failed |

### BridgeJobSpec: The Canonical Translation

Rather than forcing one model onto the other, we define a **bridge-neutral** job spec that both sides can translate to/from:

```protobuf
message BridgeJobSpec {
  string name = 1;
  string user_identity = 2;       // opaque user ID
  string project = 3;             // tenant/project mapping

  // Resources (per node)
  uint32 node_count = 4;
  uint64 cpu_millis_per_node = 5;
  uint64 memory_bytes_per_node = 6;
  uint32 gpus_per_node = 7;
  string gpu_type = 8;

  // Execution
  string image = 9;               // container image OR uenv image
  string entrypoint = 10;
  map<string, string> env_vars = 11;
  repeated VolumeMount mounts = 12;

  // Lifecycle
  uint64 walltime_secs = 13;      // 0 = unbounded
  uint32 priority = 14;           // normalized 0-100

  // Topology
  string topology_preference = 15; // "tight", "spread", "any"

  // MPI
  bool mpi_enabled = 16;
  uint32 tasks_per_node = 17;

  // Tags for metadata passthrough
  map<string, string> annotations = 18;
}
```

### Translation Rules

**BridgeJobSpec → Lattice Allocation:**
```rust
fn bridge_to_allocation(spec: &BridgeJobSpec, tenant: &TenantId) -> Allocation {
    Allocation {
        resources: ResourceRequest {
            nodes: NodeCount::Exact(spec.node_count),
            constraints: ResourceConstraints {
                gpu_type: spec.gpu_type.clone(),
                // cpu/mem encoded in resource request
                ..Default::default()
            },
        },
        lifecycle: if spec.walltime_secs > 0 {
            Lifecycle { lifecycle_type: LifecycleType::Bounded {
                walltime: Duration::from_secs(spec.walltime_secs)
            }}
        } else {
            Lifecycle { lifecycle_type: LifecycleType::Unbounded }
        },
        environment: Environment::Uenv { image: spec.image.clone() },
        entrypoint: spec.entrypoint.clone(),
        tags: spec.annotations.clone(),
        // ... remaining fields from defaults + tenant config
    }
}
```

**BridgeJobSpec → Wren WrenJob:**
```rust
fn bridge_to_wrenjob(spec: &BridgeJobSpec) -> WrenJobSpec {
    WrenJobSpec {
        nodes: spec.node_count,
        tasks_per_node: spec.tasks_per_node,
        walltime: format_walltime(spec.walltime_secs),
        image: spec.image.clone(),
        command: vec![spec.entrypoint.clone()],
        resources: WrenResources {
            cpu: format!("{}m", spec.cpu_millis_per_node),
            memory: format!("{}Gi", spec.memory_bytes_per_node / (1024*1024*1024)),
            gpus: spec.gpus_per_node,
        },
        // ... remaining fields
    }
}
```

**Lossy translation is acceptable** — features that don't map (e.g., Lattice's DAG dependencies, Wren's SSH auth) stay on the originating side. The bridge only forwards what the receiving scheduler can execute.

---

## Synchronization Protocol

### State Machine for Cross-Scheduled Jobs

```
                  ┌──────────────────┐
  PlacementRequest│                  │PlacementResponse
  ───────────────►│  BRIDGE_PENDING  │───────────────►
                  │                  │  (Accept/Reject)
                  └────────┬─────────┘
                           │ Accept
                  ┌────────▼─────────┐
                  │  BRIDGE_STAGING  │  Shadow sidecar starting
                  └────────┬─────────┘
                           │ Sidecar ready
                  ┌────────▼─────────┐
                  │  BRIDGE_RUNNING  │  StatusUpdate(Running)
                  └────────┬─────────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
    ┌─────────▼──┐  ┌──────▼─────┐  ┌──▼──────────┐
    │  COMPLETED │  │   FAILED   │  │  CANCELLED  │
    │            │  │            │  │  (either    │
    │            │  │            │  │   side)     │
    └────────────┘  └────────────┘  └─────────────┘
```

### Consistency Guarantees

- **Node ownership is always strong-consistent on the owning side.** When Lattice lends a node, Lattice's Raft quorum records `ownership.is_borrowed = true`. The node cannot be double-booked.
- **Cross-scheduler state is eventually consistent.** The BridgeAgent polls/streams status updates. A 30s staleness window is acceptable.
- **Conflict resolution**: If the owning scheduler needs a lent node back (e.g., preemption of a higher-priority local job), it sends a `CancelPlacement` to the borrowing scheduler. The borrowing scheduler has a grace period (configurable, default 60s) to checkpoint and vacate.

### Failure Modes

| Failure | Response |
|---|---|
| Bridge gRPC connection lost | Both sides mark cross-scheduled jobs as `BRIDGE_DEGRADED`. If not restored within `bridge_timeout` (default 5min), owning side reclaims nodes, borrowing side marks jobs failed. |
| Shadow sidecar crash | Owning side's native agent detects orphaned processes. BridgeAgent restarts sidecar or fails the placement. |
| Owning scheduler down | Shadow sidecars continue running (they talk to the native agent). Jobs complete normally. Status reporting resumes when bridge reconnects. |
| Borrowing scheduler down | Native agent keeps processes alive. No new commands arrive. On reconnect, status sync catches up. |

---

## Implementation Plan

### Phase 1: Bridge Protocol (lattice-bridge crate)

New crate: `crates/lattice-bridge/`
- `proto/bridge.proto` — gRPC service definition
- `src/agent.rs` — BridgeAgent (connects to local scheduler + remote bridge)
- `src/config.rs` — BridgeConfig (remote endpoint, TLS, capacity limits)
- `src/translate.rs` — BridgeJobSpec ↔ Allocation translation
- Feature-gated: `#[cfg(feature = "wren")]`

### Phase 2: Shadow Sidecars

- `src/shadow_node_agent.rs` — ShadowNodeAgent (Lattice NodeAgent shim over Reaper)
- `src/shadow_reaper.rs` — ShadowReaper (Wren ReaperPod shim over NodeAgent)
- Sidecar binary: `lattice-bridge-sidecar` (mode selected at launch)

### Phase 3: Knapsack Integration

- Add `f₁₀ = cross_scheduler_affinity` to `CostWeights`
- BridgeAgent implements `SchedulerStateReader` to inject lendable remote nodes as virtual nodes
- Remote nodes appear in the knapsack solver with a configurable affinity penalty

### Phase 4: Wren-Side Changes (upstream contribution)

- `ExecutionBackend` impl for `LatticeBridgeBackend`
- `WrenQueue` annotation for lendable capacity
- BridgeAgent K8s controller on the Wren side

---

## What This Design Does NOT Do

- **No shared Raft** — Wren doesn't join Lattice's quorum or vice versa
- **No permanent dual-agent** — shadow sidecars are ephemeral
- **No shared database** — each scheduler owns its state
- **No K8s dependency in Lattice** — bridge translates; Lattice never touches the K8s API
- **No Lattice dependency in Wren** — Wren never touches Raft or gRPC directly; the bridge handles it
- **No forced job model** — BridgeJobSpec is a translation layer, not a replacement

## Open Questions

1. **MPI across scheduler boundaries** — If a cross-scheduled job spans nodes from both clusters, who generates the hostfile? Proposal: the originating scheduler generates it since it knows all assigned nodes.
2. **Checkpoint across boundaries** — Can a job checkpointed on a Wren Reaper node be restored on a Lattice NodeAgent node? Requires a shared checkpoint format (DMTCP is backend-agnostic, so likely yes).
3. **Fair-share accounting** — Cross-scheduled GPU-hours: charged to which tenant? Proposal: charged to the tenant on the originating side; the receiving side tracks it as "federation usage."
4. **Topology scoring across clusters** — Dragonfly groups don't extend across clusters. Cross-cluster topology score should be a flat penalty (configurable).

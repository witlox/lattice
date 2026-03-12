# Module Map

Module boundaries, responsibilities, and ownership. Each module maps to a Rust crate in the workspace. Boundaries follow bounded context lines from `specs/domain-model.md`, with two cross-cutting modules (common types and CLI).

## Module Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         lattice-cli                                     │
│  CLI client + Slurm compat layer                                        │
│  Stateless. Consumes gRPC services.                                     │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │ gRPC
┌───────────────────────────────┴─────────────────────────────────────────┐
│                         lattice-api                                      │
│  gRPC + REST gateway. Middleware (OIDC, RBAC, rate limit).               │
│  Owns: API surface, request validation, event streaming.                 │
│  Context: spans all contexts (entry point).                              │
└──────┬──────────┬──────────┬───────────────┬───────────────┬────────────┘
       │          │          │               │               │
       ▼          ▼          ▼               ▼               ▼
┌───────────┐ ┌──────────────┐ ┌──────────────────┐ ┌──────────────────┐
│  lattice- │ │   lattice-   │ │    lattice-      │ │    lattice-      │
│  quorum   │ │  scheduler   │ │  checkpoint      │ │  node-agent      │
│           │ │              │ │                  │ │                  │
│ Raft      │ │ Cost fn,     │ │ Cost model,      │ │ Per-node daemon, │
│ consensus,│ │ knapsack,    │ │ policy eval,     │ │ runtime, telem,  │
│ global    │ │ placement,   │ │ signal delivery  │ │ health, PMI-2    │
│ state     │ │ preemption   │ │                  │ │                  │
│           │ │              │ │                  │ │                  │
│ Context:  │ │ Context:     │ │ Cross-context:   │ │ Context:         │
│ Consensus │ │ Scheduling   │ │ Sched ↔ Node Mgmt│ │ Node Management  │
└─────┬─────┘ └──────┬───────┘ └────────┬─────────┘ └────────┬─────────┘
      │               │                  │                     │
      └───────────────┴──────────────────┴─────────────────────┘
                                │
                    ┌───────────┴───────────┐
                    │   lattice-common      │
                    │                       │
                    │ Domain types, traits,  │
                    │ config, error, proto,  │
                    │ external clients       │
                    │                       │
                    │ Context: shared kernel │
                    └───────────────────────┘
```

## Module Responsibilities

### lattice-common (Shared Kernel)

**Bounded context:** None — shared kernel across all contexts.

**Owns:**
- Core domain types: `Allocation`, `Node`, `Tenant`, `VCluster`, `NetworkDomain`, `TopologyModel`, `MemoryTopology`
- State machines: `AllocationState`, `NodeState`
- Trait definitions for all external integration points
- Configuration model (`LatticeConfig` and sub-configs)
- Unified error enum (`LatticeError`)
- Protobuf bindings (generated from `proto/`)
- Prometheus metric definitions
- External HTTP clients: OpenCHAMI, VAST, Sovra, Waldur

**Does NOT own:** Business logic. No command processing, no scheduling decisions, no state mutation.

**Justification:** Single source of truth for types prevents duplication and inconsistency across crates. Trait definitions here allow compile-time interface enforcement. External clients live here because multiple crates need storage/infra/accounting access.

---

### lattice-quorum (Consensus Context)

**Bounded context:** Consensus

**Owns:**
- Raft state machine (`GlobalState`): node ownership, allocation states, tenant configs, vCluster configs, topology, audit log, network domains
- Command processing: proposal validation, invariant enforcement (INV-S1 through INV-S6)
- `QuorumClient`: the programmatic interface to propose state changes and read committed state
- Backup/restore: snapshot export, restore, verification
- Raft transport: gRPC-based log replication, leader election, snapshot install

**Implements traits:** `NodeRegistry`, `AllocationStore`, `AuditLog`

**Does NOT own:** Scheduling decisions, telemetry, node agent lifecycle.

**Key constraint:** This is the ONLY module that mutates strongly-consistent state. All other modules either propose changes (via `Command`) or read committed state.

---

### lattice-scheduler (Scheduling Context)

**Bounded context:** Scheduling

**Owns:**
- Cost function: 9-factor composite scoring with per-vCluster weights
- Knapsack solver: greedy topology-aware placement with backfill
- Per-vCluster scheduler implementations: HPC backfill, service bin-pack, sensitive reservation, interactive FIFO
- Scheduling cycle: periodic evaluation of pending allocations
- Preemption evaluation: class-based victim selection with checkpoint awareness
- DAG controller: dependency edge evaluation, stage progression
- Quota evaluation: hard/soft quota checks
- Autoscaler: reactive allocation node count adjustment
- Borrowing broker: inter-vCluster elastic lending
- Federation broker: cross-site offer evaluation
- Walltime enforcer: expiry timer management

**Does NOT own:** State persistence (reads from quorum), node agent communication (proposes via quorum), telemetry collection.

**Stateless:** Reads pending allocations and node state from quorum. Produces `PlacementDecision` values. Crash recovery is re-read + re-evaluate.

---

### lattice-api (API Gateway — spans all contexts)

**Bounded context:** None — entry point spanning all contexts.

**Owns:**
- 3 gRPC services: AllocationService (18 RPCs), NodeService (7 RPCs), AdminService (8 RPCs)
- REST gateway: 30+ routes mirroring gRPC surface
- Middleware stack: OIDC validation, RBAC enforcement, rate limiting
- Request validation: admission checks before proposals reach quorum
- Event bus: streaming state changes (Watch, StreamLogs, StreamMetrics)
- `ApiState`: composition root wiring all trait implementations together

**Does NOT own:** Business logic, scheduling decisions, state mutation (delegates to quorum/scheduler/node-agent).

**Key design:** `ApiState` is a trait-object bag. Each field is `Arc<dyn Trait>`. This enables test injection and keeps the API layer agnostic to implementations.

---

### lattice-node-agent (Node Management Context)

**Bounded context:** Node Management

**Owns:**
- Runtime abstraction: uenv, Sarus, DMTCP execution
- Prologue/epilogue: image pull, mount, data staging, scratch setup, cleanup
- Allocation runner: per-allocation state machine on a single node
- Heartbeat loop: periodic health reporting to quorum
- Telemetry collection: /proc parsing, GPU discovery (NVIDIA/AMD), memory topology discovery, eBPF programs
- Log buffer: per-allocation ring buffer + S3 sink
- Checkpoint signal handler: SIGUSR1/SIGUSR2 forwarding to application
- Attach/PTY: interactive terminal via nsenter
- PMI-2 server: MPI process management, cross-node KVS exchange
- Network management: VNI configuration for Slingshot domains
- Data staging execution: VAST API calls for pre-stage/QoS/cleanup
- Conformance fingerprint computation

**Does NOT own:** Scheduling decisions, global state, audit log writes (reports to quorum, quorum commits).

**Key autonomy:** Node agents continue running allocations during quorum outage. Heartbeats are buffered and replayed on reconnection. Grace periods prevent premature Down transitions.

---

### lattice-checkpoint (Cross-Context Coordinator)

**Bounded context:** None — cross-context coordinator (Scheduling ↔ Node Management).

**Owns:**
- Checkpoint cost model: Value (recompute_saved + preemptability + backlog_relief) vs. Cost (write_time + compute_waste + storage_cost)
- Policy evaluation: per-allocation checkpoint timing
- `NodeAgentPool` trait: interface for delivering checkpoint signals to node agents

**Stateless:** Evaluates current state, produces checkpoint requests. Crash recovery is re-evaluation.

**Justification as separate module:** Checkpoint decisions require inputs from both Scheduling (queue depth, preemption demand) and Node Management (health, storage bandwidth). Keeping it separate prevents either context from taking on cross-context coordination logic.

---

### lattice-cli (User-Facing Client)

**Bounded context:** None — client-side.

**Owns:**
- CLI command structure (clap): submit, status, cancel, nodes, admin, attach, logs, top, watch, diag, session, dag
- `LatticeGrpcClient`: typed wrapper around 3 tonic service clients
- Slurm compatibility translation: sbatch/squeue/scancel → Intent API
- DAG YAML parsing and validation
- Output formatting: JSON, YAML, table
- Shell completions

**Stateless:** All state lives on the server. CLI is a thin translation layer.

---

### Supporting Modules

**lattice-test-harness** — Test utilities, mock implementations for all traits. Not deployed.

**lattice-acceptance** — Cucumber/Gherkin BDD test harness. 26 feature files, 90+ scenarios. Not deployed.

## Context-to-Module Mapping

| Bounded Context | Primary Module | Supporting Modules |
|---|---|---|
| Scheduling | lattice-scheduler | lattice-api (RPCs), lattice-common (types) |
| Consensus | lattice-quorum | lattice-common (types, traits) |
| Node Management | lattice-node-agent | lattice-api (RPCs), lattice-common (clients) |
| Observability | lattice-node-agent (collection) + lattice-api (query/stream) | External: VictoriaMetrics, S3 |
| Tenant & Access | lattice-api (RBAC/OIDC) + lattice-quorum (quota storage) | lattice-common (types) |
| Federation | lattice-scheduler (broker) + lattice-api (RPCs) | lattice-common (Sovra client) |

**Note:** Observability and Tenant & Access do not have dedicated crates. Observability collection lives in lattice-node-agent; query/streaming lives in lattice-api. Tenant & Access enforcement is split between lattice-api (RBAC middleware) and lattice-quorum (hard quota validation). This is intentional — these contexts are thin enough that separate crates would be over-modularization.

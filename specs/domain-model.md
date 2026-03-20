# Domain Model

## Bounded Contexts

Lattice decomposes into six bounded contexts. Each context owns its domain entities and exposes integration surfaces to other contexts. Security/isolation boundaries are orthogonal to these contexts — they are enforced by Tenant identity, Network Domains, and RBAC, not by context boundaries.

```
┌─────────────────────────────────────────────────────────────────┐
│                        SCHEDULING                               │
│  Owns: Allocation, vCluster, CostFunction, Knapsack Solver     │
│  Owns: DAG, TaskGroup, Dependency, PreemptionPolicy            │
│  Coordinates: proposes node ownership changes to Consensus      │
└──────────┬──────────────────────────────┬───────────────────────┘
           │ proposes                     │ scores using
           ▼                             ▼
┌─────────────────────┐    ┌──────────────────────────────────────┐
│     CONSENSUS       │    │          OBSERVABILITY               │
│  Owns: GlobalState, │    │  Owns: Telemetry pipeline,           │
│  NodeOwnership,     │    │  LogBuffer, TSDB queries,            │
│  SensitiveAuditLog  │    │  AttachSession, Diagnostics          │
│  Raft log, Snapshot │    │                                      │
└──────────┬──────────┘    └──────────────────────────────────────┘
           │ committed state
           ▼
┌─────────────────────────────────────────────────────────────────┐
│                     NODE MANAGEMENT                              │
│  Owns: Node lifecycle (state machine), NodeAgent,               │
│  Heartbeat, Conformance, GPU/Memory discovery,                  │
│  Runtime (uenv/Sarus/DMTCP), Prologue/Epilogue,                │
│  DataStaging, MPI/PMI-2 process management                     │
└─────────────────────────────────────────────────────────────────┘

┌──────────────────────┐    ┌─────────────────────────────────────┐
│   TENANT & ACCESS    │    │         FEDERATION                  │
│  Owns: Tenant, Quota,│    │  Owns: FederationBroker,            │
│  RBAC, OIDC identity,│    │  FederationCatalog, Sovra trust,    │
│  User, Role          │    │  cross-site routing, data gravity   │
│                      │    │  Feature-gated: compile-time opt-in │
└──────────────────────┘    └─────────────────────────────────────┘

Cross-context coordination mechanisms (not contexts themselves):
  - Checkpoint Broker: reads Scheduling state, acts through Node Management
  - Accounting (Waldur): reads Scheduling events, feeds back to Tenant & Access
```

## Context Relationships

| Upstream | Downstream | Relationship | Integration |
|---|---|---|---|
| Consensus | Scheduling | Customer-Supplier | Scheduling proposes; Consensus validates and commits |
| Consensus | Node Management | Customer-Supplier | Node agents report to Consensus; Consensus records ownership |
| Scheduling | Node Management | Customer-Supplier | Scheduler assigns work; node agents execute |
| Tenant & Access | Scheduling | Conformist | Scheduling conforms to quota limits and RBAC decisions |
| Tenant & Access | Consensus | Conformist | Consensus enforces hard quotas during proposal validation |
| Observability | Scheduling | Supplier | TSDB provides cost function inputs (f5 data readiness, f7 energy) |
| Node Management | Observability | Supplier | Node agents produce telemetry; Observability collects/stores |
| Federation | Scheduling | Anticorruption Layer | Federation broker translates cross-site requests into local allocations |
| Accounting (Waldur) | Tenant & Access | Anticorruption Layer | Waldur quota updates translated to Lattice quota model |

## Core Entities

### Scheduling Context

**Allocation** — The universal work unit. Replaces both Slurm jobs and Kubernetes pods.
- Identity: `AllocId` (UUID), belongs to one `Tenant`, one `Project`, one `vCluster`, submitted by one `User`
- Lifecycle variant: `Bounded` (has walltime) | `Unbounded` (runs until cancelled, auto-restarts) | `Reactive` (autoscales on metric threshold)
- State machine: Pending → Staging → Running → Checkpointing → Suspended → Completed | Failed | Cancelled
  - Failed → Pending: allowed for Unbounded/Reactive allocations via reconciliation (service self-healing)
- Composes into: TaskGroup (array of identical allocations) or DAG (dependency graph)
- Has: ResourceRequest, Environment, Connectivity, DataRequirements, CheckpointStrategy, RequeuePolicy, LivenessProbe (optional)
- Service allocations expose named endpoints (ServiceEndpoint) and register in the ServiceRegistry when Running

**LivenessProbe** — Health check for service allocations.
- Type: TCP (connect to port) or HTTP (GET request, expect 2xx)
- Configuration: period_secs, initial_delay_secs, failure_threshold, timeout_secs
- Managed by ProbeManager in node agent; failures mark allocation as Failed, triggering reconciliation

**vCluster** — A scheduling policy container that projects a view of shared resources.
- Evolution of the CSCS vCluster concept (Martinasso et al., CiSE 2024). CSCS vClusters are immutable IaC-defined platforms with independent Slurm instances and static node sets. Lattice vClusters are dynamic policy boundaries within a shared quorum: mutable cost function weights, elastic node borrowing, and global state visibility.
- Each vCluster has: scheduler algorithm (backfill, bin-pack, reservation, FIFO), cost function weights, base node allocation (guaranteed), borrowing policy
- vClusters are NOT security boundaries — Tenant + Network Domain provide isolation
- M:N with Tenants: a Tenant can submit to multiple vClusters; a vCluster serves multiple Tenants
- Quotas are per-Tenant (global), not per-vCluster

**DAG** — A directed acyclic graph of Allocations with dependency edges.
- Edges carry conditions: Success (afterok), Failure (afternotok), Any (afterany), Corresponding (aftercorr), Mutex (singleton)
- Root allocations enter scheduler queue immediately; downstream allocations enter when conditions are met
- DAG state is eventually consistent (managed by DAG controller, not Raft)
- Max 1000 allocations per DAG (configurable)

**TaskGroup** — Equivalent of Slurm job arrays. A template Allocation instantiated over an index range with configurable concurrency.

**CostFunction** — Composite weighted scoring function with 9 factors:
- f1: priority_class, f2: wait_time_factor, f3: fair_share_deficit, f4: topology_fitness, f5: data_readiness, f6: backlog_pressure, f7: energy_cost, f8: checkpoint_efficiency, f9: conformance_fitness
- Weights tunable per vCluster via RM-Replay simulation

**PreemptionPolicy** — Class-based (0-10) preemption with checkpoint-aware victim selection.

### Consensus Context

**GlobalState** — The Raft-replicated state machine.
- Contains: node ownership map, allocation states, tenant states, vCluster configs, topology model, sensitive audit log, service registry, sessions
- Strong consistency: (1) node ownership, (2) sensitive audit events, (3) service registry, (4) sessions
- Eventually consistent: job queues, telemetry, quota accounting

**ServiceRegistry** — Auto-populated map from service name to registered endpoints.
- Endpoints registered when allocation with `expose` transitions to Running
- Deregistered on terminal state (Completed/Failed/Cancelled) or requeue
- Query: LookupService (by name, tenant-filtered), ListServices (tenant-filtered)
- Entries are tenant-scoped: cross-tenant lookups filtered for isolation

**Session** — Interactive attach session tracked globally for concurrency control.
- Identity: `SessionId` (UUID), links to `AllocId` and `UserId`
- Created via Raft command (CreateSession), deleted via Raft command (DeleteSession)
- Sensitive allocations: at most one active session globally (INV-C2, enforced in GlobalState)
- Survives API server restart (persisted in Raft state)

**NodeOwnership** — Which Tenant/vCluster/Allocation owns which nodes. Raft-committed. Cannot be violated even momentarily.

**SensitiveAuditLog** — Append-only, cryptographically signed, tamper-evident log of all sensitive workload events. 7-year retention. Events use standardized `hpc_audit::AuditEvent` schema wrapped in ed25519-signed envelope. `CompliancePolicy::regulated()` preset defines required audit points.

### Node Management Context

**Node** — A physical compute node identified by xname (e.g., `x1000c0s0b0n0`).
- State machine: Unknown → Booting → Ready ⇄ Degraded → Down; Ready → Draining → Drained → Ready; Booting → Failed
- Has: NodeCapabilities (GPU type/count, memory, features), ConformanceFingerprint, TopologyPosition (dragonfly group), MemoryTopology
- Sensitive nodes have extended grace periods, mandatory wipe-on-release, strict conformance enforcement

**NodeAgent** — Per-node daemon managing runtime lifecycle.
- Owns: uenv mount, Sarus container, DMTCP checkpoint, eBPF telemetry loading, health reporting, PMI-2 server
- Executes prologue (image pull, mount, data stage) and epilogue (cleanup, scratch wipe)
- Implements hpc-node contracts: `CgroupManager` (standalone), `NamespaceConsumer` (dual-mode), `MountManager` (dual-mode)
- Dual-mode operation: detects PACT presence at runtime. When PACT is present, delegates namespace creation and mount management to PACT via hpc-node handoff protocol. When standalone, self-services all resource isolation.
- Uses `IdentityCascade` (hpc-identity) for mTLS workload identity: SPIRE → self-signed → bootstrap cert
- Emits `AuditEvent` (hpc-audit) for resource isolation operations, wrapped in signed Raft envelope

**ConformanceFingerprint** — SHA-256 hash of GPU driver, NIC firmware, BIOS, kernel version. Nodes with identical fingerprints form conformance groups. Used by scheduler f9 to prefer homogeneous placement.

**MemoryTopology** — NUMA domains, memory interconnects (UPI, CXL, NVLink), superchip detection (GH200, MI300A). Informs scheduler f4 and prologue numactl policy.

**MPI ProcessManager** — Native PMI-2 server over Unix domain socket. Cross-node KV exchange (fence) via gRPC between node agents. Optional PMIx sidecar behind feature flag.

### Tenant & Access Context

**Tenant** — Organizational boundary. Owns projects, users, quotas.
- Hard quotas (Raft-enforced): max_nodes, max_concurrent_allocations, sensitive_pool_size
- Soft quotas (scheduler-enforced): gpu_hours_budget, fair_share_target, burst_allowance
- Quotas are global (not per-vCluster)

**User** — Authenticated via OIDC. Identity is the OIDC subject claim. For sensitive workloads, the User (not Tenant) claims nodes.

**RBAC** — Four roles: user, tenant-admin, system-admin, claiming-user. Role determines API visibility and operation permissions across 27 defined operations.

### Observability Context

**TelemetryPipeline** — Three-layer: eBPF collection (<0.3% overhead) → node agent aggregation (configurable resolution) → external TSDB (VictoriaMetrics).

**LogBuffer** — Dual-path: ring buffer in node agent (live streaming) + S3 (persistent storage). Sensitive logs encrypted, access-logged.

**AttachSession** — Interactive terminal via nsenter into allocation namespace. Bidirectional gRPC stream. Sensitive: one session at a time, recorded, claiming user only.

**Diagnostics** — Network health, storage performance, cross-allocation metric comparison. Sensitive: no cross-tenant comparison.

### Federation Context (feature-gated)

**FederationBroker** — Go service that advertises capacity, routes cross-site requests, signs/verifies Sovra tokens. Suggests routing; local scheduler decides.

**FederationCatalog** — Eventually consistent shared catalog: site capabilities, uenv registry, dataset locations, energy prices, tenant identity mapping.

**Sovra Trust** — Cryptographic trust via sovereign key management. Site root keys never leave the site. Federation revocable by workspace revocation.

### hpc-core Shared Contracts (External)

Lattice depends on four shared contract crates from the hpc-core workspace (published to crates.io). These define trait-only interfaces with no implementations — both PACT and Lattice implement the traits independently. No code coupling exists between the two systems.

**hpc-node** — Cgroup hierarchy (`CgroupManager`), namespace handoff (`NamespaceProvider`/`NamespaceConsumer`), refcounted mounts (`MountManager`), boot readiness (`ReadinessGate`). Defines well-known filesystem paths (`workload.slice/`, `/run/pact/handoff.sock`, `/run/pact/uenv/`). Lattice implements `CgroupManager` and `NamespaceConsumer` in lattice-node-agent.

**hpc-audit** — Universal audit event format (`AuditEvent`), sink trait (`AuditSink`), 40+ well-known action constants, compliance policy (`CompliancePolicy`). Includes `AuditSource` enum with variants for both systems. Lattice wraps `AuditEvent` in signed Raft envelope.

**hpc-identity** — Workload identity cascade (`IdentityCascade`: SPIRE → self-signed → bootstrap), certificate rotation (`CertRotator`: dual-channel non-disruptive swap), identity provider trait (`IdentityProvider`). Used by both lattice-node-agent (per-node mTLS on HSN) and lattice-quorum (inter-replica mTLS). SPIRE is the primary provider on HPE Cray deployments. Lattice-quorum acts as ephemeral self-signed CA (same model as PACT ADR-008) when SPIRE is not deployed. Trust domains are separate from PACT even when co-deployed.

**hpc-auth** — OAuth2/OIDC token management (`AuthClient`), cascading flows (PKCE → Device Code → Manual Paste → Client Credentials), per-server token caching, OIDC discovery. Used by lattice-cli for user authentication.

**Dual-mode operation model:**

| Capability | Standalone (no PACT) | PACT-managed (supercharged) |
|---|---|---|
| Cgroup hierarchy | lattice-node-agent creates `workload.slice/` | PACT creates both slices at boot |
| Namespace creation | Self-service via `unshare(2)` | Handed off from PACT via SCM_RIGHTS |
| uenv mounts | One mount per allocation | Shared refcounted mounts (cache locality) |
| Boot readiness | Ready at startup | Waits for PACT readiness signal |
| Audit trail | Lattice quorum only | Both pact-journal + lattice-quorum, unified format |
| mTLS identity | IdentityCascade (same) | IdentityCascade (same, shared trust bundle) |
| Resource limits | Lattice writes cgroup limits | PACT creates scopes, Lattice writes limits |

## Cross-Context Coordination Mechanisms

**Checkpoint Broker** — Reads scheduling state (queue depth, preemption demand, cost function) and node state (health, storage bandwidth). Evaluates `Value > Cost` per running allocation. Acts through node agents (sends CHECKPOINT_HINT). Stateless — crash recovery is re-evaluation on restart.

**Accounting (Waldur)** — Receives async events from Scheduling (allocation.started, allocation.completed). Feeds quota updates back to Tenant & Access. Feature-gated. Failure never blocks scheduling. Events buffered in memory + disk WAL.

**Data Staging** — Reads allocation data requirements (Scheduling). Executes via storage APIs (VAST) during queue wait. Reports readiness back to Scheduling (f5 score). Runs within Node Management during prologue.

## Aggregate Boundaries

| Aggregate | Root Entity | Consistency | Owned By |
|---|---|---|---|
| Allocation lifecycle | Allocation | Eventually consistent (queue); strong (node ownership commit) | Scheduling |
| Node state | Node | Strong (ownership); eventual (capacity/health) | Consensus + Node Management |
| Tenant quotas | Tenant | Strong (hard quotas); eventual (soft quotas) | Tenant & Access + Consensus |
| Sensitive audit | AuditEvent | Strong (Raft-committed, append-only) | Consensus |
| DAG workflow | DAG | Eventually consistent | Scheduling |
| Network Domain | NetworkDomain | Eventually consistent (VNI assignment) | Node Management |
| vCluster config | vCluster | Eventually consistent | Scheduling |
| Federation catalog | FederationCatalog | Eventually consistent | Federation |

## Key Domain Rules

1. **Allocation is the universal work unit.** There is no separate "job" and "service" concept. Lifecycle variant determines duration semantics.
2. **Node ownership is the critical consistency boundary.** A node is owned by exactly one (Tenant, vCluster, Allocation) tuple or unowned. This is the only thing that MUST be strongly consistent for correctness.
3. **vClusters are policy boundaries, not isolation boundaries.** Security comes from Tenant + Network Domain + RBAC.
4. **Network Domains are orthogonal to vClusters.** Two allocations in different vClusters but the same Tenant can share a Network Domain. This is intentional — it enables optimal resource sharing across scheduling policies while maintaining network reachability.
5. **Sensitive workloads are risk-averse by design.** No clever optimizations. User (not Tenant) claims nodes. No sharing, no borrowing, no preemption. Wipe on release.
6. **Federation is always optional.** The system is fully functional without it. When enabled, sovereignty is preserved — the local scheduler always has final say.
7. **PACT is complementary, never required.** Lattice is fully functional standalone. When PACT is co-deployed, Lattice gains enhanced capabilities (faster boot, shared mounts, unified audit, integrated namespace management) via hpc-core trait contracts. Detection is automatic (socket/file probes), not configured. Both systems are independently developed with zero code coupling.
8. **Shared contracts via hpc-core are trait-only.** hpc-node, hpc-audit, hpc-identity, and hpc-auth define interfaces, not implementations. Each system implements traits independently. Breaking changes affect both systems equally — version coordination is required.

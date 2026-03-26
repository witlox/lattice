# Dependency Graph

Module dependencies with justification. Every edge traces to a specification requirement.

## Compile-Time Dependencies

```
lattice-cli ──────────► lattice-common (types for display conversion)
     │
     ├─ tonic client ──► proto/ (gRPC stubs)
     │
     └─────────────────► hpc-auth (OAuth2 token management)

lattice-api ──────────► lattice-common (types, traits, config, error, proto, clients)
     │
     ├─────────────────► lattice-quorum (QuorumClient for proposals + reads)
     │
     ├─────────────────► lattice-scheduler (scheduling cycle, DAG controller)
     │
     └─────────────────► lattice-checkpoint (CheckpointBroker trait impl)

lattice-quorum ───────► lattice-common (types, traits, config)
     │
     └─────────────────► openraft + raft-hpc-core (Raft infrastructure)

lattice-scheduler ────► lattice-common (types, traits)

lattice-checkpoint ───► lattice-common (types, traits)

lattice-node-agent ───► lattice-common (types, traits, config, clients, proto)
     │
     ├─────────────────► hpc-node (CgroupManager, NamespaceConsumer, MountManager, ReadinessGate)
     │
     └─────────────────► hpc-identity (IdentityCascade, CertRotator)

lattice-common ───────► hpc-audit (AuditEvent, AuditSink, actions, CompliancePolicy)
```

## Dependency Justification

### lattice-api → lattice-quorum

**Spec source:** IP-01 (Scheduling → Consensus proposals), IP-02 (Consensus → Node Management notifications), IP-07 (Tenant & Access → Consensus quotas)

**Why:** API server submits `Command` values to the quorum via `QuorumClient.propose()`. Reads committed state for query endpoints. This is the primary write path for all state mutations.

**Nature:** Direct method call (in-process). API server and quorum run in the same process for the `QuorumMember` role.

---

### lattice-api → lattice-scheduler

**Spec source:** Scheduling context owns the cost function and placement algorithm. API server delegates scheduling decisions.

**Why:** API server drives scheduling cycles and DAG controller evaluation. Uses `run_cycle()` and scheduler factory.

**Nature:** Direct method call (in-process). Scheduler is stateless — API server provides inputs, scheduler returns decisions.

---

### lattice-api → lattice-checkpoint

**Spec source:** IP-04 Path B (checkpoint/preemption flow). Checkpoint broker evaluates and initiates checkpoints.

**Why:** API server hosts the checkpoint broker. Checkpoint evaluation is triggered during scheduling cycles when preemption is needed.

**Nature:** Trait-based (`CheckpointBroker`). API server holds `Arc<dyn CheckpointBroker>`.

---

### lattice-api → lattice-common

**Spec source:** All contexts use shared types.

**Why:** Types (`Allocation`, `Node`, `Tenant`), traits (`NodeRegistry`, `AllocationStore`, `AuditLog`), config (`LatticeConfig`), error (`LatticeError`), proto bindings, and external clients are all defined in lattice-common.

**Nature:** Type imports. This is the shared kernel dependency.

---

### lattice-quorum → lattice-common

**Spec source:** Consensus context stores and validates domain entities.

**Why:** `GlobalState` contains `HashMap<AllocId, Allocation>`, `HashMap<NodeId, Node>`, etc. Command processing validates against domain types.

**Nature:** Type imports.

---

### lattice-scheduler → lattice-common

**Spec source:** Scheduling context reads node/allocation state, produces placement decisions.

**Why:** Cost function scores `Allocation` values against `Node` values. Knapsack solver operates on these types.

**Nature:** Type imports only. Scheduler does NOT depend on quorum — it receives state as function arguments.

---

### lattice-checkpoint → lattice-common

**Spec source:** Checkpoint broker reads allocation state and produces checkpoint requests.

**Why:** Cost model evaluates `Allocation` and `CheckpointStrategy`. `NodeAgentPool` trait defined in common.

**Nature:** Type and trait imports.

---

### lattice-node-agent → lattice-common

**Spec source:** Node Management context executes allocations using domain types, reports to quorum via gRPC.

**Why:** Runtime operates on `Allocation`, `Node`, `PrepareConfig`. Heartbeat carries `NodeId`, conformance fingerprint. gRPC client uses proto bindings.

**Nature:** Type imports + gRPC client stubs.

---

### lattice-cli → lattice-common

**Spec source:** CLI displays domain entities.

**Why:** Display conversion from proto types to user-friendly output.

**Nature:** Type imports only. CLI does NOT depend on any server-side crate.

### lattice-cli → hpc-auth

**Spec source:** INV-A1 (unauthenticated command allowlist), IP-14 (CLI → AuthClient token management), A-Auth1 (hpc-auth availability)

**Why:** CLI needs OAuth2 token management: login, logout, automatic refresh, per-server caching. `AuthClient` provides cascading flow selection (PKCE, Device Code, Manual Paste, Client Credentials) and OIDC discovery.

**Nature:** Library dependency. `AuthClient` instantiated at CLI startup, used for all authenticated commands.

---

### lattice-node-agent → hpc-node

**Spec source:** IP-11 (namespace handoff), INV-RI1 (cgroup ownership), INV-RI2 (handoff fallback), A-HC1/A-HC2 (dual-mode operation)

**Why:** Node agent implements `CgroupManager` for standalone resource isolation, `NamespaceConsumer` for dual-mode namespace management (PACT handoff or self-service), `MountManager` for uenv mount lifecycle. Well-known paths prevent convention drift with PACT.

**Nature:** Trait imports. Node agent provides implementations; hpc-node provides only interfaces and constants.

---

### lattice-node-agent → hpc-identity

**Spec source:** IP-13 (mTLS via identity cascade), INV-ID1 (cascade order), INV-ID2 (private key locality), INV-ID3 (non-disruptive rotation)

**Why:** Node agent needs mTLS workload identity for gRPC communication with quorum (heartbeats, registration). `IdentityCascade` provides SPIRE → self-signed → bootstrap fallback. `CertRotator` enables zero-downtime cert rotation.

**Nature:** Library dependency. `IdentityCascade` constructed at startup; `CertRotator` runs on scheduled interval.

---

### lattice-common → hpc-audit

**Spec source:** INV-AF1 (standardized schema), INV-AF2 (well-known actions), INV-AF3 (non-blocking sink), IP-12 (audit forwarding)

**Why:** Common crate owns the `AuditEntry` signed envelope. hpc-audit's `AuditEvent` is the inner payload. Action constants imported here, used by all crates that emit audit events. Always-on dependency (standardized audit format is valuable standalone).

**Nature:** Type imports + re-exports. `AuditEvent` wrapped in `AuditEntry { event: AuditEvent, signature: Vec<u8>, prev_hash: [u8; 32] }`.

---

## Cycle Analysis

**No cycles exist.** The dependency graph is a DAG:

```
Level 0: lattice-common (no internal deps)
Level 1: lattice-quorum, lattice-scheduler, lattice-checkpoint, lattice-node-agent
Level 2: lattice-api (depends on Level 0 + Level 1)
Level 3: lattice-cli (depends on Level 0 only, communicates via gRPC)
```

**Key architectural constraint:** lattice-scheduler does NOT depend on lattice-quorum. The scheduler receives state as arguments and returns decisions as values. This keeps the scheduling algorithm testable without Raft infrastructure.

## Runtime Dependencies

These are not compile-time crate dependencies but runtime communication paths:

```
lattice-cli ─── gRPC ──► lattice-api
lattice-node-agent ─── gRPC ──► lattice-api (RegisterNode, Heartbeat)
lattice-api ─── gRPC ──► lattice-node-agent (RunAllocation, StopAllocation, Attach, LaunchProcesses)
lattice-node-agent ─── gRPC ──► lattice-node-agent (PmiFence, cross-node MPI)
lattice-quorum ─── gRPC ──► lattice-quorum (Raft AppendEntries, Vote, InstallSnapshot)
```

## External Runtime Dependencies

| Module | External System | Protocol | Spec Source |
|---|---|---|---|
| lattice-common (client) | OpenCHAMI | REST | FM-12, INV-S6 (wipe) |
| lattice-common (client) | VAST | REST | IP-05, FM-11 (storage) |
| lattice-common (client) | Waldur | REST | IP-10, FM-13 (accounting) |
| lattice-common (client) | Sovra | REST | IP-09, Federation context |
| lattice-api (middleware) | OIDC Provider | REST/JWKS | Tenant & Access context |
| lattice-node-agent | VictoriaMetrics | Prometheus push | IP-05, FM-14 (telemetry) |
| lattice-node-agent | S3 | REST | Observability (log persistence) |
| lattice-node-agent | Slingshot NIC | libfabric/CXI | Network Domain VNI config |
| lattice-node-agent | PACT agent | Unix socket (SCM_RIGHTS) | IP-11, namespace handoff (optional) |
| lattice-node-agent | SPIRE agent | Unix socket | IP-13, workload identity (optional) |
| lattice-cli | OIDC Provider | REST/OAuth2 | IP-14, user authentication |
| lattice-common (SecretResolver) | HashiCorp Vault | REST (KV v2 + AppRole) | IP-15, INV-SEC1-6 (optional, startup-only) |

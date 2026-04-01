# Ubiquitous Language

Precise definitions for every domain term used in Lattice. When two terms seem synonymous, this document explains why both exist. Code, APIs, docs, and conversations must use these terms consistently.

## Core Concepts

**Allocation** — The universal work unit. A request for compute resources with a defined environment, lifecycle, and resource requirements. Replaces both "job" (Slurm) and "pod" (Kubernetes). The term "job" is avoided in Lattice's domain language because it implies batch-only semantics; Allocation spans batch, service, and reactive workloads through lifecycle variants.

**Lifecycle** — The duration semantics of an Allocation. Three variants:
- **Bounded**: Has a walltime. Runs until completion or walltime expiry. Equivalent to a Slurm batch job.
- **Unbounded**: Runs until explicitly cancelled. Auto-restarts on failure per restart policy. Equivalent to a Kubernetes Deployment.
- **Reactive**: Unbounded with autoscaling. Node count varies between min and max based on a metric threshold.

**vCluster** — A scheduling policy container that projects a filtered view of shared resources. Each vCluster has its own scheduler algorithm and cost function weights but shares the global quorum state with all other vClusters. Inspired by CSCS vClusters (Martinasso et al., CiSE 2024) but evolved: Lattice vClusters are dynamic, mutable, and have soft boundaries with elastic borrowing. vClusters are NOT security or isolation boundaries.

**Tenant** — An organizational boundary representing a group of users with shared quotas and access policies. Maps 1:1 to a Waldur Customer. Tenants are the isolation boundary for security and quota enforcement. A Tenant can have allocations in multiple vClusters.

**Project** — A subdivision within a Tenant for organizational purposes (maps to Waldur Project). Does not carry its own quotas — quotas are per-Tenant.

**User** — An authenticated identity, identified by OIDC subject claim. For sensitive workloads, the User (not Tenant) is the entity that claims nodes and is recorded in the audit log.

## Scheduling

**Cost Function** — A composite weighted scoring function that evaluates how valuable it is to schedule a given Allocation. Score(j) = Σ wᵢ · fᵢ(j). Nine factors, weights tunable per vCluster.

**Knapsack Solver** — The algorithm that selects which pending Allocations to schedule given available resources. Multi-dimensional (nodes, GPU-hours, topology span, I/O bandwidth, power). Greedy heuristic with topology-aware backfill.

**Scheduling Cycle** — A periodic evaluation (5-30s, configurable per vCluster) where the scheduler scores pending Allocations, solves the knapsack, and proposes node ownership changes to the quorum.

**Backfill** — Scheduling a lower-scored Allocation into resource gaps that cannot be used by higher-scored Allocations due to constraints (topology, conformance, etc.).

**Preemption** — Evicting a running Allocation to free resources for a higher-priority one. Only moves down preemption classes (0-10). Checkpoint-aware: the system attempts to checkpoint before killing. Sensitive Allocations (class 10) are never preempted.

**Preemption Class** — An integer 0-10 representing an Allocation's resistance to preemption. Higher = harder to preempt. Set by the Tenant's contract, not by the user. Sensitive claims are always class 10.

**Fair Share** — The mechanism ensuring each Tenant gets approximately their contracted proportion of resources over time. Computed as the deficit between target share and actual usage. Feeds into cost function factor f3.

**Elastic Borrowing** — When a vCluster has idle nodes beyond its base allocation, other vClusters may borrow them. Borrowed nodes carry preemption risk — the home vCluster can reclaim them via checkpoint + preempt.

**Base Allocation** — The guaranteed node count for a vCluster. Nodes in the base allocation are not borrowable. Nodes beyond the base allocation that are idle become borrowable.

## Service Lifecycle

**Service** — An Allocation with Unbounded or Reactive lifecycle that exposes named endpoints. Not a separate type — it is an Allocation with `connectivity.expose` endpoints and optionally a `liveness_probe`. The term "service" is used informally when the allocation runs indefinitely and serves requests.

**Liveness Probe** — A periodic health check on a service allocation. Two types:
- **TCP probe**: Connects to a port; success = connection established.
- **HTTP probe**: Sends GET to port+path; success = 2xx response.
Configured with: `period_secs`, `initial_delay_secs`, `failure_threshold`, `timeout_secs`. Managed by the node agent's ProbeManager.

**Service Registry** — A map in GlobalState from service name to registered endpoints. Auto-populated when allocations with `expose` endpoints transition to Running. Deregistered on terminal state or requeue. Tenant-filtered on query.

**Service Discovery** — The act of querying the ServiceRegistry for endpoints of a named service. Available via `LookupService`/`ListServices` gRPC RPCs and REST `/api/v1/services`.

**Reconciliation** — The scheduler's periodic check for failed Unbounded/Reactive allocations eligible for requeue. Failed → Pending transition with `requeue_count` increment. Respects `RequeuePolicy` (Never, OnNodeFailure, Always) and `max_requeue` cap (≤100).

**Scale Executor** — Component that translates autoscaler decisions (ScaleUp/ScaleDown) into infrastructure actions: boot nodes via OpenCHAMI (scale-up) or drain idle nodes (scale-down).

**Session** — An interactive terminal session attached to a running allocation. Tracked globally in Raft state for concurrency control. Sensitive allocations are limited to one concurrent session (INV-C2).

## Allocation Composition

**DAG** — A directed acyclic graph of Allocations with typed dependency edges. Submitted as a single unit. The DAG structure controls when Allocations enter the scheduler queue, not how they are scored.

**TaskGroup** — An array of identical Allocations instantiated over an index range (equivalent to Slurm job arrays). Each element is an independent Allocation. Parameterized by `${INDEX}`.

**Dependency** — A typed edge between two Allocations in a DAG. Conditions:
- **Success** (afterok): successor runs only if predecessor exits 0
- **Failure** (afternotok): successor runs only if predecessor exits non-zero
- **Any** (afterany): successor runs regardless of predecessor exit status
- **Corresponding** (aftercorr): element-wise dependency between TaskGroups
- **Mutex** (singleton): only one Allocation with this mutex name runs at a time

## Node & Infrastructure

**Node** — A physical compute node. Identified by an **xname** (e.g., `x1000c0s0b0n0`) following the Cray/HPE naming convention. A Node is owned by exactly one (Tenant, vCluster, Allocation) tuple, or is unowned.

**xname** — The hierarchical hardware identifier: `x{cabinet}c{chassis}s{slot}b{board}n{node}`. Used as NodeId throughout the system.

**Node State** — The lifecycle state of a Node: Unknown, Booting, Ready, Degraded, Down, Draining, Drained, Failed. See node-lifecycle in architecture docs for the full state machine.

**Heartbeat** — Periodic message (default: 10s) from a Node Agent to the quorum reporting health, conformance fingerprint, and running allocation count. Lightweight (~200 bytes), sent over management traffic class.

**Grace Period** — The window between a missed heartbeat (Degraded) and declaring the node Down. Standard: 60s. Sensitive: 5 minutes. Borrowed: 30s.

**Conformance Fingerprint** — SHA-256 hash of GPU driver version, NIC firmware version, BIOS version, kernel version. Nodes with identical fingerprints form a **Conformance Group**. The scheduler prefers placing multi-node Allocations within a single Conformance Group.

**Conformance Group** — A set of Nodes with identical Conformance Fingerprints. Used by cost function factor f9 to avoid cross-configuration placement that causes subtle failures (NCCL hangs, libfabric ABI mismatches).

**Dragonfly Group** — A topological unit in the Slingshot interconnect. Intra-group communication is electrical (low latency, high bandwidth). Inter-group is optical (higher latency, potential congestion). The scheduler packs Allocations into the fewest groups possible.

## Software Delivery

**ImageRef** — Content-addressed reference to a software image (uenv SquashFS or OCI container). Resolved at submit time (sha256 pinned), with optional deferred resolution for CI pipelines. Carries `original_tag` for auditability. The scheduler treats all ImageRefs uniformly for data readiness scoring.

**uenv** — User Environment. A SquashFS image mounted via Linux mount namespace. Near-zero runtime overhead, native GPU/Slingshot access. Default software delivery mechanism. Images stored in ORAS/S3 registry, cached on node-local NVMe.

**View** (uenv) — A named set of environment variable patches (EnvPatch) stored in `env.json` inside a uenv SquashFS image. Views are validated at resolution time and applied during the prologue. Examples: `default`, `spack`. Multi-uenv compositions use namespaced views: `image-name:view-name`.

**EnvPatch** — A single environment variable modification: prepend, append, set, or unset. Used by both uenv views and container EDF env sections. Applied in declaration order; last-write-wins for conflicts.

**Container** (sarus-suite) — OCI container executed via Podman with the `setns()` namespace-joining pattern. The workload process enters the container's user+mount namespace but remains a child of the lattice-node-agent. Provides full filesystem isolation while preserving native MPI/RDMA/Slingshot access.

**EDF** — Environment Definition File (TOML). Declarative container specification from the sarus-suite project. Supports `base_environment` inheritance chains (max 10 levels), mounts, CDI device specs, env vars, and annotations. Resolved and rendered at submit time by the API server.

**Parallax** — Shared read-only OCI image store using SquashFS layers on VAST/NFS. Eliminates per-node image pulls. Accessed via `squashfuse_ll` (FUSE). Optional — without it, nodes pull directly from the registry.

**DMTCP** — Distributed MultiThreaded Checkpointing. Transparent process-level checkpointing for applications that don't implement their own checkpoint support. Higher overhead but works for unmodified applications.

## Network

**Network Domain** — A named group of Allocations that share L3 network reachability via a Slingshot VNI. Scoped to a Tenant. Cross-vCluster sharing is intentional and supported. Sensitive Allocations get a unique per-allocation domain.

**VNI** — Virtual Network Identifier. Hardware-enforced network isolation on Slingshot/Ultra Ethernet NICs. Each Network Domain maps to one VNI. Pool: 1000-4095 (expandable).

**Traffic Class** — Slingshot hardware-enforced bandwidth partitioning: management, compute, storage, telemetry. Prevents one class from starving another.

## Data

**Data Staging** — Background pre-staging of input data from warm/cold tiers to hot tier (VAST) during queue wait. Invisible to the user. Cost function factor f5 scores data readiness.

**Hot Tier** — VAST storage (NFS + S3). Active data. Scheduler can set QoS, pre-stage, snapshot.

**Warm/Cold Tier** — S3-compatible capacity/archive storage. Cost-optimized. Regulatory retention on cold tier.

## Sensitive Workloads

**Sensitive** — The classification for workloads requiring regulatory-grade isolation and audit. Replaces the earlier term "medical" (generalized to cover financial, defense, etc.). Characteristics: user (not scheduler) claims nodes, dedicated hardware, encrypted storage, full audit trail, wipe on release, signed images only.

**Claiming User** — The specific authenticated User who claims sensitive nodes. Their OIDC subject is recorded in the Raft-committed audit log. Individual accountability, not organizational.

**Wipe on Release** — When a sensitive Allocation ends, the node undergoes secure erasure (GPU memory clear, NVMe secure erase, RAM scrub, reboot into clean image) via OpenCHAMI before returning to the general pool. Wipe failure quarantines the node.

**Quarantine** — A node state (treated as Down) for sensitive nodes where secure wipe failed. Excluded from scheduling until operator intervention.

## Checkpointing

**Checkpoint Strategy** — How an Allocation communicates with the checkpoint system: Signal (SIGUSR1), SharedMemory (futex/flag), gRPC (callback with negotiation), DMTCP (transparent), or None (non-checkpointable).

**Checkpoint Hint** — An advisory signal from the checkpoint broker to a node agent requesting that the application checkpoint. Not a command — the application may take time or fail.

**Checkpoint Broker** — Cross-context coordination mechanism that evaluates `Value > Cost` for each running Allocation and decides when checkpointing is worthwhile. Stateless; crash recovery is re-evaluation.

## Consensus

**Quorum** — The Raft consensus cluster (3-5 replicas). Owns the GlobalState. Only two categories of state require Raft consensus: node ownership and sensitive audit events.

**Proposal** — A state change submitted by a vCluster scheduler to the quorum for validation and commit. If the proposal conflicts with current state (e.g., node already owned), it is rejected and the scheduler retries next cycle.

**Strong Consistency** — State that is Raft-committed and cannot be violated even momentarily. In Lattice: node ownership and sensitive audit log.

**Eventual Consistency** — State that may be briefly stale (bounded by scheduling cycle time, ~5-30s). In Lattice: job queues, telemetry, quota accounting, session state, node capacity, DAG state, vCluster config.

## hpc-core Shared Contracts

**hpc-core** — A separate workspace of crates (published to crates.io) defining shared trait-based contracts for HPC infrastructure. Both PACT and Lattice depend on hpc-core; neither depends on the other. Prevents convention drift without code coupling.

**Dual-Mode Operation** — Lattice-node-agent's ability to operate in two modes: **standalone** (self-services all resource isolation) or **PACT-managed** (delegates to PACT via hpc-node contracts). Mode is detected at runtime by probing for PACT's handoff socket and readiness file. Also referred to informally as the "supercharged model" — PACT presence enhances Lattice capabilities without changing its correctness guarantees.

**CgroupManager** — hpc-node trait for cgroup v2 hierarchy management. Methods: `create_hierarchy()`, `create_scope()`, `destroy_scope()`, `read_metrics()`, `is_scope_empty()`. Lattice implements this in standalone mode; PACT implements it in managed mode. Both use the well-known `workload.slice/` path.

**NamespaceConsumer** — hpc-node trait implemented by lattice-node-agent. Requests pid/net/mount namespaces from PACT via unix socket (`/run/pact/handoff.sock`). Falls back to self-service (`unshare(2)`) when PACT is absent. Contrast with **NamespaceProvider** (implemented by PACT, not Lattice).

**MountManager** — hpc-node trait for refcounted SquashFS mount management. `acquire_mount()` increments refcount (or mounts on first use), `release_mount()` decrements (lazy unmount when zero). `reconstruct_state()` recovers from agent restart by scanning `/proc/mounts`.

**ReadinessGate** — hpc-node trait signaling that PACT has completed node initialization (all infrastructure services started, cgroup hierarchy created). Lattice waits for this signal with a timeout; standalone mode skips it.

**AuditEvent** — hpc-audit struct defining the universal audit event format: who (`AuditPrincipal`), what (`action` string from well-known constants), when (`timestamp`), where (`AuditScope`: node/vCluster/allocation), outcome (Success/Failure/Denied), detail, metadata, and source (`AuditSource` enum). Lattice wraps this in a signed envelope for Raft storage.

**AuditSink** — hpc-audit trait for non-blocking audit event emission. `emit()` buffers internally; `flush()` drains on shutdown. Lattice's implementation proposes events to the Raft quorum.

**CompliancePolicy** — hpc-audit struct defining audit retention rules (retention days, required audit points, full-access logging flag). `CompliancePolicy::regulated()` preset: 2555 days (7 years), full access logging, ~35 required actions.

**IdentityCascade** — hpc-identity struct that tries multiple `IdentityProvider` implementations in priority order to obtain a `WorkloadIdentity`. Standard order: SPIRE → self-signed CA → bootstrap cert. Used by both lattice-node-agent and lattice-quorum for mTLS.

**WorkloadIdentity** — hpc-identity struct containing cert chain PEM, private key PEM (never logged/transmitted), trust bundle PEM, expiration, and source provenance. Renewal triggered at 2/3 of certificate lifetime.

**CertRotator** — hpc-identity trait for non-disruptive certificate rotation using dual-channel swap: build new TLS channel → health-check → atomically swap → drain old channel.

**AuthClient** — hpc-auth struct providing OAuth2 token management. Cascading flow selection (Auth Code + PKCE → Device Code → Manual Paste → Client Credentials), per-server token caching (`~/.config/{app}/tokens.json`), automatic refresh, OIDC discovery. Used by lattice-cli.

## Secret Management

**SecretResolver** — Cross-cutting infrastructure that resolves operational secrets at component startup. Owned by `lattice-common`, consumed by `lattice-api`, `lattice-node-agent`, and `lattice-quorum`. Resolution is startup-only (restart to rotate). Three backends in precedence order when Vault is not configured: environment variable → config literal. When Vault is configured, it overrides all other sources globally.

**Vault Global Override** — When `vault.address` is set in configuration, all secret fields are resolved exclusively from HashiCorp Vault KV v2. Config file literals and environment variables for secret fields are ignored. Missing keys in Vault are fatal startup errors. This is an all-or-nothing mode: either all secrets come from Vault, or none do.

**Convention-Based Vault Path** — The deterministic mapping from a config field name to a Vault KV v2 path. The config field `{section}.{field}` maps to `GET {vault_prefix}/{section}` with response key `{field}`. The default Vault prefix is `secret/data/lattice`. No per-secret path configuration exists — operators organize their Vault namespace to match Lattice's config structure.

**Secret Field** — A configuration field whose value is a credential, token, or key material. Secret fields are: `storage.vast_username`, `storage.vast_password`, `accounting.waldur_token`, `quorum.audit_signing_key`, `sovra.key_path`. TLS certificates and private keys are NOT secret fields — they are managed by the hpc-identity cascade.

**AppRole** — The Vault authentication method used by Lattice server components. A role ID and secret ID are provided via config or environment variables to obtain a Vault token at startup. Not used by lattice-cli (which uses hpc-auth for OIDC).

## External Systems

**OpenCHAMI** — Infrastructure management system. Handles node boot/reimage (BSS), hardware discovery (Magellan/Redfish), state management (SMD), identity (OPAAL), and configuration injection (cloud-init). Lattice integrates but does not own.

**Sovra** — Federated sovereign key management. Each site has its own root key. Shared workspaces enable cross-site trust. Feature-gated.

**FirecREST** — Optional compatibility gateway for hybrid Slurm deployments. When present, acts as passthrough — lattice authenticates directly against the IdP via hpc-auth. Not required.

**Waldur** — External accounting and billing system. Lattice pushes usage events; Waldur pushes quota updates. Feature-gated. Failure never blocks scheduling.

**Budget Ledger** — Internal GPU-hours usage tracking computed from allocation history in the quorum. Provides budget utilization data to the cost function without requiring Waldur. When Waldur is available, Waldur's `remaining_budget` takes precedence; when unavailable, the internal ledger is the fallback. The ledger is not a separate store — it is a computation over existing allocation records (started_at, completed_at, assigned_nodes, gpu_count).

**Budget Period** — The time window over which GPU-hours consumption is measured. Configurable per system (default: 90 days). Allocations completed before the period start are excluded from usage computation. The period is a rolling window, not a calendar-aligned reset.

**Budget Utilization** — The fraction of budget consumed within the current budget period. Computed from both `gpu_hours_used / gpu_hours_budget` and `node_hours_used / node_hours_budget`. When both budgets are set, the worse (higher) fraction drives the penalty. Values above 1.0 indicate over-budget. Fed into the cost function as a penalty multiplier (0-80%: no penalty, 80-120%: increasing penalty, >120%: floor at 0.05).

**Node-Hours** — The universal resource metric for budget tracking. Computed as `assigned_nodes.len() × elapsed_hours`. Works for all node types (GPU and CPU-only). One node occupied for one hour = one node-hour. Since Lattice does full-node scheduling (ADR-007), there is no fractional node usage.

**VAST** — Storage system providing hot tier (NFS + S3). Lattice integrates via REST API for QoS, pre-staging, snapshots, and secure encrypted pools.

## API Tiers

**Intent API** — The primary API. Declarative: agents/users declare what they need, the scheduler resolves how. All scheduling decisions flow through this path.

**Compatibility API** — Stateless translation layer mapping Slurm commands (sbatch, squeue, scancel) to Intent API calls. Graceful degradation for unsupported directives (warns, never errors).

## Terms Deliberately Avoided

| Avoided Term | Reason | Use Instead |
|---|---|---|
| Job | Implies batch-only semantics | Allocation |
| Pod | Implies Kubernetes model | Allocation |
| Partition | Implies hard resource boundary (Slurm) | vCluster |
| Container | Ambiguous (Docker? OCI? Sarus?) | uenv (default) or Sarus (when OCI needed) |
| Cluster | Ambiguous (the whole system? a vCluster?) | "system" for the whole thing, "vCluster" for a policy boundary |
| Medical | Too narrow for the general sensitive model | Sensitive |

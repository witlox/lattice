# System Invariants

Invariants that must ALWAYS hold, regardless of system state, load, or failure conditions. Organized by consistency domain and bounded context. Each invariant specifies its enforcement mechanism.

## Strong Consistency Invariants (Raft-Enforced)

These invariants are guaranteed by Raft consensus. They cannot be violated even momentarily, even under concurrent proposals from multiple vCluster schedulers.

### INV-S1: Exclusive Node Ownership

**Statement:** A node is owned by at most one (Tenant, vCluster, Allocation) tuple at any point in time.

**Enforcement:** Raft proposal validation. The quorum rejects any proposal that would assign a node already owned by another allocation. Serialized by Raft log ordering.

**Violation consequence:** Double-assignment of physical resources. Two allocations believe they own the same node. Data corruption, performance interference, security breach (for sensitive).

**Cross-context:** Scheduling proposes, Consensus validates. Node Management reads committed state.

---

### INV-S2: Hard Quota Non-Violation

**Statement:** A Tenant's concurrent node count never exceeds `max_nodes`. A Tenant's concurrent allocation count never exceeds `max_concurrent_allocations`. The system-wide sensitive pool never exceeds `sensitive_pool_size`.

**Enforcement:** Raft proposal validation. The quorum sums current ownership and rejects proposals that would exceed limits.

**Note:** Reducing a hard quota below current usage does NOT preempt running allocations — it only blocks new proposals. Current usage may temporarily exceed the new limit until allocations complete naturally.

**Violation consequence:** Resource over-commitment. Potentially unbounded resource consumption by a single Tenant.

---

### INV-S3: Sensitive Audit Completeness

**Statement:** Every action on a sensitive allocation (claim, release, data access, attach, log access, metrics query, checkpoint) produces a Raft-committed audit entry with the authenticated User identity and timestamp before the action takes effect.

**Enforcement:** Sensitive operations are gated on Raft commit of the audit entry. The action is not performed until the audit entry is committed.

**Violation consequence:** Regulatory non-compliance. Cannot prove who accessed what data.

---

### INV-S4: Sensitive Audit Immutability

**Statement:** The sensitive audit log is append-only. No entry can be modified or deleted. Entries are cryptographically signed and chained.

**Enforcement:** Raft log structure (entries reference predecessors). Signing with site PKI or Sovra keys. Storage on immutable S3.

**Violation consequence:** Tamper evidence lost. Audit trail inadmissible for regulatory purposes.

---

### INV-S5: Sensitive Node Isolation

**Statement:** A node claimed for sensitive use runs exactly one Tenant's allocations. No co-scheduling, no borrowing, no elastic sharing.

**Enforcement:** Raft proposal validation rejects any proposal that would assign a sensitive-claimed node to a different allocation. The sensitive scheduler does not participate in elastic borrowing.

**Violation consequence:** Data exposure across Tenants on shared hardware. Regulatory violation.

---

### INV-S6: Sensitive Wipe Before Reuse

**Statement:** A node released from sensitive use must complete secure wipe (GPU memory clear, NVMe erase, RAM scrub, reboot) before returning to the general scheduling pool. Wipe confirmation is Raft-committed.

**Enforcement:** Node Management executes wipe via OpenCHAMI. Wipe completion event is Raft-committed. Node remains in quarantine (treated as Down) until wipe confirmation. Wipe failure keeps the node quarantined indefinitely.

**Violation consequence:** Data remnants accessible to the next Tenant.

## Eventual Consistency Invariants (Scheduler-Enforced)

These invariants may be briefly violated during consistency windows (bounded by scheduling cycle, ~5-30s) but are self-correcting.

### INV-E1: Preemption Class Ordering

**Statement:** Preemption only moves down: a class-N allocation can only be preempted by a class-(N+1) or higher allocation. Sensitive allocations (class 10) are never preempted.

**Enforcement:** Scheduler preemption decision algorithm filters candidates by class. Validated at API admission (class 0-10 range, class tied to Tenant contract).

**Violation consequence:** Priority inversion. High-value workloads evicted by low-priority ones.

---

### INV-E2: DAG Acyclicity

**Statement:** A DAG submission must be acyclic. Cycle detection runs at submission time.

**Enforcement:** Kahn's algorithm (topological sort) during API validation. Submissions with cycles are rejected with a descriptive error.

**Violation consequence:** Deadlock — allocations waiting on each other indefinitely.

---

### INV-E3: Dependency Condition Satisfaction

**Statement:** A DAG allocation enters the scheduler queue only when ALL its incoming dependency conditions are satisfied.

**Enforcement:** DAG controller evaluates edges on each allocation state change. Eventually consistent — evaluation may lag allocation state by one scheduling cycle.

**Violation consequence:** Allocation runs before its prerequisites are met. Incorrect results, missing input data.

---

### INV-E4: Walltime Supremacy

**Statement:** Walltime expiry takes priority over all other operations, including in-progress checkpoints. When walltime expires: SIGTERM → grace period → SIGKILL.

**Enforcement:** Node agent timer. Independent of checkpoint broker.

**Violation consequence:** Allocations run indefinitely, consuming resources beyond their contract.

---

### INV-E5: Network Domain Tenant Scoping

**Statement:** Only allocations from the same Tenant can share a Network Domain. Cross-tenant domains are never created.

**Enforcement:** API validation on allocation submission. Domain name is scoped to Tenant ID internally.

**Violation consequence:** Cross-tenant network reachability. Data leakage via network.

---

### INV-E6: Soft Quota Self-Correction

**Statement:** Soft quotas (gpu_hours_budget, fair_share_target) may temporarily overshoot but converge within a bounded window (~30s). Over-budget Tenants receive progressively lower scheduling scores.

**Enforcement:** Scheduler cost function factors f3 (fair share) and budget penalty. Budget utilization computed from internal ledger (allocation history) or Waldur (when available). Eventual consistency window bounded by scheduling cycle time.

**Violation consequence (if self-correction fails):** Unbounded resource consumption by a single Tenant, starvation of others.

---

### INV-E7: Budget Ledger Consistency

**Statement:** The internal budget ledger computes GPU-hours and node-hours from allocation records (started_at, completed_at, assigned_nodes, gpu_count_per_node). The computation is deterministic: given the same allocation set and timestamp, the result is identical across scheduler replicas. When both budgets are set, the worse utilization fraction drives the penalty.

**Enforcement:** Computation uses only Raft-committed allocation state and wall-clock time. No external dependencies. Waldur override, when available, takes precedence but internal ledger is always available as fallback.

**Violation consequence:** Budget penalty applied inconsistently across scheduling cycles. Tenants may oscillate between penalized and unpenalized states.

## Ordering Invariants

### INV-O1: Proposal Before Execution

**Statement:** Node ownership must be Raft-committed before any allocation workload starts on the node. The node agent does not begin prologue until it receives committed assignment.

**Enforcement:** Quorum notifies node agents only after Raft commit. Node agent waits for assignment notification.

**Violation consequence:** Workload starts on a node that may be reassigned. Race condition with another allocation.

---

### INV-O2: Prologue Before Entrypoint

**Statement:** The allocation prologue (uenv pull/mount, data staging, scratch setup) must complete before the user's entrypoint executes.

**Enforcement:** Node agent prologue/entrypoint sequencing. Prologue failure → allocation retried or failed.

**Violation consequence:** Entrypoint runs without its software environment or data. Immediate crash or silent incorrect behavior.

---

### INV-O3: Audit Before Sensitive Action

**Statement:** For sensitive allocations, the audit entry is Raft-committed BEFORE the action is performed (claim, attach, data access).

**Enforcement:** Sensitive operation handlers commit audit entry first, then proceed. See INV-S3.

**Violation consequence:** Action performed without audit trail. Regulatory gap.

## Cardinality Invariants

### INV-C1: Node Ownership Cardinality

**Statement:** A node has at most one owner (Allocation). 0:1 relationship.

**Enforcement:** See INV-S1.

---

### INV-C2: Sensitive Attach Cardinality

**Statement:** A sensitive allocation has at most one active attach session at any time.

**Enforcement:** Node agent rejects concurrent attach requests for sensitive allocations.

**Violation consequence:** Shared terminal could leak sensitive data to unauthorized observer.

---

### INV-C3: VNI Uniqueness

**Statement:** Each active Network Domain maps to exactly one VNI. No two active domains share a VNI.

**Enforcement:** VNI pool allocator (sequential allocation, released on domain teardown).

**Violation consequence:** Two domains sharing a VNI have unintended network reachability.

---

### INV-C4: Allocation-vCluster Binding

**Statement:** An Allocation belongs to exactly one vCluster for its entire lifetime. It cannot migrate between vClusters.

**Enforcement:** Set at submission time, immutable thereafter.

**Violation consequence:** Scheduling inconsistency — two vCluster schedulers managing the same allocation.

---

### INV-C5: DAG Size Limit

**Statement:** A DAG contains at most `max_dag_size` (default: 1000) allocations.

**Enforcement:** API validation at submission time.

**Violation consequence:** Unbounded DAG resolution overhead in the DAG controller.

## Negative Invariants (Must NEVER Happen)

### INV-N1: No Kubernetes Dependencies

**Statement:** The system must never depend on Kubernetes APIs, CRDs, or controllers.

**Enforcement:** Code review. Cargo dependency audit (deny.toml).

---

### INV-N2: No SSH Between Compute Nodes

**Statement:** Compute nodes must never use SSH for inter-node communication. All inter-node coordination uses gRPC over the management network.

**Enforcement:** Node agent design (PMI-2 over gRPC). Sensitive hardened images have no SSH daemon.

---

### INV-N3: No Sensitive Data Federation

**Statement:** Sensitive data must not leave its designated jurisdiction. Compute may theoretically federate (with consent) but data never transits.

**Enforcement:** Federation broker policy check. Data staging refuses cross-site transfers for sensitive allocations.

---

### INV-N4: No Silent Failure

**Statement:** No component may silently swallow errors that affect allocation correctness. Errors must be surfaced (to the user via API, to operators via metrics/alerts, or to the audit log for sensitive).

**Enforcement:** Error handling conventions. Typed errors per module. Adversarial review.

---

### INV-N5: Accounting Never Blocks Scheduling

**Statement:** Waldur unavailability must never prevent or delay allocation scheduling.

**Enforcement:** Async push with bounded buffer. Events dropped (with counter metric) rather than blocking.

## Resource Isolation Invariants (hpc-node)

### INV-RI1: Cgroup Slice Ownership

**Statement:** Lattice owns the `workload.slice/` cgroup subtree exclusively. When PACT is present, PACT owns `pact.slice/`. Neither system writes to the other's subtree.

**Enforcement:** `CgroupManager` implementation creates scopes only under `workload.slice/`. Standalone mode: lattice-node-agent creates the `workload.slice/` hierarchy at startup. PACT-managed mode: PACT creates both slices at boot; lattice-node-agent verifies `workload.slice/` exists before creating scopes.

**Violation consequence:** Cross-system resource limit interference. Processes placed in wrong cgroup could escape resource accounting.

---

### INV-RI2: Namespace Handoff Fallback

**Statement:** If the PACT handoff socket (`/run/pact/handoff.sock`) is unavailable, lattice-node-agent creates namespaces via `unshare(2)` directly (self-service mode). Namespace handoff failure must never block allocation startup.

**Enforcement:** `NamespaceConsumer` implementation: attempt socket connection → on failure, log warning + emit audit event (`namespace.handoff_failed`) → create namespaces locally. Timeout: 1s for socket connection.

**Violation consequence:** Allocation startup blocked indefinitely. Violates principle that PACT absence must not degrade Lattice functionality.

---

### INV-RI3: Mount Refcount Consistency

**Statement:** When using `MountManager`, every `acquire_mount()` must have a corresponding `release_mount()`. Refcount must never go negative. On agent restart, mount state is reconstructed from `/proc/mounts` and active allocations.

**Enforcement:** `MountManager` implementation tracks refcounts in-memory. `reconstruct_state()` called on agent startup. Standalone mode: simpler mount/unmount without refcounting (one mount per allocation).

**Violation consequence:** Orphaned mounts consume memory and block image updates. Negative refcount causes premature unmount, crashing running allocations.

---

### INV-RI4: Readiness Gate Non-Blocking

**Statement:** Lattice-node-agent waits for PACT readiness signal for at most `readiness_timeout` (default: 30s). If timeout expires or readiness gate is absent (standalone mode), the agent proceeds as ready. Readiness waiting must never prevent the agent from starting.

**Enforcement:** `ReadinessGate` consumer: check `/run/pact/ready` file → if exists, ready. If not, poll with timeout → on timeout, log warning and proceed.

**Violation consequence:** Node never becomes schedulable. Stuck in Booting state.

## Audit Format Invariants (hpc-audit)

### INV-AF1: Standardized Audit Event Schema

**Statement:** All audit events emitted by Lattice components use the `hpc_audit::AuditEvent` struct (id, timestamp, principal, action, scope, outcome, detail, metadata, source). The `source` field is set to the appropriate `AuditSource` variant (`LatticeNodeAgent`, `LatticeQuorum`, or `LatticeCli`).

**Enforcement:** `AuditEntry` wraps `hpc_audit::AuditEvent` directly (clean break — no backwards compatibility layer). The Raft state machine stores `AuditEntry { event: AuditEvent, previous_hash: String, signature: String }`. The old `AuditAction` enum and flat `user`/`action`/`details` fields are removed. The `event.source` field is set per component (`LatticeNodeAgent`, `LatticeQuorum`, `LatticeCli`). The `event.action` field uses hpc-audit constants or lattice-specific constants with `lattice.` prefix (see IP-12 action mapping). `event.principal.identity` replaces the old `user: UserId` field.

**Violation consequence:** SIEM forwarding requires a translation layer. Cross-system audit correlation breaks.

---

### INV-AF2: Well-Known Action Constants

**Statement:** Lattice uses `hpc_audit::actions::*` constants for action strings in audit events. Custom lattice-specific actions use the `lattice.` prefix (e.g., `lattice.scheduling.proposal_rejected`). No ad-hoc action strings.

**Enforcement:** Code review. Action constants imported from hpc-audit crate. Custom actions defined in lattice-common as constants.

**Violation consequence:** Audit queries miss events due to inconsistent action naming.

---

### INV-AF3: Audit Sink Non-Blocking

**Statement:** `AuditSink::emit()` must never block the caller. Implementations buffer internally. If the sink is unavailable, events are buffered locally. Dropping audit events violates audit trail continuity.

**Enforcement:** Lattice's `AuditSink` implementation (Raft-backed) buffers events in-memory and proposes them to the quorum asynchronously. Sensitive audit events are the exception: they block on Raft commit (INV-S3/INV-O3).

**Violation consequence:** Audit gap. For sensitive workloads, this is a regulatory violation.

## Identity Invariants (hpc-identity)

### INV-ID1: Identity Cascade Order

**Statement:** Lattice components use `IdentityCascade` with providers in priority order:
1. **SpireProvider** (primary) — SPIRE agent socket, X.509 SVID. Standard on HPE Cray.
2. **SelfSignedProvider** (fallback) — agent-generated keypair + CSR signed by lattice-quorum ephemeral CA. Same model as PACT ADR-008.
3. **StaticProvider** (bootstrap) — pre-provisioned cert from SquashFS image or local files. Used during boot window.

The cascade tries each provider's `is_available()` before calling `get_identity()`. SPIRE availability is detected via local socket probe (no network dependency).

**Enforcement:** `IdentityCascade::new()` called with providers in correct order at agent/quorum startup. Provider list is immutable after construction. Lattice owns its own trust domain — separate CA keys from PACT even when co-deployed.

**Violation consequence:** Using a weaker identity source when a stronger one is available. Not a security breach (all sources are valid), but suboptimal trust posture.

---

### INV-ID2: Private Key Locality

**Statement:** Private keys generated by `IdentityProvider` implementations must never be transmitted over the network or stored by the identity manager. Keys are generated locally and used only for local TLS termination.

**Enforcement:** `WorkloadIdentity.private_key_pem` is generated in-process. CSR signing sends only the public key to the signing endpoint. `Debug` trait redacts private key field.

**Violation consequence:** Private key compromise. mTLS security model broken.

---

### INV-ID3: Certificate Rotation Non-Disruptive

**Statement:** `CertRotator::rotate()` must not interrupt in-flight gRPC connections. The rotation protocol is: build passive channel with new cert → health-check → atomically swap active ↔ passive → drain old channel.

**Enforcement:** Dual-channel pattern in `CertRotator` implementation. Old channel continues serving until drained. Rotation failure leaves active channel unchanged.

**Violation consequence:** Dropped gRPC connections during cert rotation. Heartbeat gaps, scheduling pauses.

## Network Topology Invariants

### INV-NET1: HSN Binding for Lattice Services

**Statement:** Lattice quorum (Raft transport + gRPC API) and lattice-node-agent bind to the high-speed network interface, not the management network. In co-located mode with PACT, lattice and PACT use different network interfaces on the same physical servers (PACT: management 1G, lattice: HSN 200G+).

**Enforcement:** `QuorumConfig.bind_network` and `NodeAgentConfig.bind_network` specify `hsn` (default) or `management`. When set to `hsn`, the bind address resolves to the HSN interface. Standalone mode: defaults to `0.0.0.0` (all interfaces) for backwards compatibility.

**Violation consequence:** Lattice traffic on management network saturates 1G links. Raft consensus latency degrades. At scale (>1000 nodes), scheduling becomes unreliable.

---

### INV-NET2: No Port Conflicts in Co-located Mode

**Statement:** When lattice quorum and PACT journal share physical servers, they use different ports on different network interfaces. Lattice: gRPC 50051, Raft 9000 (HSN). PACT: gRPC 9443, Raft 9444 (management). No overlap.

**Enforcement:** Default port configuration. Deployment validation checks for port conflicts at startup.

**Violation consequence:** Service bind failure at startup. One system fails to start.

## Raft Co-location Invariants

### INV-R1: Independent Raft Group

**Statement:** Lattice quorum is always an independent Raft group, even when co-located with PACT journal on the same physical servers. Separate consensus, separate state machine, separate ports, separate WAL.

**Enforcement:** Distinct port configuration (Lattice: 9000/50051/8080, PACT: 9444/9443). Separate raft-hpc-core instances. No shared state.

**Violation consequence:** State corruption in one system propagates to the other.

---

### INV-R2: PACT Is Incumbent

**Statement:** In co-located mode, PACT journal quorum is running before Lattice starts. Lattice does not depend on PACT being present (standalone mode is valid), but when co-located, PACT is the pre-existing service.

**Enforcement:** Deployment ordering. Lattice startup does not wait for or check PACT state.

---

## Authentication Invariants (Lattice-specific)

### INV-A1: Unauthenticated Command Allowlist

**Statement:** Only `login`, `logout`, `version`, and `--help` may execute without a valid token. All other commands require authentication.

**Enforcement:** CLI middleware checks token before dispatching to command handler.

---

### INV-A2: Lenient Permission Mode

**Statement:** Lattice CLI uses lenient permission mode for the token cache (warn and fix, not reject). This differs from PACT's strict mode.

**Enforcement:** Configuration passed to hpc-auth crate at initialization.

---

### INV-A3: Waldur Authorization Is Runtime

**Statement:** vCluster access authorization is checked at request time via Waldur allocation state, not at login time. The login token identifies the user; Waldur determines what they can do.

**Enforcement:** lattice-api checks Waldur allocations on each authenticated request that targets a vCluster.

**Violation consequence:** Users could access vClusters they have no allocation for, or be denied access despite having a valid allocation.

---

### INV-A4: Auth Discovery Endpoint Is Public

**Statement:** The lattice-api auth discovery endpoint does not require authentication. It returns the IdP URL and public client ID needed to initiate login.

**Enforcement:** Endpoint excluded from auth middleware.

**Violation consequence:** Chicken-and-egg — users cannot login because login requires a token.

---

### INV-A5: API Authentication Enforcement

**Statement:** All REST and gRPC API endpoints except `/healthz` and `/api/v1/auth/discovery` require a valid Bearer token in the `Authorization` header. Tokens are validated via JWKS (production, RS256/ES256) or HMAC-SHA256 (dev/testing via `LATTICE_OIDC_HMAC_SECRET`). Requests without a valid token receive `401 Unauthenticated`. Role derivation checks both OIDC scopes and cross-system role claims (`pact_role`, `lattice_role`) for pact+lattice co-deployment.

**Enforcement:** REST auth middleware validates asynchronously; gRPC interceptor validates synchronously using cached JWKS keys (pre-fetched at startup) or HMAC. Cross-system `pact-platform-admin` role maps to `SystemAdmin`.

**Violation consequence:** Unauthenticated network access to scheduler state — any host on the network can query and mutate allocations, nodes, and tenants.

---

### INV-A6: Agent-to-Server Authentication

**Statement:** Node agents authenticate to lattice-server via mTLS (production) or Bearer token (dev/testing/break-glass). mTLS identity is acquired via the identity cascade (SPIRE → SelfSigned CA → Bootstrap certs). When no mTLS identity is available, the agent falls back to `LATTICE_AGENT_TOKEN` Bearer auth. Both paths coexist; mTLS takes priority.

**Enforcement:** Agent gRPC client configures `ClientTlsConfig` when identity is available. `AgentAuthInterceptor` injects Bearer token as fallback. Server validates via mTLS client cert verification or OIDC/HMAC token validation.

**Violation consequence:** Agents cannot register or heartbeat. Nodes remain in Unknown state and are never scheduled.

---

### INV-A7: FirecREST Is Not Part of the Architecture

**Statement:** FirecREST is not part of the Lattice architecture. Lattice authenticates directly against the institutional IdP via hpc-auth. If FirecREST is present as a legacy compatibility gateway for hybrid Slurm deployments, it is transparent — it does not participate in authentication, authorization, or any Lattice-specific logic.

**Enforcement:** No FirecREST-specific code anywhere in the codebase. The auth path uses hpc-auth for OIDC flows.

---

## Secret Resolution Invariants

### INV-SEC1: Secret Resolution Before Service Start

**Statement:** All secret references must be resolved before the component begins serving requests or accepting connections. A resolution failure for any required secret is a fatal startup error — the component refuses to start.

**Enforcement:** `SecretResolver::resolve_all()` called in `main()` before constructing any client or server. Returns `Result` — `Err` triggers `process::exit(1)` with descriptive error.

**Violation consequence:** Component starts with missing or stale credentials. Runtime failures (401/403) that are harder to diagnose than a clear startup error.

---

### INV-SEC2: Secrets Never Logged

**Statement:** Resolved secret values must never appear in log output, error messages, debug dumps, tracing spans, or Raft state. Secret references (Vault paths, config field names, env var names) may be logged; resolved values may not.

**Enforcement:** `SecretValue` wrapper type that implements `Debug` and `Display` as `"[REDACTED]"`. Secret fields stored as `SecretValue`, not `String`. Review via `cargo clippy` custom lint or adversary review.

**Violation consequence:** Credentials exposed in log aggregation systems (ELK, Loki, stdout captured by systemd journal). Credential rotation required.

---

### INV-SEC3: Vault Unavailable at Startup Is Fatal

**Statement:** If Vault is configured (`vault.address` is set) and Vault is unreachable or authentication fails at startup, the component fails to start. There is no retry loop, no cached-last-known-good fallback, and no degraded mode.

**Enforcement:** `VaultClient::authenticate()` called during `SecretResolver` construction. Connection failure or auth failure returns `Err`, propagated to `main()`.

**Violation consequence:** Component runs with no secrets resolved (all empty). Immediate runtime failures.

---

### INV-SEC4: Vault Global Override

**Statement:** When `vault.address` is configured, ALL secret fields are resolved from Vault KV v2. Config file literals and environment variables for those fields are ignored. A missing key in Vault is a fatal startup error (not a fallback to config).

**Enforcement:** `SecretResolver` checks `vault_config.is_some()` and routes all resolution through the Vault backend. No fallback chain when Vault is active.

**Violation consequence:** Mixed secret sources (some from Vault, some from config). Inconsistent security posture. Operator believes secrets are in Vault but component uses stale config values.

---

### INV-SEC5: Convention-Based Vault Paths

**Statement:** Secret field `{section}.{field}` maps deterministically to Vault path `{vault_prefix}/{section}` with response key `{field}`. No per-secret path configuration exists. The default prefix is `secret/data/lattice`.

**Enforcement:** Path construction in `SecretResolver::vault_path()` is a pure function of section name, field name, and prefix. No lookup tables or overrides.

**Violation consequence:** Operator cannot predict which Vault path a secret maps to. Operational confusion.

---

### INV-SEC6: Fully Functional Without Vault

**Statement:** Every secret field has a non-Vault resolution path (environment variable → config literal, or file path for binary secrets). Vault is never a runtime dependency. All cryptographic operations (audit signing, TLS) use locally-held key material.

**Enforcement:** `SecretResolver` always has env/config fallback logic. No code path requires Vault to be present. Integration tests run without Vault.

**Violation consequence:** Lattice cannot be deployed without Vault infrastructure. Violates design principle that external systems are optional.

---

## Service Lifecycle Invariants

### INV-SVC1: Reconciliation Only For Service Lifecycles

**Statement:** The reconciliation loop (Failed → Pending) only applies to Unbounded and Reactive allocations. Bounded allocations that fail remain permanently Failed.

**Enforcement:** `should_requeue()` in loop_runner.rs checks `LifecycleType::Unbounded | Reactive`.

---

### INV-SVC2: Max Requeue Cap

**Statement:** `max_requeue` is capped at 100. Values above 100 are rejected at submission time.

**Enforcement:** Validation in `allocation_from_proto()` (convert.rs).

---

### INV-SVC3: Service Registry Consistency

**Statement:** The service registry is consistent with allocation state: an endpoint exists in the registry if and only if its allocation is Running.

**Enforcement:** Registration/deregistration in `update_allocation_state()` handler (global_state.rs). Deregistration also in `requeue_allocation()`.

---

### INV-SVC4: Requeue Optimistic Concurrency

**Statement:** `RequeueAllocation` carries `expected_requeue_count`. If the count has changed since the reconciler read it, the requeue is rejected to prevent double-increment.

**Enforcement:** Check in `requeue_allocation()` (global_state.rs).

---

### INV-SVC5: Tenant-Scoped Service Discovery

**Statement:** Service discovery queries are filtered by the requesting tenant. A tenant cannot see endpoints belonging to other tenants.

**Enforcement:** `x-lattice-tenant` header extraction + filter in LookupService/ListServices handlers.

---

### INV-SVC6: Input Validation at API Boundary

**Statement:** The API rejects malformed input at the boundary:
- Empty tenant or entrypoint
- TaskGroup step=0 or range_start > range_end
- Empty DAG allocations list
- Self-referencing DAG dependencies
- Reactive min_nodes > max_nodes
- Service endpoint port outside 1-65535

**Enforcement:** Validation in `allocation_from_proto()` and submit handler (allocation_service.rs).

---

## Software Delivery Invariants

### INV-SD1: Content-Addressed Image Pinning

**Statement:** After resolution, an allocation's ImageRef.sha256 is immutable. Re-pushing a mutable tag to a different hash does not change the pinned image. The allocation runs exactly the image that was resolved at submit time (or scheduling time for deferred).

**Enforcement:** API server resolves ImageRef on submit, stores sha256 in allocation spec. Node agent pulls by sha256, not tag. No re-resolution after pinning.

**Violation consequence:** Different nodes in a multi-node job could run different image versions. Non-reproducible results.

---

### INV-SD2: View Validation at Resolution Time

**Statement:** View names are validated against the image's metadata at resolution time. Requesting a nonexistent view is a submit-time error (or scheduling-time error for deferred resolution).

**Enforcement:** API server extracts `env.json` (uenv) or EDF metadata (container) and validates view names before accepting the allocation.

**Violation consequence:** Allocation reaches prologue, view activation fails, allocation fails after consuming scheduler resources.

---

### INV-SD3: Mount Point Non-Overlap

**Statement:** When an allocation specifies multiple images, their mount points must not overlap (no prefix containment). Validated at submit time.

**Enforcement:** API server checks all `ImageRef.mount_point` values for prefix containment in `allocation_from_proto()`.

**Violation consequence:** Mount shadowing — one image's files hidden by another's mount. Unpredictable behavior.

---

### INV-SD4: Deferred Resolution Timeout

**Statement:** Allocations with `resolve_on_schedule: true` must resolve within a configurable timeout (default: 1 hour). After timeout, allocation transitions to Failed with reason "image resolution timeout".

**Enforcement:** Scheduler loop checks unresolved allocations each cycle. Timeout tracked from `submitted_at`.

**Violation consequence:** Unresolvable allocations occupy queue indefinitely, consuming quota and confusing users.

---

### INV-SD5: Sensitive Image Integrity

**Statement:** Sensitive allocations require: `sign_required: true` (image signature verified at pull), digest reference (`@sha256:...`) not mutable tags, `writable: false` for containers, specific GPU indices (not `gpu=all`) in device specs.

**Enforcement:** API server validates sensitive constraints at submit time. Node agent verifies signatures at pull time.

**Violation consequence:** Unsigned or tampered images on sensitive nodes. Data exfiltration via writable rootfs. Unauditable GPU access.

---

### INV-SD6: Single Pull for Shared Store

**Statement:** For multi-node allocations, the image is pulled once to the shared store (VAST/NFS for uenv, Parallax for OCI). Individual nodes read from the shared store. There is never a per-node pull to a registry for images available in the shared store.

**Enforcement:** Data stager creates one staging request per image (not per node). Node agent checks shared store before attempting registry pull.

**Violation consequence:** 1000 nodes simultaneously hitting the registry. Registry rate-limiting causes cascading timeouts.

---

### INV-SD7: Prologue Env Patch Ordering

**Statement:** Environment patches are applied in the order views are listed in the allocation spec. For multi-image compositions, patches from the first image's views are applied before the second image's views. Last-write-wins for conflicts.

**Enforcement:** Prologue iterates `env_patches` in declaration order. No sorting or deduplication.

**Violation consequence:** Non-deterministic PATH ordering. Different env state depending on internal iteration order.

---

### INV-SD8: EDF Inheritance Bounded

**Statement:** EDF `base_environment` chains have a maximum depth of 10 levels. Cycles are detected and rejected at submit time.

**Enforcement:** API server resolves EDF chain with depth counter and visited set. Rejects with error on cycle or depth > 10.

**Violation consequence:** Infinite resolution loop. API server hang or stack overflow.

---

### INV-SD9: Image Staging Reuses Data Mover

**Statement:** Image pre-staging uses the existing `DataStager` and `DataStageExecutor` pipeline. Images are treated as staging requests alongside data mounts, prioritized by allocation preemption class.

**Enforcement:** `DataStager.should_prefetch()` checks for unresolved/uncached images in addition to data mounts. Same `StagingRequest` type.

**Violation consequence:** Duplicate staging infrastructure. Inconsistent priority handling between data and images.

---

### INV-SD10: Namespace Joining Preserves Agent Parentage

**Statement:** When using the Podman `setns()` pattern, the spawned workload process is a direct child of the lattice-node-agent (not Podman). The agent retains signal delivery, exit status collection, and cgroup membership control.

**Enforcement:** Agent starts Podman detached, joins namespace via `setns()`, then `exec()`s the entrypoint directly. Podman's init process is stopped, not the parent of the workload.

**Violation consequence:** Agent loses process management. Cannot signal, checkpoint, or collect exit status. Zombie processes after Podman stop.

---

## Dispatch Invariants (INV-D)

These invariants govern the bridge between the Scheduling context and the Node Management context — the hand-off of an assigned allocation to the node agent that actually executes it. Introduced 2026-04-16 after the OV suite exposed that this bridge was not wired. Mix of strong (Raft) and eventual (agent/scheduler-enforced) consistency, noted per invariant.

### INV-D1: Node Has Agent Address

**Consistency:** Strong (Raft-enforced).

**Statement:** Every Node record in `GlobalState` whose `state` is one of `Ready`, `Draining`, or `Drained` has a non-empty `agent_address` recorded. A `RegisterNode` proposal with an empty, syntactically invalid, or unreachable-by-construction (e.g., `0.0.0.0:0`) address is rejected by the quorum.

**Enforcement:** Raft proposal validation on both `RegisterNode` and `UpdateNodeAddress` commands. Re-registration that would clear the address is rejected. The node's `state` cannot transition to `Ready` unless the address is present.

**Violation consequence:** Dispatcher has no call target; any allocation assigned to the node silently hangs forever (exactly the failure mode the OV suite exposed on 2026-04-16).

**Cross-context:** Node Management ↔ Consensus. The Node Agent is responsible for sending a correct address; the quorum enforces that the recorded address is present. Neither can be skipped.

---

### INV-D2: Addressless Nodes Are Scheduler-Invisible

**Consistency:** Eventual (scheduler-enforced).

**Statement:** `SchedulerStateReader::available_nodes()` returns only nodes whose `state == Ready AND agent_address.is_some()`. A node in a transient state without an address is never considered for placement.

**Enforcement:** Filter applied at the `available_nodes` read path. Defence-in-depth for INV-D1 — if the Raft invariant somehow admits an addressless Ready node (bug, migration), the scheduler still refuses to place on it.

**Violation consequence:** Allocations placed on unreachable nodes; dispatch attempts immediately exhaust retry budget and Requeue; user sees repeated re-placement churn.

---

### INV-D3: Dispatch Idempotency (Agent Side)

**Consistency:** Eventual (agent-enforced).

**Statement:** Duplicate `RunAllocation` RPCs for the same `(allocation_id, node_id)` pair — across retries, server restart, or network duplication — result in at most one Workload Process. The agent's `AllocationManager` is the source of truth; a second RunAllocation for an allocation already registered and not in a terminal phase MUST NOT spawn again; it MUST respond `accepted: true` to satisfy the caller's at-least-once requirement.

**Enforcement:** `NodeAgentServer::run_allocation` consults `AllocationManager::contains(alloc_id)` before any Runtime spawn. If present, returns `accepted: true` with a `reason: "already_running"` annotation.

**Violation consequence:** Double-spawn → resource over-use (2× CPU/GPU load), PID race with duplicate exit-code reporting, cgroup scope conflict, file-descriptor exhaustion on long-running services.

---

### INV-D4: Completion Report Idempotency (Quorum Side)

**Consistency:** Eventual (quorum-enforced).

**Statement:** A Completion Report keyed `(allocation_id, phase)` delivered by Heartbeat produces exactly one state transition in `GlobalState` for that allocation. Duplicate reports for the same `(allocation_id, phase)` — arising from agent retransmit, server restart, or heartbeat buffering — are accepted and acknowledged but MUST NOT produce additional side effects (accounting emission, audit emission, reconciliation trigger).

**Enforcement:** Quorum's heartbeat handler compares the report's `phase` to the allocation's current state **as part of the Raft command-apply step** (not at submit-time). If the transition has already been applied (state already matches the report's target), the handler short-circuits to acknowledge without any side effect. Apply-time check is mandatory because a submit-time check would race with concurrent duplicate reports through the Raft leader.

**Violation consequence:** Double-counted accounting, duplicate audit events, duplicate requeues, broken at-most-once semantics for downstream consumers (Waldur billing, sensitive audit).

---

### INV-D5: Running Implies Live Process Or In-Flight Recovery

**Consistency:** Eventual (agent + quorum together).

**Statement:** For every `Allocation` whose state in `GlobalState` is `Running` with a non-empty `assigned_nodes`, at least one of the following holds on each assigned node:
- (a) A Workload Process exists in the agent's `AllocationManager` with a live `pid`, OR
- (b) The agent is within its current `reattach_in_progress` window and reattach is in progress, OR
- (c) The agent has the allocation's Completion Report buffered for the next Heartbeat.

If none of (a), (b), (c) holds for longer than `heartbeat_interval + grace_period`, INV-D8 requires the scheduler to declare the allocation Failed.

**Reattach flag lifecycle (closes D-ADV-ARCH-06):** The `reattach_in_progress` flag on the Node record (Raft-committed per DEC-DISP-10) follows a strict lifecycle:
  1. Set to `true` ONLY on the first heartbeat following an observable re-registration event (`Command::RegisterNode` or `Command::UpdateNodeAddress`). Subsequent heartbeats may carry `true` but the `reattach_grace_period` countdown starts from the first `true` heartbeat and is absolute — it does NOT reset on subsequent `true` heartbeats.
  2. After `reattach_grace_period` elapses since the flag was first set to `true`, the quorum treats the flag as `false` for INV-D8 purposes regardless of what subsequent heartbeats say. This bounds the suppression window and prevents a malicious agent from indefinitely suppressing silent-sweep.
  3. Set to `false` is a one-way transition within the lifetime of a registration. Once cleared, the flag cannot return to `true` without a new registration.

**Enforcement:** Joint property — each side enforces its contribution. Agent populates (a) or (c); reattach populates (b); the scheduler's stuck-Running detector relies on violation of all three as the signal to move to Failed. Flag lifecycle is enforced by the quorum's `RecordHeartbeat` apply step, which tracks `reattach_first_set_at` per node and computes the effective flag value.

**Violation consequence:** Zombie `Running` allocations that consume Raft node-ownership indefinitely without executing. Blocks new work.

---

### INV-D6: Dispatch Failure Atomic Rollback

**Consistency:** Strong (Raft-enforced).

**Statement:** When a Dispatch Failure is declared by the Dispatcher (all retry attempts exhausted), the Raft proposal that effects the rollback atomically (a) transitions the allocation's state from `Running` back to `Pending`, (b) increments `dispatch_retry_count`, and (c) releases the allocation's node-ownership record. No proposal that applies a strict subset of these three operations is accepted.

**Enforcement:** Single Raft `RollbackDispatch` command that encapsulates all three. Optimistic version check: the proposal carries the allocation's observed `state_version`; Raft rejects if the state has advanced in the interim (e.g., a late Completion Report arrived and applied first).

**Violation consequence:** Partial rollback leaves either (a) the node held forever — resource leak, or (b) the allocation `Running` with no assigned node — unschedulable zombie state.

**Note:** A healthy late-arriving Completion Report racing against rollback is acceptable: the version check means rollback loses, the allocation completes normally, and retry is moot.

---

### INV-D7: Completion Report Phase Monotonicity

**Consistency:** Eventual (quorum-enforced).

**Statement:** For any allocation, the sequence of phases reported via Completion Reports advances monotonically along `Staging → Running → (Completed | Failed)`. Reports whose target phase is earlier than the allocation's current state are rejected: not applied, logged as anomaly, acknowledged so the agent does not retransmit.

**Enforcement:** Quorum's heartbeat handler enforces the ordering. Rejection produces an anomaly log line AND increments the Prometheus counter `lattice_completion_report_phase_regression_total` with labels `(reporting_node_id, allocation_id_hash, current_phase, reported_phase)` so the event is surfaced in monitoring, not only in logs.

Check order per INV-D12 Note: source-auth → monotonicity (this invariant) → idempotency (INV-D4) → apply.

**Violation consequence:** A stale heartbeat — e.g., an agent buffer replayed after a reattach — could otherwise resurrect a Completed allocation, corrupting accounting and reopening node ownership.

---

### INV-D8: Heartbeat-Bounded Completion Latency

**Consistency:** Eventual (agent + scheduler).

**Statement:** Workload Process exit on the agent produces a Completion Report on the next heartbeat, and reaches the quorum within `2 × heartbeat_interval` under normal operation (default ~20s). The scheduler's silent-node sweep declares an allocation `Failed` with reason `node_silent` when BOTH of the following hold:
  1. The allocation has been `Running` for longer than `heartbeat_interval + grace_period` since its `assigned_at` — freshly-placed allocations are exempt (closes D-ADV-ARCH-07), AND
  2. At least one of:
     (a) No heartbeat from any assigned node within `heartbeat_interval + grace_period` AND that node's effective `reattach_in_progress` is `false` (per INV-D5 lifecycle), OR
     (b) Node is heartbeating but no Completion Report has advanced this allocation's phase for longer than `grace_period` — the ghost-agent case (closes D-ADV-ARCH-09). Detection uses `allocation.last_completion_report_at` tracked by the quorum on every `ApplyCompletionReport` commit.

On firing, the scheduler proposes `ApplyCompletionReport(phase=Failed, reason=node_silent_or_no_progress)`. If the allocation's RequeuePolicy permits, reconciliation then requeues; else terminal Failed.

**Enforcement:** Agent appends Completion Reports at heartbeat-send time (never delays past one interval). Scheduler runs a silent-node sweep each cycle, evaluating both conditions (a) and (b). The silent-sweep pass runs AFTER placement but SKIPS allocations whose `assigned_at` is younger than the fresh-allocation exemption window.

**Violation consequence:** Indefinite `Running` state when agent has crashed/partitioned OR is ghosting (claiming ALREADY_RUNNING without reporting), blocking downstream DAG edges and preemption decisions.

---

### INV-D9: Orphan Process Cleanup On Agent Boot

**Consistency:** Eventual (agent-enforced).

**Statement:** On agent startup, every cgroup scope under `workload.slice/` whose allocation ID is neither present in the persisted `AgentState` nor in the currently in-flight reattach set is cleaned up (process termination + scope removal) within one heartbeat interval of boot. An audit event is emitted for each orphan.

**Enforcement:** Agent boot sequence: (1) load `AgentState`, (2) enumerate `workload.slice/`, (3) reattach known allocations, (4) terminate + remove unknown scopes, (5) emit `lost_workload` audit event per orphan.

**Violation consequence:** Resource leak that survives agent restart; stuck cgroups; user-visible "phantom load" on the node; sensitive data possibly still resident if cleanup is skipped.

---

### INV-D10: Agent Address Resolution Is Per-Attempt, Not Cached

**Consistency:** Eventual (dispatcher-enforced).

**Statement:** The Dispatcher re-reads the target Node's `agent_address` from `GlobalState` at the start of every Dispatch Attempt, including retries. It MUST NOT cache the address across attempts.

**Enforcement:** `Dispatcher::attempt(alloc_id, node_id)` begins with a fresh `GlobalState::get_node(node_id)` call. No attempt-local or dispatcher-local caching layer stores address values.

**Violation consequence:** If the agent restarts on a different port between Attempt 1 and Attempt 2 (e.g., container rescheduled), cached-address retries all fail against the stale endpoint and the allocation enters Requeue even though the agent is healthy at its new address.

---

### INV-D11: Dispatch Failures Degrade The Node, Not Only The Allocation

**Consistency:** Eventual (dispatcher + quorum + scheduler).

**Statement:** Two independent counters track dispatch failures, with distinct scopes and distinct consequences:
1. `allocation.dispatch_retry_count` — per-allocation, incremented on each Dispatch Failure (after the per-attempt retry budget on one node is exhausted). When it reaches `max_dispatch_retries`, the allocation transitions to `Failed` with reason `dispatch_exhausted` (INV-D6 / IP-14). This counter answers "is this workload broken?".
2. `node.consecutive_dispatch_failures` — per-node, incremented on every Dispatch Failure attributed to this node, reset to zero on any successful Dispatch on this node. When it reaches `max_node_dispatch_failures`, the node's state transitions from `Ready` to `Degraded` via a Raft proposal. This counter answers "is this node broken?".

A node whose agent continues to heartbeat but whose RunAllocation handler refuses or fails repeatedly MUST eventually be marked Degraded and excluded from `available_nodes()` (reinforces INV-D2).

**Enforcement:** Dispatcher increments both counters on Dispatch Failure in a single Raft proposal that follows the IP-14 RollbackDispatch. Reset of `node.consecutive_dispatch_failures` is part of the quorum's handler for the first successful `Staging` Completion Report on that node.

**Violation consequence:** Without the node-level counter, a node with a working heartbeat but a broken RunAllocation path silently absorbs every allocation placed there, each one burning the per-attempt retry budget × the per-allocation retry budget of RPC traffic. Users see inexplicable `dispatch_exhausted` failures across many allocations; operators see no node-level alerts because the node continues to appear Ready. The two counters cleanly separate allocation-attribution from node-attribution.

**Note:** `max_node_dispatch_failures` is deliberately separate from `max_dispatch_retries` because the two questions are independent. A sensible default is `5` (one node, five different allocations all failing to dispatch is strong signal that the node is broken); the allocation-side default remains `3`.

---

### INV-D12: Completion Report Source Authentication

**Consistency:** Strong (Raft-enforced).

**Statement:** A Completion Report for allocation `X` delivered on a heartbeat from node `N` is accepted by the quorum ONLY IF `N` is in `X.assigned_nodes` at the allocation's current `state_version`. Reports from nodes not in the allocation's assigned set are rejected, not applied, and logged as a `cross_node_report` anomaly with counter `lattice_completion_report_cross_node_total`.

**Enforcement:** Quorum's heartbeat-processing step validates `(reporting_node_id, allocation_id)` against `GlobalState.allocations[allocation_id].assigned_nodes` before any state transition or side effect. Validation is part of the Raft command-apply step, not a pre-submit check (consistent with D-ADV-09 resolution for INV-D4).

**Violation consequence:** A compromised or misconfigured agent could mark allocations assigned elsewhere as Failed (denial of service), Completed (fraudulent accounting, evading requeue), or Running (ghosting). mTLS (INV-A6) authenticates that the caller is *an* agent; this invariant authenticates that the caller is *the right* agent for the allocation being reported on.

**Note:** This invariant applies BEFORE INV-D4 (idempotency) and INV-D7 (monotonicity). A cross-node report is rejected without considering whether the transition would otherwise be idempotent or monotonic. Order of checks: source-auth (INV-D12) → monotonicity (INV-D7) → idempotency (INV-D4) → apply.

---

### INV-D13: Completion Report Buffer Uses Latest-Wins Per Allocation

**Consistency:** Eventual (agent-enforced).

**Statement:** The agent's Completion Report buffer contains at most one entry per allocation. When the agent enqueues a new Completion Report for allocation `X`, any prior buffered report for `X` is replaced in place; the newer report wins. The buffer bound (default 256) therefore limits the number of distinct allocations reported-on per heartbeat, not the total number of phase transitions.

**Enforcement:** Agent's heartbeat-assembly step uses a keyed collection (`HashMap<AllocId, CompletionReport>`) rather than a queue. `append(report)` upserts on `report.allocation_id`. Flush copies the current map into the heartbeat payload and clears local state. No buffer-eviction policy is needed because the buffer does not grow past the number of active allocations on this node.

**Violation consequence:** Without this mechanism, a chatty allocation (many intermediate reports) could push earlier terminal reports out of a FIFO buffer under pressure (FM-D8 scenario). Terminal reports could be lost; allocations could remain `Running` in GlobalState forever despite having exited on the node.

**Correctness by construction:** Because Local Allocation Phases advance monotonically (Prologue → Running → Epilogue → Terminal) and because the agent's phase tracker enforces that ordering, any overwrite of a buffered report for `X` replaces an earlier-phase report with a later-phase report. A terminal report, once enqueued, cannot be overwritten by a non-terminal one (there is no "backward" phase to emit). Therefore, terminal reports are preserved without requiring a priority queue or two-tier buffer.

**Note:** This invariant resolves the FM-D8 correctness claim (formerly open as A-D19 `[CRITICAL]`). The buffer bound still applies as a safety net for pathological allocation-count scenarios (e.g., >256 distinct allocations alive concurrently on one node) — in that extreme case oldest-allocation-report eviction with alarm is still the fallback, and the system still converges because (a) terminal reports are preserved for each allocation as long as it remains active, (b) the 257th simultaneous allocation is itself a violation of typical HPC node cardinality.

---

### INV-D14: Agent Address Is Bound To The Agent's Authenticated Identity

**Consistency:** Strong (Raft-enforced).

**Statement:** For any `Command::RegisterNode` or `Command::UpdateNodeAddress` submitted over an mTLS-authenticated channel, the proposal is accepted by the quorum ONLY IF the `agent_address` (or `new_address`) value appears as a Subject Alternative Name (SAN) — either DNS or IP — in the peer workload certificate presented during the mTLS handshake. Proposals whose address is not in the cert SANs are rejected with `address_not_in_cert_sans`.

**Enforcement:** The mTLS middleware in `lattice-api` extracts the peer certificate's SAN list at connection time and attaches it to the request context. The command-constructor wraps the list into the Raft proposal as `cert_sans: Vec<String>`. The quorum's `apply` step validates membership before mutating the Node record.

**Violation consequence:** Without this invariant, a compromised or misconfigured agent could claim an `agent_address` pointing to a host it does not control — redirecting Dispatcher traffic to a rogue endpoint. Attacker with a stolen workload cert could not also move to an arbitrary host of their choosing.

**Deployment requirement:** SPIRE registration entries MUST include the agent's reachable network address(es) as SANs in the SPIFFE ID's underlying X.509 SVID. Self-signed CA path MUST include the address in the CSR. Dev mode (no auth configured) bypasses this check — dev mode is not for production deployment. Documented in `docs/usage/admin-deployment.md`.

**Note:** This invariant was added 2026-04-16 resolving D-ADV-08 (adversary review of the analyst layers). See DEC-DISP-06 in `specs/architecture/interfaces/allocation-dispatch.md` for the implementation pattern.

---


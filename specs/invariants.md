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

**Enforcement:** Scheduler cost function factors f3 (fair share) and budget penalty. Eventual consistency window bounded by scheduling cycle time.

**Violation consequence (if self-correction fails):** Unbounded resource consumption by a single Tenant, starvation of others.

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

**Statement:** Lattice components use `IdentityCascade` with providers in priority order: SPIRE (preferred) → self-signed CA (fallback) → bootstrap cert (last resort). The cascade tries each provider's `is_available()` before calling `get_identity()`.

**Enforcement:** `IdentityCascade::new()` called with providers in correct order at agent/quorum startup. Provider list is immutable after construction.

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

### INV-A5: FirecREST Optionality

**Statement:** FirecREST presence or absence does not change the authentication model. Lattice authenticates directly against the IdP. If FirecREST is present, it accepts the same token as a passthrough.

**Enforcement:** No FirecREST-specific code in the auth path.

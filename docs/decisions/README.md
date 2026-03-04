# Architecture Decision Records

## Template

Each ADR follows this format:
- **Status**: Proposed | Accepted | Superseded
- **Context**: What is the problem?
- **Decision**: What did we decide?
- **Consequences**: What are the trade-offs?

---

## ADR-001: Raft for Quorum Consensus

**Status:** Accepted

**Context:** The scheduler needs a distributed control plane that avoids single-point-of-failure (Slurm's slurmctld problem). We need strong consistency for node ownership and sensitive audit, but the system schedules tens-to-hundreds of large allocations, not millions of microservices.

**Decision:** Use Raft consensus (via `openraft` crate) for the quorum. 3-5 replicas. Only node ownership changes and sensitive audit events go through Raft. Everything else is eventually consistent.

**Consequences:**
- (+) No SPOF. Quorum tolerates minority failures.
- (+) Raft is well-understood, battle-tested, good Rust implementations exist.
- (+) Consistency latency (few ms per commit) is acceptable for our scheduling granularity.
- (-) Operational complexity of running a Raft cluster (leader election, log compaction, membership changes).
- (-) Write throughput limited by Raft commit latency. Not a problem at our scale.

---

## ADR-002: Knapsack Scheduling with Composite Cost Function

**Status:** Accepted

**Context:** We need a scheduling algorithm that handles both HPC batch (topology-aware, fair-share) and cloud service (bin-packing, autoscale) workloads. Different vClusters need different optimization strategies.

**Decision:** Multi-dimensional knapsack formulation with a composite weighted cost function. Weights tunable per vCluster. Greedy solver with topology-aware backfill. Validated via RM-Replay simulator before production deployment.

**Consequences:**
- (+) Unified framework for all workload types (just change weights).
- (+) Cost function is extensible (add new factors without restructuring).
- (+) RM-Replay provides safe testing of configuration changes.
- (-) Weight tuning requires expertise and simulation. Not "plug and play."
- (-) Greedy solver is not globally optimal. Acceptable for our scale.

---

## ADR-003: uenv-First Software Delivery

**Status:** Accepted

**Context:** Users need reproducible software environments. Options: full containers (Docker/Sarus), uenv (SquashFS mount namespaces), or module systems.

**Decision:** uenv is the default software delivery mechanism. Sarus for OCI containers when isolation is needed (multi-tenant node sharing, third-party images, sensitive with enhanced isolation). No module system.

**Consequences:**
- (+) Near-zero runtime overhead (mount namespace, no container isolation overhead).
- (+) Native GPU/Slingshot access without namespace workarounds.
- (+) MPI "just works" — no network namespace translation.
- (+) Proven at CSCS scale (Alps, 10,752 GH200 GPUs).
- (-) Users must use curated uenv stacks or build their own (Spack/Stackinator).
- (-) Weaker isolation than containers — fine for trusted HPC users, needs Sarus for untrusted workloads.

---

## ADR-004: Two Strong Consistency Domains

**Status:** Accepted

**Context:** Strong consistency (Raft) has a performance cost. We need to minimize what goes through consensus while ensuring correctness for critical state.

**Decision:** Exactly two categories of state require strong consistency:
1. **Node ownership** — which tenant/vCluster/allocation owns which nodes
2. **Sensitive audit log** — all events related to sensitive node claims, data access, and isolation boundaries

Everything else (job queues, telemetry, quota accounting, session state) is eventually consistent.

**Consequences:**
- (+) Minimal Raft throughput requirements (node ownership changes are infrequent).
- (+) Sensitive compliance: audit trail is provably consistent and tamper-evident.
- (+) Job queue staleness is bounded and self-correcting (rejected proposals retry next cycle).
- (-) Eventual consistency means two vCluster schedulers might propose conflicting allocations. One gets rejected. This is a retry, not a bug.
- (-) Quota accounting can lag. Hard limits enforced at quorum (node ownership), soft limits eventually.

---

## ADR-005: Federation as Opt-In via Sovra

**Status:** Accepted

**Context:** Multi-site operation is desirable but adds significant complexity. Not all deployments need it. The trust model for cross-site operation is a hard problem.

**Decision:** Federation is a compile-time feature flag. When disabled, no Sovra dependency and no cross-site code paths. When enabled, Sovra provides the cryptographic trust layer. Each site retains full sovereignty — federation broker suggests, local scheduler decides.

**Consequences:**
- (+) Zero overhead when federation is not needed.
- (+) Sovra's sovereign key model aligns with institutional requirements (each site controls its keys).
- (+) Revocable federation (revoke workspace → instant defederation).
- (-) Additional infrastructure to operate (Sovra instances, federation brokers).
- (-) Cross-site scheduling decisions are based on eventually consistent capacity data (may be stale).

---

## ADR-006: Rust for Scheduler Core

**Status:** Accepted

**Context:** The scheduler is a long-lived, performance-critical, correctness-critical system. Options: Rust, Go, C++.

**Decision:** Rust for all performance-critical components (quorum, schedulers, node agent, API server, CLI, checkpoint broker). Go for infrastructure integration (OpenCHAMI, Sovra, federation broker). Python for user-facing SDK and tooling.

**Consequences:**
- (+) Memory safety without GC pauses (critical for scheduler latency).
- (+) Strong type system for modeling resource constraints (algebraic types for allocation states).
- (+) Excellent async/concurrency (tokio) for handling many concurrent node agent connections.
- (+) Single binary deployment for node agents (no runtime dependencies).
- (-) Steeper learning curve for contributors.
- (-) Slower initial development velocity vs. Go.
- (-) Ecosystem for HPC is smaller than C/C++ (but growing).

---

## ADR-007: Full-Node Scheduling with Intra-Node Packing

**Status:** Accepted

**Context:** Scheduling granularity: full nodes, fractional nodes, or both?

**Decision:** The scheduler reasons about full nodes. The node agent handles intra-node packing (multiple containers/uenvs on a single node) for workloads that don't need a full node (interactive sessions, small Jupyter notebooks). This is a two-level scheme: scheduler assigns nodes to vClusters, node agent packs work within allocated nodes.

**Consequences:**
- (+) Simplifies the scheduler (no cgroup negotiation between co-tenants).
- (+) Predictable performance for large jobs (no noisy neighbor at scheduler level).
- (+) Node agent can use simple bin-packing for small workloads.
- (-) Potential waste for small workloads that get a full node unnecessarily.
- (-) Mitigated by: Sarus containers with resource limits for interactive vCluster, and by grouping small workloads on designated "shared" nodes.

---

## ADR-008: Asynchronous Accounting via Waldur

**Status:** Accepted

**Context:** Lattice needs external accounting and billing but should not depend on an accounting system for core scheduling functionality. Waldur provides HPC-aware accounting, billing, and self-service portal capabilities.

**Decision:** Integrate with Waldur as an optional, feature-flagged accounting provider. Lattice pushes accounting events (allocation started/completed, resource usage) to Waldur asynchronously. Waldur can push quota updates back to Lattice. Waldur unavailability never blocks scheduling. Events are buffered in memory and persisted to disk on overflow, replayed on reconnection.

**Consequences:**
- (+) Clean separation of concerns: Lattice schedules, Waldur accounts.
- (+) Zero scheduling impact from accounting failures (events are buffered).
- (+) Waldur's self-service portal gives tenant admins quota visibility without Lattice changes.
- (+) Feature-flagged: zero overhead when accounting is not needed.
- (-) Eventually consistent accounting data (events pushed at configurable interval, default 60s).
- (-) Additional external dependency to operate (Waldur instance, API token management).
- (-) Entity mapping (Tenant↔Customer, Project↔Project) must stay synchronized.

---

## ADR-009: Two-Tier Quota Enforcement

**Status:** Accepted

**Context:** Quota enforcement must balance strictness (prevent over-allocation) with performance (don't bottleneck scheduling on consensus). Some quotas are safety-critical (node counts), others are advisory (GPU-hours budgets).

**Decision:** Two-tier quota enforcement matching the two consistency domains (ADR-004):
1. **Hard quotas** (quorum-enforced, strong consistency): `max_nodes`, `max_concurrent_allocations`, `sensitive_pool_size`. Checked during Raft proposal validation. Cannot be violated even momentarily.
2. **Soft quotas** (scheduler-enforced, eventual consistency): `gpu_hours_budget`, `fair_share_target`, `burst_allowance`. Influence scheduling score but don't hard-block. May temporarily overshoot during consistency window (~30s), self-correcting via fair-share scoring.

**Consequences:**
- (+) Hard quotas are provably enforced (Raft consensus guarantees).
- (+) Soft quotas don't bottleneck scheduling (no consensus required for budget checks).
- (+) Consistency window for soft quotas is acceptable (scheduling cycle is 5-30s, budget tracking is for billing not safety).
- (+) Integrates cleanly with Waldur (ADR-008): Waldur updates quotas, Lattice enforces them.
- (-) Soft quotas can temporarily overshoot (by design). Requires clear documentation that GPU-hours tracking is approximate.
- (-) Two enforcement paths add complexity. Developers must know which tier a quota belongs to.

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

---

## ADR-010: Native PMI-2 with Optional PMIx Sidecar

**Status:** Accepted

**Context:** Lattice replaces Slurm's `srun`, which serves as both a process launcher (fan-out to nodes) and a PMI server (rank/key-value discovery for MPI). Without a PMI provider, multi-node MPI jobs fall back to SSH for process spawning (OpenMPI's ORTE, MPICH's Hydra). SSH between compute nodes is a security risk, conflicts with network-domain isolation, and is incompatible with the sensitive workload model. The system must support OpenMPI, MPICH, and Cray MPICH.

Three options were evaluated:
1. **Full PMIx server in Rust** -- PMIx v4/v5 is ~200+ attributes, enormous implementation surface, no existing Rust implementation. Rejected: too much scope, too much risk.
2. **Embed OpenPMIx library via FFI** -- Battle-tested, full compatibility. But adds a heavy C dependency (~100K LOC), complex FFI, and still requires custom cross-node transport via gRPC.
3. **Native PMI-2 wire protocol** -- ~8 text commands over Unix domain socket. Implementable in ~1000-1500 lines of Rust. All three target MPI implementations support PMI-2 natively. The only cross-node operation (kvsfence) maps cleanly to gRPC between node agents.

**Decision:** Implement a native PMI-2 server in the node agent as the default process management interface. The node agent provides a Unix domain socket per launch, sets `PMI_FD`/`PMI_RANK`/`PMI_SIZE`, and handles cross-node KV exchange (fence) via gRPC between node agents. Optionally, for workloads requiring full PMIx (dynamic spawn, tools API, event notification), support an OpenPMIx sidecar process managed by the node agent, behind the `pmix` feature flag.

**Consequences:**
- (+) No SSH between compute nodes. Eliminates an entire class of security and operational issues.
- (+) No external C dependencies for the default path. PMI-2 is simple enough to implement and test in pure Rust.
- (+) All three target MPI implementations (OpenMPI, MPICH, Cray MPICH) work with PMI-2 out of the box.
- (+) Cross-node fence reuses the existing node-agent gRPC infrastructure (management network, mTLS).
- (+) CXI credential management integrates naturally with existing VNI/network-domain lifecycle.
- (+) PMIx available as opt-in for the ~5% of workloads that need it, without burdening the default path.
- (-) PMI-2 does not support dynamic process spawning (`MPI_Comm_spawn`). Rare in HPC but used by some frameworks.
- (-) OpenMPI users must set `OMPI_MCA_pmix=pmi2` (or Lattice sets it automatically). Minor friction.
- (-) PMIx sidecar mode adds a C dependency (OpenPMIx) and a host callback shim (~200 LOC C). Only needed when feature-flagged.
- (-) Fence performance at extreme scale (>1000 nodes) requires tree-based reduction instead of star topology. Optimization deferred until needed.

---

## ADR-011: Observability Data Out-of-Raft

**Status:** Accepted

**Context:** The system generates significant observability data: per-node telemetry (CPU, GPU, network, I/O), allocation logs (stdout/stderr), and metrics time series. This data must be queryable by users (dashboards, debugging) and by the scheduler (cost function factors like energy cost and data readiness). The question is where to store it.

Options:
1. **Raft state machine** — guarantees consistency but creates enormous write load (thousands of metric points per second across hundreds of nodes). Raft commit latency becomes the bottleneck for telemetry ingestion.
2. **External TSDB + S3** — eventually consistent but decouples observability throughput from scheduling throughput. Standard tooling (Grafana, PromQL) works out of the box.
3. **In-memory ring buffers only** — fast but volatile; node agent restart loses history; no cross-node aggregation.

**Decision:** Observability data is stored entirely outside the Raft state machine. Metrics go to an external TSDB (VictoriaMetrics). Logs are dual-path: ring buffer in the node agent for live streaming, S3 for persistent storage. The scheduler queries the TSDB for cost function inputs. Only sensitive audit events *about* observability actions (e.g., "user X attached to allocation Y") flow through Raft consensus (per ADR-004).

**Consequences:**
- (+) Raft throughput is reserved for what matters: node ownership and sensitive audit.
- (+) Standard observability tooling (Grafana, PromQL) works without custom integration.
- (+) Telemetry pipeline failures do not disrupt scheduling or allocation lifecycle.
- (+) TSDB handles retention, downsampling, and high-cardinality queries natively.
- (-) Metrics are eventually consistent (~30s lag). Scheduler cost function inputs may be slightly stale.
- (-) TSDB is an additional infrastructure dependency to operate.
- (-) Log persistence depends on S3 availability; brief gaps possible during S3 outages (ring buffer covers live access).

---

## ADR-012: Allocation as Universal Work Unit

**Status:** Accepted

**Context:** The system must schedule both finite work (training runs, simulations, CI jobs) and infinite work (inference services, monitoring daemons, interactive notebooks). Slurm treats these as fundamentally different (jobs vs. "perpetual" jobs with workarounds). Kubernetes treats everything as a pod/deployment but lacks HPC scheduling semantics. We need a single abstraction that spans both worlds without losing scheduling precision.

Options:
1. **Two separate types (Job and Service)** — clear semantics per type, but duplicates scheduling logic, quota enforcement, preemption policy, and API surface. Every feature must be implemented twice.
2. **Always bounded (Slurm model)** — services require walltime workarounds (submit with max walltime, auto-resubmit). Clumsy and fragile.
3. **Always unbounded (K8s model)** — batch jobs require explicit termination signals. Cannot express "run until completion" natively.
4. **Single type with lifecycle variants** — one Allocation with lifecycle: Bounded | Unbounded | Reactive.

**Decision:** A single `Allocation` type is the universal work unit. The `lifecycle` field determines duration semantics: `Bounded` (has walltime, completes or is killed), `Unbounded` (runs until cancelled, auto-restarts on failure), `Reactive` (scales in response to metrics/load). All scheduling, quota, preemption, checkpoint, and telemetry policies operate on Allocations uniformly. Task Groups (Slurm job arrays) and DAGs (dependency graphs) compose Allocations.

**Consequences:**
- (+) Unified scheduling: one cost function, one knapsack solver, one preemption engine for all workload types.
- (+) Simpler API: users learn one submission model. Services and batch jobs differ only in lifecycle field.
- (+) Quota and fair-share accounting is uniform — no special cases for services vs. jobs.
- (+) DAG dependencies can mix bounded and unbounded allocations (e.g., training job → inference service).
- (-) Lifecycle variants add complexity to the state machine (Bounded has walltime enforcement; Unbounded has restart policy; Reactive has scaling triggers).
- (-) Users coming from Slurm must learn that "job" and "service" are the same thing with different lifecycle.

---

## ADR-013: Network Domains via Hardware VNIs

**Status:** Accepted

**Context:** Multi-tenant HPC requires network isolation between allocations. On Slingshot/Ultra Ethernet fabrics, the NIC supports Virtual Network Identifiers (VNIs) that provide hardware-enforced L3 isolation. Alternative approaches exist in software.

Options:
1. **Software-based isolation (Linux network namespaces, iptables)** — can be bypassed by privileged processes, adds per-packet overhead, difficult to audit at scale, incompatible with RDMA.
2. **No network isolation** — all allocations share L2/L3. Unacceptable for multi-tenant security and sensitive workloads.
3. **Full overlay network (Kubernetes CNI model)** — adds encapsulation overhead, incompatible with Slingshot fabric semantics, destroys RDMA performance.
4. **Hardware VNI isolation** — Slingshot NIC enforces isolation at line rate, zero software overhead, auditable via fabric manager.

**Decision:** Network isolation is enforced at the Slingshot hardware level via VNIs. Each network domain maps to a VNI allocated from a managed pool. Allocations in the same domain share a VNI and have L3 reachability. Allocations in different domains are hardware-isolated. VNI assignment is eventually consistent (node agents configure NICs based on quorum-reported domain membership). Sensitive allocations get unique per-allocation domains with encrypted RDMA (Ultra Ethernet).

**Consequences:**
- (+) Zero-overhead isolation — no per-packet software processing, RDMA performance preserved.
- (+) Hardware-enforced — cannot be bypassed by user processes, even with root inside a container.
- (+) Auditable via fabric manager — network domain membership is visible to operators.
- (+) Naturally integrates with CXI credential management for MPI (ADR-010).
- (-) Tied to Slingshot/Ultra Ethernet hardware. Non-Slingshot deployments need a software fallback.
- (-) VNI pool is finite (default: 3095). Exhaustion blocks new domain creation.
- (-) VNI configuration propagation to NICs adds latency to allocation startup (~50ms).

---

## ADR-014: Conformance Fingerprinting for Configuration Drift Detection

**Status:** Accepted

**Context:** Multi-node GPU workloads (distributed training, MPI simulations) are sensitive to configuration heterogeneity. Nodes with different GPU driver versions, NIC firmware, or kernel versions can cause subtle correctness issues (NCCL version mismatches, libfabric ABI incompatibilities) or performance degradation. Slurm has no built-in mechanism to detect this; operators discover it via user bug reports.

Options:
1. **No tracking** — silent failures; users debug configuration drift themselves.
2. **Exact node-by-node attribute matching** — too strict; every firmware update requires simultaneously updating all nodes or scheduling breaks.
3. **Conformance fingerprint (hash of driver/firmware/kernel)** — nodes with identical fingerprints are grouped into cohorts; scheduler places multi-node jobs on same-cohort nodes.
4. **Scheduler-driven remediation** — scheduler triggers firmware updates on non-conforming nodes. Out of scope; OpenCHAMI handles infrastructure.

**Decision:** Each node agent computes a conformance fingerprint (SHA-256 of GPU driver version, NIC firmware version, BIOS version, kernel version) and reports it with heartbeats. The quorum groups nodes into conformance cohorts. The cost function factor `f₉: conformance_fitness` penalizes multi-node allocations that would span cohorts. Allocations can set `require_conformance: true` to hard-require same-cohort placement. Conformance drift on sensitive nodes triggers immediate drain (not remediation — that's OpenCHAMI's job).

**Consequences:**
- (+) Detects configuration drift before it causes user-visible failures.
- (+) Soft by default (penalty, not hard block) — avoids scheduling starvation during rolling updates.
- (+) Hard mode available for workloads that need it (`require_conformance`).
- (+) Sensitive nodes get stricter enforcement (drain on drift) for compliance.
- (-) Fingerprint granularity is coarse. Two nodes with different BIOS settings but same BIOS version have the same fingerprint.
- (-) Multi-node jobs with `require_conformance` may wait longer for same-cohort nodes.
- (-) Rolling firmware updates temporarily create many small cohorts, reducing scheduling flexibility.

---

## ADR-015: Attach via nsenter

**Status:** Accepted

**Context:** Users need interactive terminal access to running allocations for debugging, monitoring, and interactive workflows (equivalent to Slurm's `srun --pty bash` into a running job). The question is how to provide this without compromising isolation or consuming scheduling resources.

Options:
1. **Create a new "attach" allocation on the same node** — goes through the scheduler queue; consumes quota; adds latency; overkill for a debugging session.
2. **SSH into the compute node** — requires SSH key distribution between login and compute nodes; security risk; incompatible with network domain isolation; operationally fragile.
3. **nsenter from node agent** — the node agent enters the allocation's mount/PID namespace via Linux `nsenter`; bidirectional gRPC stream provides the PTY. No new resource allocation, no SSH.
4. **Direct socket from user to container** — requires host filesystem access; less secure; doesn't work with uenv (no container to connect to).

**Decision:** Attach uses `nsenter` executed by the node agent. The user's `lattice attach <id>` command opens a bidirectional gRPC stream to the API server, which forwards to the node agent hosting the allocation. The node agent spawns a shell inside the allocation's namespace via `nsenter`. No new allocation is created, no quota is consumed, and no SSH is involved.

**Consequences:**
- (+) Instant attach — no scheduler queue, no resource allocation.
- (+) No SSH infrastructure needed on compute nodes.
- (+) Works identically for uenv and Sarus allocations (both use Linux namespaces).
- (+) Attach sessions are logged as observability events (sensitive: Raft-committed audit entry).
- (-) Requires the node agent to have `CAP_SYS_ADMIN` / sufficient privileges for `nsenter`.
- (-) Attach shares the allocation's resource limits — a heavy debugging tool could impact the running workload.
- (-) If the node agent is down, attach is unavailable (no fallback).

---

## ADR-016: Two-Tier API (Intent API + Compatibility Layer)

**Status:** Accepted

**Context:** Lattice must serve two audiences: (1) new users and AI agents who benefit from a declarative, intent-based API ("I need 64 GPU nodes for 2 hours with this data"), and (2) existing Slurm users who have years of scripts using `sbatch`, `squeue`, `scancel`. Supporting both without maintaining two scheduling engines requires a clear layering decision.

Options:
1. **Single imperative API (Slurm-style)** — familiar to HPC users but locks the system into Slurm's abstractions (partitions, job steps, GRES). Cannot express reactive scaling or data staging intent.
2. **Single declarative API (Intent-only)** — clean design but forces all existing users to rewrite scripts immediately. Migration barrier too high.
3. **Dual engines** — one for Intent, one for Slurm compat. Code duplication, inconsistent scheduling behavior, unmaintainable.
4. **Two-tier: Intent API as primary, Compatibility API as thin mapping** — Slurm commands are translated to Intent API calls. One scheduling engine, one state machine, one set of semantics.

**Decision:** The Intent API is the primary and only scheduling interface. The Compatibility API (`sbatch`, `squeue`, `scancel` and their `lattice submit`, `lattice status`, `lattice cancel` equivalents) is a stateless translation layer that maps Slurm directives to Intent API fields. All scheduling decisions, state transitions, and quota enforcement happen through the Intent API path. The compat layer produces warnings for unsupported directives but never errors (graceful degradation for migration).

**Consequences:**
- (+) One scheduling engine, one code path, one set of tests.
- (+) Gradual migration: existing scripts work on day one via compat layer.
- (+) Intent API can evolve freely without Slurm compatibility constraints.
- (+) AI agents use the Intent API directly — no impedance mismatch.
- (-) Some Slurm features have no mapping (hetjob, burst buffer, GRES beyond GPU). Users get warnings.
- (-) Compat layer must be maintained and tested against Slurm script variations.
- (-) Users may stay on compat layer indefinitely, never adopting Intent API features.

---

## ADR-017: Eventual Consistency for Job Queues

**Status:** Accepted

**Context:** When a user submits an allocation, how quickly must the system guarantee that the submission is durable and schedulable? Raft consensus provides strong guarantees but adds latency (few ms per commit) and throughput limits. Job queues see bursts (hundreds of submissions in seconds during class assignments or automated pipelines).

Options:
1. **Synchronous Raft commit on every submission** — strong guarantee but adds 10-100ms per submission, bottlenecks the API under burst load, scheduler throughput limited by Raft commit latency.
2. **Eventually consistent with bounded staleness** — submission is acknowledged immediately (stored in-memory queue), committed to Raft asynchronously on the next scheduling cycle. Staleness bounded by scheduling cycle time (~5-30s).
3. **Optimistic with no retry** — submissions may be silently lost on leader failover. Unacceptable.

**Decision:** Job queue state is eventually consistent. Allocation submissions are acknowledged immediately by the API server and placed in the vCluster scheduler's in-memory queue. The scheduler proposes allocations to the quorum on each scheduling cycle; the quorum validates and commits node ownership (strong consistency). If the API server fails between acknowledgment and the next scheduling cycle, the submission is lost — but the user receives an allocation ID and can query status, which will show "not found" (detectable failure, not silent). In practice, the window is <30s and API server failures are rare.

**Consequences:**
- (+) Submission API is fast (<5ms) regardless of Raft cluster health.
- (+) Burst submissions don't bottleneck on consensus.
- (+) Scheduling cycle naturally batches proposals, reducing Raft commit count.
- (-) Submissions can be lost on API server crash (between ack and next cycle). Mitigated by: client retries on "not found" status, and API server persistence to disk (WAL) as future enhancement.
- (-) Two schedulers may independently queue the same submission if load-balanced. Deduplication by allocation ID at quorum level.

---

## ADR-018: Scheduler-Coordinated Checkpointing

**Status:** Accepted

**Context:** Preemption requires evicting running allocations to free resources for higher-priority work. Killing allocations without warning wastes all computed progress. Checkpointing preserves progress but has cost: I/O bandwidth for writing state, compute time lost during checkpoint, and storage for checkpoint data. The question is who decides when to checkpoint.

Options:
1. **User-initiated checkpointing** — user inserts checkpoint calls in their code. Does not solve the preemption problem (scheduler cannot wait for user to decide).
2. **Periodic automatic checkpointing (fixed interval)** — simple but wasteful. Short intervals waste I/O on stable workloads; long intervals lose too much progress on preemption.
3. **Transparent checkpointing (DMTCP) without cost model** — works for any application but causes I/O storms when many allocations checkpoint simultaneously. No way to prioritize which allocations to preempt.
4. **Scheduler-coordinated with cost function** — scheduler evaluates checkpoint value vs. cost per allocation, decides when and which allocations to checkpoint for preemption.

**Decision:** Checkpointing is scheduler-coordinated. The cost function evaluates `checkpoint_value = resource_freed × preemptability + backlog_relief` vs. `checkpoint_cost = write_time + compute_waste + storage_cost`. The scheduler triggers checkpoints by sending `CHECKPOINT_HINT` to the node agent, which forwards to the application (via signal, shmem flag, or gRPC callback). Applications declare their checkpoint capability (`signal`, `shmem`, `grpc`, `dmtcp`, or `none`). Applications with `none` are either non-preemptible or killed without checkpoint. Backlog pressure increases checkpoint aggressiveness (more allocations waiting → more willing to preempt).

**Consequences:**
- (+) Checkpoint decisions are globally optimal (scheduler has full visibility of queue, resources, priorities).
- (+) Avoids I/O storms (scheduler staggers checkpoints across time and storage bandwidth).
- (+) Backlog-responsive: system becomes more aggressive about freeing resources when demand is high.
- (+) Applications retain control of checkpoint mechanics (signal handler, custom format).
- (-) Applications must implement checkpoint support to benefit. Unsupported applications are either non-preemptible or lose progress.
- (-) Cost function calibration requires tuning (write bandwidth, storage cost per GB).
- (-) Checkpoint hint is advisory — application may take too long, forcing a hard kill after timeout.

---

## ADR-019: Eventually Consistent Node Capacity

**Status:** Accepted

**Context:** The scheduler needs two kinds of information about nodes: (1) ownership — which tenant/vCluster/allocation owns the node, and (2) capacity — current health, GPU utilization, temperature, available memory. Ownership must be strongly consistent (ADR-004) to prevent double-assignment. But capacity data changes frequently (every heartbeat, ~10s) and is used for scoring, not for correctness.

Options:
1. **All node updates through Raft** — ownership and capacity in one consistent view. But heartbeats every 10s × hundreds of nodes = thousands of Raft writes per minute. Commit latency becomes the scheduling bottleneck.
2. **All node updates eventually consistent** — fast but ownership conflicts are possible. Two schedulers could assign the same node simultaneously.
3. **Split: ownership via Raft, capacity via eventual consistency** — ownership changes are rare (scheduling cycles) and go through Raft. Capacity updates are frequent (heartbeats) and propagated via gossip or direct reporting.

**Decision:** Node ownership (tenant, vCluster, allocation assignment) is Raft-committed (strong consistency). Node capacity (health, utilization, temperature, conformance fingerprint) is eventually consistent — node agents report to the quorum leader, which updates in-memory state without Raft commit. The scheduler reads the latest reported capacity when scoring. Stale capacity data may cause suboptimal placement but never incorrect ownership.

**Consequences:**
- (+) Heartbeats do not bottleneck Raft. Hundreds of nodes can report every 10s without consensus overhead.
- (+) Scheduling cycle time is decoupled from Raft commit latency for capacity reads.
- (+) Ownership consistency is preserved — double-assignment is impossible.
- (-) Capacity staleness can cause suboptimal decisions (e.g., scheduling on a node whose GPU just failed but hasn't reported yet). Bounded by heartbeat interval.
- (-) Two levels of consistency require developers to know which fields are strong vs. eventual.

---

## ADR-020: Sensitive Node Claims by User Identity

**Status:** Accepted

**Context:** Sensitive (regulated, high-security) workloads require provable isolation and audit trails that satisfy regulatory requirements (e.g., data protection laws, institutional compliance). The question is what identity is recorded as the "owner" of a sensitive node allocation: the tenant (organizational unit), a role, or the specific user.

Options:
1. **Tenant-owned** — the organizational unit owns the nodes. Cannot prove which individual accessed which data. Insufficient for regulatory audit ("who accessed patient records?").
2. **Role-based** — a role (e.g., "researcher") owns the nodes. Same problem: multiple users share a role; individual accountability is lost.
3. **User-owned (OIDC subject)** — the authenticated user's identity (from OIDC token) is recorded in the Raft-committed audit log as the owner. Every data access, attach session, and log retrieval is tied to a specific person.

**Decision:** Sensitive allocations are claimed by the authenticated user's OIDC subject identifier, not by the tenant or a role. The quorum records the user identity in the Raft-committed audit log. All subsequent actions on the allocation (data access, attach, log retrieval) are logged with user identity. Nodes are wiped on release (OpenCHAMI secure erase) with wipe confirmation recorded in the audit log. Audit retention is 7 years.

**Consequences:**
- (+) Individual accountability: every action is tied to a specific authenticated person.
- (+) Regulatory defensibility: audit trail shows who claimed what, when, and what they did.
- (+) Wipe-on-release with Raft-committed confirmation provides provable data destruction.
- (+) 7-year retention satisfies most regulatory frameworks.
- (-) User identity must be available at claim time (requires OIDC authentication, no service accounts for sensitive claims).
- (-) Sensitive allocations cannot be transferred between users (the claim is to a specific identity).
- (-) Wipe-on-release adds latency to node return-to-pool (10-30 minutes for secure erase).

---

## ADR-021: Data Staging as Invisible Background Pre-stage

**Status:** Accepted

**Context:** Many HPC workloads require large datasets (TBs) that may reside on warm or cold storage tiers. If data is not on the hot tier (VAST NFS/S3) when the allocation starts, the first minutes of compute time are wasted on I/O. The question is when and how to move data to the hot tier.

Options:
1. **User-managed staging** — user runs a separate staging job before the compute job. Shifts responsibility; users who forget waste compute time. Incompatible with multi-tenant fairness (staging time counted against user).
2. **Blocking inline staging** — allocation starts, blocks on data transfer before running the entrypoint. User sees unpredictable startup latency. If staging fails, the allocation is stuck in a running-but-waiting state, consuming resources.
3. **Background pre-staging during queue wait** — when an allocation is queued and declares data mounts with `tier_hint: hot`, the data mover begins warming data to the hot tier while the allocation waits in the queue. Queue wait time becomes productive.
4. **Post-allocation staging on compute nodes** — wastes compute resources on I/O; saturates node-local network bandwidth.

**Decision:** Data staging runs as a background process during queue wait time. The allocation transitions through a `Staging` state where the data mover pre-stages declared data mounts from warm/cold to hot tier. The cost function factor `f₅: data_readiness` scores how ready an allocation's data is: fully staged allocations score higher and are scheduled sooner. Allocations whose data is not yet ready can still be scheduled if resources are available (staging continues during prologue). Staging failure is non-fatal — the allocation starts with a warning, and the entrypoint may encounter I/O latency.

**Consequences:**
- (+) Queue wait time is no longer wasted — data moves while the allocation waits.
- (+) Users don't need to manage staging manually; just declare data mounts.
- (+) Scheduler can prioritize data-ready allocations, improving overall throughput.
- (+) Non-blocking: staging failure degrades performance but doesn't prevent execution.
- (-) Adds complexity to the allocation state machine (Staging state, data mover integration).
- (-) Hot tier must have capacity for pre-staged data. Over-staging wastes hot tier space.
- (-) Cost function tuning: `f₅` weight determines how much data readiness influences scheduling order.

---

## ADR-022: Three-Layer Telemetry Pipeline

**Status:** Accepted

**Context:** The system needs telemetry for three consumers: (1) operators (dashboards, alerts), (2) users (debugging, performance analysis), and (3) the scheduler (cost function inputs: GPU utilization, network congestion, energy cost). Each has different resolution, latency, and retention requirements. The pipeline must handle hundreds of nodes producing thousands of metric points per second.

Options:
1. **In-memory ring buffers only** — fast, low overhead. But volatile: node agent restart loses history. No cross-node aggregation for dashboards. Insufficient for scheduler feedback (requires historical trends).
2. **Direct eBPF-to-S3 pipeline** — durable but high latency. No live metrics for dashboards. Raw data too granular for efficient query.
3. **Stream all metrics to Raft state machine** — consistent but bloats the state machine. Raft commit latency becomes the telemetry bottleneck. Fundamentally wrong abstraction.
4. **Three-layer: collect (eBPF) → aggregate (configurable resolution) → store (external TSDB)** — each layer optimized for its purpose.

**Decision:** Telemetry follows a three-layer pipeline. Layer 1: eBPF programs (always-on, <0.3% overhead) collect kernel-level metrics at high resolution. Layer 2: the node agent aggregates at configurable resolution (production: 30s bicubic smoothing, debug: 1s raw, audit: access logs). Layer 3: aggregated metrics are pushed to an external TSDB (VictoriaMetrics) for storage, query, and alerting. The scheduler queries the TSDB for cost function inputs. Users query the TSDB via Grafana or the `lattice top`/`lattice metrics` commands.

**Consequences:**
- (+) Each layer is independently scalable and replaceable (swap TSDB, change eBPF programs, adjust resolution).
- (+) eBPF collection is always-on with negligible overhead — no sampling trade-offs.
- (+) Configurable resolution per use case: fine-grained for debugging, coarse for production.
- (+) Standard tooling (Grafana, PromQL, AlertManager) works without custom integration.
- (+) Telemetry pipeline failure does not affect scheduling (graceful degradation: stale cost function inputs).
- (-) Three layers add operational complexity (eBPF programs, agent aggregation config, TSDB deployment).
- (-) End-to-end latency from event to queryable metric is ~30s in production mode.
- (-) eBPF programs require kernel version compatibility and `CAP_BPF` on nodes.

---

## ADR-023: vCluster as Soft Isolation Boundary

**Status:** Accepted

**Context:** Different workload types need different scheduling policies: HPC batch needs backfill with topology packing, ML training needs fair-share with GPU affinity, services need bin-packing with autoscale, sensitive needs dedicated reservation. A single scheduler cannot optimize for all simultaneously. But hard partitioning wastes resources when one workload type is idle while another is starved.

Options:
1. **Hard partitioning (dedicated node pools per workload type)** — simple isolation but guaranteed waste. If the ML training pool is 50% idle and HPC batch is oversubscribed, resources sit unused.
2. **Single global scheduler with workload-type heuristics** — no waste but cannot apply fundamentally different policies (backfill vs. bin-pack) simultaneously. Policy conflicts create unpredictable behavior.
3. **Opaque vClusters (cannot see each other)** — avoids conflicts but makes cross-vCluster fairness impossible. Borrowing is non-deterministic because the lending vCluster doesn't know its own utilization relative to others.
4. **Soft vClusters with global visibility** — each vCluster has its own scheduler and cost function weights, but all schedulers see the global node ownership state via the quorum. Borrowing is explicit and policy-driven.

**Decision:** vClusters are soft isolation boundaries. Each vCluster has an independent scheduler instance with its own cost function weights (ADR-002) and scheduling algorithm (backfill, bin-pack, reservation, FIFO). All schedulers read the same global state from the quorum. vClusters have base allocations (guaranteed node counts) and can borrow from other vClusters with explicit priority and duration. Borrowed nodes are returned when the lending vCluster needs them (preemption of borrowed allocations at lower priority). The quorum enforces that proposals from different vCluster schedulers don't conflict (node ownership is Raft-committed).

**Consequences:**
- (+) Each workload type gets an optimized scheduler without one-size-fits-all compromises.
- (+) No waste: idle resources in one vCluster are available to others via borrowing.
- (+) Fair-share is globally visible: `f₃` can compare a tenant's usage across all vClusters.
- (+) Borrowing is explicit and reversible: lending vCluster retains priority over its base allocation.
- (-) Multiple schedulers proposing simultaneously can cause Raft proposal conflicts (one rejected, retried next cycle). Not a bug, but adds latency under contention.
- (-) Borrowing policy configuration is complex (priority levels, max borrow duration, return grace period).
- (-) Operators must understand that vClusters are not security boundaries — they are scheduling policy boundaries. Tenant isolation is provided by RBAC and network domains, not vClusters.

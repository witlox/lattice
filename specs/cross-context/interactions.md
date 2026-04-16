# Cross-Context Interactions

Integration points between bounded contexts. Each interaction specifies the upstream/downstream relationship, the contract, and what happens when things go wrong.

## Interaction Map

```
                         ┌──────────────┐
                         │   TENANT &   │
                         │    ACCESS    │
                         └──┬───────┬──┘
                   quotas   │       │  RBAC
              ┌─────────────┘       └────────────────┐
              ▼                                      ▼
     ┌────────────────┐   proposals    ┌──────────────────┐
     │   CONSENSUS    │◄──────────────│    SCHEDULING     │
     │  (Raft quorum) │───────────────►│                  │
     └───┬────────┬───┘  committed     └──┬──────────┬────┘
         │        │       state           │          │
  notifies│     ownership              scores     assigns
         │        │                   using        work
         ▼        ▼                      │          │
┌─────────────────────┐    telemetry  ┌──┴──────────┴─────┐
│   NODE MANAGEMENT   │──────────────►│   OBSERVABILITY    │
│                     │               │                    │
└─────────────────────┘               └────────────────────┘

Cross-context coordinators (stateless):
  Checkpoint Broker:  Scheduling ──reads──► decides ──acts──► Node Management
  Accounting (Waldur): Scheduling ──events──► Waldur ──quotas──► Tenant & Access
  Data Staging:        Scheduling ──declares──► Node Management ──executes──► VAST

Cross-cutting infrastructure:
  SecretResolver: Config/Vault/Env ──secrets──► lattice-api, lattice-quorum (startup-only)

hpc-core integration (trait-based, no code coupling):
  Node Mgmt ──hpc-node──► PACT (namespace handoff, mount sharing) OR self-service
  Consensus ──hpc-audit──► SIEM (standardized audit events)
  Node Mgmt ──hpc-identity──► SPIRE/self-signed/bootstrap (mTLS identity)
  CLI ──hpc-auth──► IdP (OAuth2 token management)
```

## Integration Points

### IP-01: Scheduling → Consensus (Proposal)

**Direction:** Scheduling proposes; Consensus validates and commits.

**Contract:**
- Input: Proposal containing desired node ownership changes (assign nodes to allocation)
- Validation: Consensus checks INV-S1 (exclusive ownership), INV-S2 (hard quotas), INV-S5 (sensitive isolation)
- Output: Commit (ownership updated, notified) or Reject (reason: ownership conflict, quota exceeded, etc.)
- Guarantee: At most one proposal for the same node committed per Raft log entry

**Failure modes:**
- Quorum unavailable → proposals fail → scheduler retries next cycle. No data loss.
- Leader election → proposals delayed 1-3s → scheduler retries.
- Proposal rejected → scheduler re-scores and re-proposes next cycle. Expected under concurrent vCluster scheduling.

**Ordering:** Proposals are serialized by Raft. Two vCluster schedulers proposing conflicting allocations: first committed wins, second rejected.

**Duplication:** Scheduler may re-propose the same allocation if it didn't observe the commit. Quorum deduplicates by allocation ID.

---

### IP-02: Scheduling → Node Management (Dispatch, via Dispatcher)

*Rewritten 2026-04-16 after OV suite exposed that the previously-documented "Consensus → Node Management notification" was never implemented. The correct flow is polled by a Dispatcher component in Node Management, not pushed by Consensus.*

**Direction:** Scheduling commits placement to Consensus; a Dispatcher (in Node Management, hosted by the `lattice-api` process) observes committed state and hands off to the target node agent.

**Contract:**
- Input (Raft-observed): an Allocation with `state == Running` whose `assigned_nodes` is non-empty and for which no Completion Report has yet advanced the phase to Staging or beyond.
- Action: Dispatcher resolves each assigned Node's `agent_address` from GlobalState and issues `NodeAgentService.RunAllocation` on each.
- Output: Agent responds `accepted` (normal case, or `already_running` per INV-D3); the allocation moves to Staging on first Completion Report from the agent.
- Guarantee: Dispatch is at-least-once from the server's perspective (retries on failure), at-most-once from the agent's perspective (INV-D3).

**Why Dispatcher, not direct-from-Scheduler:** The scheduler's concern is "what runs where." The Dispatcher's concern is "making sure the agent heard us." Separating them lets the scheduler advance to its next cycle without blocking on agent I/O, and lets dispatch retries occur independently of scheduling cadence.

**Failure modes:**
- Agent unreachable → bounded retry (3 attempts, 1s/2s/5s backoff) → Dispatch Failure → Rollback via IP-14 (allocation returns to Pending). Both counters from INV-D11 are incremented: `allocation.dispatch_retry_count` and `node.consecutive_dispatch_failures`.
- Prologue fails inside the agent → agent emits Completion Report phase "Failed" on next heartbeat (IP-03) → quorum applies state transition → standard requeue path per RequeuePolicy.
- Duplicate dispatch (dispatcher restart, network duplication) → agent's `AllocationManager` deduplicates (INV-D3); no second Workload Process.
- Address change between attempts → Dispatcher re-reads per attempt (INV-D10). Eventually-consistent: a lag between `UpdateNodeAddress` commit and the next attempt may still see the stale address, absorbed by retries or rollback.
- Node with working heartbeat but broken RunAllocation (agent up, handler failing) → `node.consecutive_dispatch_failures` (INV-D11) increments across allocations. Reaching `max_node_dispatch_failures` transitions the node to `Degraded` and removes it from `available_nodes()`. A successful Dispatch on the node resets the counter.

**Ordering:** Not required between allocations. A Dispatcher worker pool may process multiple allocations in parallel.

**Duplication:** Same allocation seen across multiple scheduler cycles without a phase transition: dispatcher retries on each observation (bounded by attempt budget). INV-D3 absorbs the agent-side.

---

### IP-03: Node Management → Consensus (Heartbeat, including Completion Reports)

**Direction:** Node agents report to quorum.

**Contract:**
- Input: Heartbeat (health, conformance fingerprint, running allocation count, sequence number, **Completion Reports list**).
- Completion Reports are per-allocation state-change records accumulated since the previous heartbeat; each carries `(allocation_id, phase, optional pid, optional exit_code, optional reason)`. Bounded list (default 256 entries; oldest-drop plus alarm on overflow).
- Processing:
  - Node health updates are in-memory (eventually consistent), not Raft-committed.
  - Node ownership state changes (Degraded, Down) triggered by missed heartbeats ARE Raft-committed.
  - Completion Reports ARE Raft-committed — each report that advances an allocation's state produces a Raft proposal, subject to idempotency (INV-D4) and monotonicity (INV-D7).

**Failure modes:**
- Heartbeat missed → Degraded (after heartbeat_timeout). Down (after grace_period).
- Heartbeat storm → quorum-side rate limiting (max 1 per interval per node).
- Stale heartbeat (replay) → sequence number check rejects.
- Duplicate Completion Report (same allocation_id/phase) → applied once by INV-D4; acknowledged to caller so agent stops retransmitting. Idempotency check happens at Raft command-apply step, not at submit-time.
- Completion Report with regressed phase (e.g., Running after Completed) → rejected + logged per INV-D7; counter `lattice_completion_report_phase_regression_total` emitted; acknowledged so agent does not retransmit.
- Completion Report from a node not in the allocation's `assigned_nodes` → rejected per INV-D12; counter `lattice_completion_report_cross_node_total` emitted. Check happens before monotonicity and idempotency checks.
- Completion Reports buffer cardinality limit — keyed by allocation_id per INV-D13. The buffer holds at most one entry per allocation with latest-phase-wins semantics; no FIFO eviction. If distinct active-allocation cardinality exceeds the bound (anomalous for typical HPC nodes), new allocations' first reports are rejected locally with a `dispatch_report_buffer_exhausted` alarm until a slot frees (see FM-D8).

**Validation order at the receiver** (Raft command-apply step):
  1. **INV-D12** — is `reporting_node ∈ allocation.assigned_nodes`? If not → reject + `cross_node_report` anomaly.
  2. **INV-D7** — does `report.phase` strictly advance the current phase? If not → reject + phase-regression counter.
  3. **INV-D4** — has this `(allocation_id, phase)` already been applied? If so → short-circuit (no-op ack).
  4. Apply state transition.

**Ordering:** Heartbeats are idempotent by sequence number. Completion Reports within a heartbeat are processed in order; across heartbeats, INV-D7 enforces allocation-phase monotonicity regardless of delivery order.

---

### IP-04: Scheduling → Node Management (via Consensus, Checkpoint Broker)

**Direction:** Scheduler decisions flow to node agents through two paths.

**Path A: Direct assignment** — Scheduling → Consensus → Node Management (IP-01 → IP-02)

**Path B: Checkpoint/preemption** — Scheduling evaluates preemption → Checkpoint Broker sends CHECKPOINT_HINT to node agent → node agent forwards to application → application checkpoints → node agent reports completion → quorum releases nodes.

**Contract (Path B):**
- Input: CHECKPOINT_HINT (advisory, not mandatory)
- Timeout: checkpoint_timeout (default 10min). One 50% extension for slow-but-responsive applications.
- Output: Checkpoint complete (allocation → Suspended) OR timeout (SIGTERM → SIGKILL → Failed)
- Guarantee: Nodes not released until checkpoint completes or timeout expires.

**Failure modes:**
- Application unresponsive to hint → timeout → forced kill. Allocation Failed, not Suspended.
- Node agent crash during checkpoint → quorum detects via heartbeat → node Down → allocation requeued.
- Checkpoint broker crash → no hints sent → allocations effectively non-preemptible until broker restarts.

---

### IP-05: Node Management → Observability (Telemetry Push)

**Direction:** Node agents produce telemetry; Observability pipeline collects and stores.

**Contract:**
- Layer 1: eBPF programs collect kernel metrics (always-on, <0.3% overhead)
- Layer 2: Node agent aggregates at configurable resolution (30s prod, 1s debug, access logs audit)
- Layer 3: Aggregated metrics pushed to external TSDB (VictoriaMetrics) every push interval
- Logs: Dual-path — ring buffer (live) + S3 (persistent)

**Failure modes:**
- TSDB unavailable → metrics buffered briefly in node agent, then dropped. No impact on scheduling.
- S3 unavailable → log persistence paused. Ring buffer covers live access. Gap in historical logs.
- eBPF program crash → agent restarts eBPF. Brief telemetry gap.

**Ordering:** Metrics are timestamped. Out-of-order arrival handled by TSDB (VictoriaMetrics supports out-of-order ingestion).

---

### IP-06: Observability → Scheduling (Cost Function Inputs)

**Direction:** Scheduler queries TSDB for cost function factors.

**Contract:**
- f5 (data_readiness): fraction of input data on hot tier
- f7 (energy_cost): time-varying electricity price
- f6 (backlog_pressure): derived from queue state (internal to scheduler, but validated against TSDB queue depth)
- Query: PromQL via TSDB HTTP API. Scoped to allocation/node labels.

**Failure modes:**
- TSDB unavailable → scheduler uses stale values from last successful query. Suboptimal scoring but not incorrect.
- Stale metrics (TSDB lag ~30s) → scheduling decision based on slightly old data. Acceptable at HPC scheduling granularity.

---

### IP-07: Tenant & Access → Consensus (Hard Quotas)

**Direction:** Quota definitions stored in quorum state. Validated during proposal processing.

**Contract:**
- Hard quotas (max_nodes, max_concurrent_allocations, sensitive_pool_size) are Raft-committed state.
- Changes: admin API call → Raft commit → immediate effect.
- Waldur can push quota updates via API → same Raft commit path.

**Failure modes:**
- Quota update fails (quorum unavailable) → old quotas remain in effect.
- Quota reduction below current usage → new proposals blocked, running allocations unaffected.

---

### IP-08: Tenant & Access → Scheduling (Soft Quotas & RBAC)

**Direction:** Scheduler reads soft quota state for cost function scoring. API layer enforces RBAC.

**Contract:**
- Soft quotas (gpu_hours_budget, fair_share_target, burst_allowance) propagated to schedulers eventually.
- RBAC: API middleware validates role before passing requests to scheduling.
- Consistency: Soft quotas may lag by scheduling cycle (~30s). Self-correcting.

**Failure modes:**
- Stale soft quota → brief overshoot, corrected next cycle.
- RBAC misconfiguration → user gets more/less access than intended. Configuration error, not system failure.

---

### IP-09: Federation → Scheduling (Cross-Site Allocation)

**Direction:** Federation broker injects remote allocation requests into local scheduling plane.

**Contract:**
- Input: Signed request from remote site (Sovra token verified)
- Validation: OPA policy check (user authorized, resources available, data sovereignty)
- Output: Allocation enters local vCluster scheduler queue (treated as local allocation from that point)
- Remote site can query status via federation catalog

**Failure modes:**
- Remote site unreachable → retry 3x with backoff → fail with explanation. Fall back to local if --site=auto.
- Sovra token invalid → reject immediately (403).
- Remote request during quorum election → 503 with Retry-After header.

**Ordering:** Remote requests are serialized through the local quorum like any other proposal.

**Duplication:** Request ID provides idempotency. Duplicate remote requests (retry) are deduplicated.

---

### IP-10: Scheduling → Accounting (Waldur Events)

**Direction:** Scheduling pushes usage events to Waldur asynchronously.

**Contract:**
- Events: allocation.started, allocation.completed, allocation.checkpointed, node.claimed, node.released
- Delivery: async push at configurable interval (default 60s). At-least-once (buffer + replay).
- Buffer: in-memory (10K events) → disk WAL (100K events) → drop with counter metric.

**Failure modes:**
- Waldur unavailable → events buffered. Replayed on reconnection.
- Buffer overflow → events dropped. Reconstructable from quorum logs via `lattice admin accounting reconcile`.
- Waldur slow → batch size increases naturally (more events per push).

**Guarantee:** Accounting never blocks scheduling (INV-N5).

---

### IP-10a: Scheduling ← Consensus (Internal Budget Ledger)

**Direction:** Scheduler reads allocation history from quorum state to compute GPU-hours usage per tenant.

**Contract:**
- Input: All allocations with `started_at` set (Running + terminal states) within the budget period
- Computation: `Σ (end_time - started_at).hours × assigned_nodes.len() × gpu_count_per_node`
  - For running allocations: `end_time = now`
  - For completed/failed/cancelled: `end_time = completed_at`
- Output: `BudgetUtilization { fraction_used }` per tenant, fed into cost function
- Period: configurable `budget_period_days` (default 90), rolling window

**Relationship to Waldur (IP-10):**
- Without Waldur: internal ledger is the sole source of budget utilization
- With Waldur available: Waldur's `remaining_budget()` takes precedence
- With Waldur unavailable (transient): internal ledger used as fallback

**Failure modes:**
- Quorum unavailable → scheduler cannot read allocations → no budget data → penalty defaults to 0.0 (no penalty). Acceptable because scheduling also pauses when quorum is down.
- Clock skew between scheduler replicas → minor GPU-hours discrepancy (~seconds). Acceptable.

**Guarantee:** Budget computation is read-only on Raft-committed state. No new writes. No external dependencies.

## hpc-core Integration Points

### IP-11: Node Management ↔ PACT (Namespace Handoff via hpc-node)

**Direction:** Lattice-node-agent requests; PACT-agent provides. Unidirectional.

**Contract (PACT-managed mode):**
- Input: `NamespaceRequest` via unix socket (`/run/pact/handoff.sock`)
  - `allocation_id`: unique allocation identifier
  - `namespaces`: requested types (Pid, Net, Mount)
  - `uenv_image`: optional SquashFS image to mount inside mount namespace
- Output: `NamespaceResponse` with FD types and uenv mount path. Actual FDs passed via SCM_RIGHTS ancillary data.
- Cleanup: `AllocationEnded` message sent when allocation completes → PACT cleans up namespaces and decrements mount refcount.

**Contract (standalone mode):**
- Same `NamespaceConsumer` trait, different implementation
- Creates namespaces via `unshare(2)` directly
- Mounts uenv without refcounting (one mount per allocation)

**Software Delivery Cross-Context:**
- **API Server → Registry**: Resolves ImageRef at submit time (sha256 pin). Registry can be ORAS/S3 (uenv) or OCI (containers). If registry unavailable and `resolve_on_schedule: false`, submit fails.
- **Scheduler → Data Stager**: Image staging requests generated alongside data mount requests. Images treated as staging targets with priority from `preemption_class`. Single pull to shared store (VAST/Parallax), not per-node.
- **Node Agent → Shared Store**: Reads images from VAST/NFS (uenv) or Parallax (OCI). Falls back to direct registry pull if shared store unavailable.
- **Node Agent → Podman**: For container workloads, starts Podman in detached mode, joins namespaces via `setns()`. Workload is agent-parented, not Podman-parented.
- **API Server → Metadata Service** (future): Extracts `env.json` from uenv images for view validation. Currently resolved by reading from the mounted image on the API server or cached alongside the registry.
- No socket communication

**Failure modes:**
- Socket unavailable → 1s timeout → fallback to standalone mode (INV-RI2). Audit event emitted.
- PACT crash mid-handoff → partial FDs received → abort, fallback to standalone for this allocation.
- Namespace FD invalid → `setns()` fails → allocation retried on different node.

**Ordering:** Each handoff is independent. No ordering constraints between allocations.

**Duplication:** Idempotent. Duplicate `NamespaceRequest` for same allocation_id returns cached response.

---

### IP-12: Consensus → External (Audit Event Forwarding via hpc-audit)

**Direction:** Lattice quorum emits standardized `AuditEvent` values. External SIEM forwarder consumes.

**Contract:**
- Lattice's `AuditSink` implementation wraps `AuditEvent` in ed25519-signed envelope → proposes to Raft
- SIEM forwarder subscribes to committed audit entries (same mechanism as existing audit log streaming)
- `AuditSource` field distinguishes origin: `LatticeNodeAgent`, `LatticeQuorum`, `LatticeCli`
- When PACT co-deployed: single SIEM forwarder can consume from both pact-journal AND lattice-quorum streams using the same `AuditEvent` schema

**AuditEntry wrapping model:**

Lattice's `AuditEntry` struct (ed25519-signed, hash-chained) wraps `hpc_audit::AuditEvent` directly. The old `AuditAction` enum and flat `user`/`action`/`details` fields are removed (clean break — lattice is not yet deployed):

```
AuditEntry {
    event: hpc_audit::AuditEvent,    // standardized payload (who, what, when, where, outcome)
    previous_hash: String,            // SHA-256 chain
    signature: String,                // ed25519 signature over (event + previous_hash)
}
```

The `event.principal.identity` replaces the old `user: UserId` field. The `event.action` (string) replaces the old `AuditAction` enum. The `event.scope` provides structured node/vCluster/allocation context (replaces embedding these in `details`).

**Cross-system admin/user audit correlation:**

PACT handles admin operations (node provisioning, emergency freeze, config drift remediation). Lattice handles user operations (allocation lifecycle, scheduling, data access). When admin actions interfere with user requests (e.g., PACT drains a node with running allocations), both systems emit `AuditEvent` with the same `node_id` in `AuditScope`. SIEM correlation on `(node_id, timestamp)` surfaces the admin cause alongside the user-visible effect.

**Future feature:** Causal references between admin and user audit events. When lattice detects a disruption caused by an external admin action, the audit event could carry `metadata: {"caused_by": "pact", "pact_event_id": "..."}` for explicit causal linking rather than temporal correlation. Requires a notification channel from PACT to Lattice (not yet designed).

**Lattice-specific action constants** (defined in `lattice-common`, using `lattice.` prefix):

```
// Node ownership (sensitive audit path)
pub const NODE_CLAIM: &str = "lattice.node.claim";
pub const NODE_RELEASE: &str = "lattice.node.release";

// Allocation lifecycle (extends hpc-audit workload actions)
pub const ALLOCATION_COMPLETE: &str = "lattice.allocation.complete";
pub const ALLOCATION_FAILED: &str = "lattice.allocation.failed";
pub const ALLOCATION_CANCELLED: &str = "lattice.allocation.cancelled";
pub const ALLOCATION_REQUEUED: &str = "lattice.allocation.requeued";
pub const ALLOCATION_SUSPENDED: &str = "lattice.allocation.suspended";

// Sensitive workload operations
pub const DATA_ACCESS: &str = "lattice.data.access";
pub const ATTACH_SESSION: &str = "lattice.attach.session";
pub const LOG_ACCESS: &str = "lattice.log.access";
pub const METRICS_QUERY: &str = "lattice.metrics.query";

// Secure wipe
pub const WIPE_STARTED: &str = "lattice.wipe.started";
pub const WIPE_COMPLETED: &str = "lattice.wipe.completed";
pub const WIPE_FAILED: &str = "lattice.wipe.failed";

// Scheduling
pub const PROPOSAL_COMMITTED: &str = "lattice.scheduling.proposal_committed";
pub const PROPOSAL_REJECTED: &str = "lattice.scheduling.proposal_rejected";
pub const PREEMPTION_INITIATED: &str = "lattice.scheduling.preemption_initiated";

// Quota
pub const QUOTA_EXCEEDED: &str = "lattice.quota.exceeded";
pub const QUOTA_UPDATED: &str = "lattice.quota.updated";

// DAG
pub const DAG_SUBMITTED: &str = "lattice.dag.submitted";
pub const DAG_COMPLETED: &str = "lattice.dag.completed";

// VNI / Network domain
pub const VNI_ASSIGNED: &str = "lattice.network.vni_assigned";
pub const VNI_RELEASED: &str = "lattice.network.vni_released";
```

These constants supplement (not replace) hpc-audit's well-known actions. Shared actions (`cgroup.create`, `namespace.handoff`, `mount.acquire`, etc.) use hpc-audit constants directly.

**AuditPrincipal mapping:**
- `AuditEntry.user` (UserId string) → `AuditPrincipal { identity: user_id, principal_type: Human, role: rbac_role }`
- System operations → `AuditPrincipal::system("lattice-quorum")` or `AuditPrincipal::system("lattice-node-agent")`

**Failure modes:**
- SIEM forwarder down → events accumulate in Raft log (always persisted). No loss.
- Schema mismatch between hpc-audit versions → forwarder logs parse error, buffers raw events.

---

### IP-13: Node Management → Identity (mTLS via hpc-identity)

**Direction:** Lattice-node-agent and lattice-quorum obtain workload identity from cascade.

**Contract:**
- `IdentityCascade` tried at startup and on rotation schedule (2/3 certificate lifetime)
- Provider priority: SPIRE agent socket → self-signed CA endpoint (quorum/journal) → static bootstrap cert
- `WorkloadIdentity` result used to configure tonic TLS
- `CertRotator` performs dual-channel swap for zero-downtime rotation

**Failure modes:**
- All providers fail → FM-20 (identity cascade exhaustion). Agent cannot establish new connections.
- SPIRE unavailable but self-signed works → transparent degradation. Log warning + audit event.
- Rotation failure → FM-23. Active cert unchanged, retry scheduled.

**Ordering:** Identity must be established before any gRPC communication (heartbeats, registration, proposals).

---

### IP-14: CLI → AuthClient (Token Management via hpc-auth)

**Direction:** Lattice CLI uses `AuthClient` for user authentication against the institutional IdP.

**Contract:**
- CLI calls `AuthClient::login()` → cascading flow selection → token stored in `~/.config/lattice/tokens.json`
- CLI calls `AuthClient::get_token()` before every authenticated RPC → returns cached or refreshed token
- CLI calls `AuthClient::logout()` → local cache cleared, IdP revocation attempted (best-effort)
- Server discovery: CLI queries `lattice-api /api/v1/auth/discovery` (unauthenticated) for IdP URL and client ID

**Failure modes:**
- IdP unreachable → `AuthError::IdpUnreachable`. Login fails. User informed.
- Token expired + refresh expired → `AuthError::TokenExpired`. User must re-login.
- Cache corrupted → `AuthError::CacheCorrupted`. Lenient mode: warn, delete, require re-login.
- No browser (SSH session) → Device Code or Manual Paste flow selected automatically.

**Ordering:** Login must precede all authenticated commands (INV-A1).

### IP-15: SecretResolver → All Server Components (Startup Secret Injection)

**Direction:** SecretResolver provides resolved secrets to lattice-api and lattice-quorum at startup.

**Contract:**
- `SecretResolver` constructed early in `main()`, before any server or client
- When Vault configured: authenticates via AppRole, fetches all secrets from KV v2 using convention-based paths (INV-SEC5)
- When Vault not configured: resolves from env vars (`LATTICE_{SECTION}_{FIELD}`), then config literals
- Returns `ResolvedSecrets` struct consumed by component initialization code
- Binary secrets (audit signing key) are base64-decoded after fetch

**Consumers:**
| Component | Secrets consumed | Used for |
|---|---|---|
| lattice-api | vast_username, vast_password, waldur_token, sovra key | External client construction (VAST, Waldur, Sovra) |
| lattice-quorum | audit_signing_key | Ed25519 signing of audit entries |

**Relationship to hpc-identity (IP-13):**
- SecretResolver handles API credentials and signing keys
- hpc-identity handles TLS certificates and private keys
- No overlap. SPIRE may use Vault PKI as upstream CA (A-V10), but that is SPIRE's concern, not Lattice's

**Failure modes:**
- Vault unreachable → FM-25. Component does not start.
- Vault key missing → FM-26. Component does not start.
- Env var unset + config empty → fatal error naming the field.
- All failures are startup-only. No runtime Vault dependency.

**Ordering:** SecretResolver must complete before any gRPC server starts, any external client is constructed, or any audit entry is signed. Sequenced before hpc-identity cascade (identity cascade may depend on resolved config, e.g., quorum CA endpoint address).

**Duplication:** Resolution is idempotent. Same input → same output. No caching concerns (startup-only).

---

## Cross-Context Race Conditions

### Race: Concurrent vCluster Proposals for Same Nodes

**Scenario:** HPC scheduler and Service scheduler both propose allocations that need the same nodes.
**Resolution:** Raft serializes proposals. First committed wins. Second rejected with OwnershipConflict. Losing scheduler retries next cycle with updated state.
**Impact:** One scheduling cycle delay for the loser. No data loss, no inconsistency.

### Race: Preemption vs. Natural Completion

**Scenario:** Checkpoint hint sent to allocation that is about to complete naturally.
**Resolution:** If allocation completes before checkpoint starts: completion takes priority, nodes released normally. Checkpoint hint becomes no-op.
**Impact:** None. Checkpointing is advisory.

### Race: Quota Reduction vs. In-Flight Proposal

**Scenario:** Waldur reduces max_nodes. Simultaneously, a scheduler proposes an allocation that would now exceed the new limit.
**Resolution:** Quota change is Raft-committed. If quota commit precedes proposal: rejected. If proposal precedes quota change: committed (both valid at their point in time).
**Impact:** Brief window where allocation committed under old quota. No violation — quota change is prospective, not retroactive.

### Race: VNI Exhaustion During DAG Execution

**Scenario:** DAG has sequential stages needing new Network Domains. VNI pool exhausted between stages.
**Resolution:** Pending allocation enters Pending with reason `vni_pool_exhausted`. DAG stalls at this allocation. Already-running stages unaffected.
**Impact:** DAG stalls. Recoverable when VNIs freed by other allocations.

### Race: Walltime Expiry vs. In-Progress Checkpoint

**Scenario:** Walltime timer fires while checkpoint is writing.
**Resolution:** Walltime takes priority (INV-E4). If checkpoint completes within SIGTERM grace period (30s): usable. Otherwise: discarded, allocation Failed.
**Impact:** Potential loss of checkpoint. Tracked by `lattice_checkpoint_walltime_conflict_total` counter.

### Race: Late Completion Report vs. Dispatch Rollback (INV-D6)

**Scenario:** Dispatcher decides a Dispatch Failure has occurred (retry budget exhausted) and is about to submit `RollbackDispatch` at observed `state_version = 5`. Simultaneously, a delayed heartbeat arrives carrying a successful Completion Report for the same allocation; the quorum applies it, advancing `state_version` to 6.
**Resolution:** `RollbackDispatch` carries the observed `state_version`. The quorum rejects any proposal whose observed version is stale. Completion Report wins; the allocation reaches its terminal state normally; node ownership is released via the normal completion path.
**Impact:** None. Retry budget was consumed but the allocation succeeded. No user-visible failure. Counter `lattice_dispatch_rollback_raced_by_completion_total` increments for observability.

### Race: Completion Report vs. StopAllocation (user cancel during exit)

**Scenario:** User calls cancel on an allocation that is in the middle of exiting normally. The server issues `StopAllocation` to the agent; the agent has already observed the process exit and queued a Completion Report.
**Resolution:** Agent's `StopAllocation` handler is idempotent — if the allocation is already in a terminal phase or being epilogued, it acknowledges without sending additional signals. The first Completion Report to reach the quorum wins: either `Completed` (natural exit beat cancel) or `Cancelled` (cancel landed first). INV-D7 prevents a later `Completed` from overriding `Cancelled`.
**Impact:** User may see `Completed` instead of `Cancelled`, or vice versa, depending on timing. Both are legitimate outcomes; documented in CLI help text.

## Dispatch Cross-Context Contracts

Introduced 2026-04-16 after the OV suite exposed that the dispatch bridge was not implemented. These contracts complement IP-02 (dispatch itself) and IP-03 (completion reporting) with the supporting flows.

---

### IP-13: Node Management → Consensus (Node Registration with Agent Address)

**Direction:** Node agent registers its Node record with the quorum at startup and on address change.

**Contract:**
- Input: `RegisterNodeRequest { node_id, capabilities, agent_address, conformance_fingerprint }`. `agent_address` is a required non-empty host:port. Validation rejects empty, syntactically malformed, or construction-unreachable addresses (`0.0.0.0:0`, `0.0.0.0:<port>`, `localhost:0`).
- Raft-committed: Yes — the Node record is strong-consistency state (enforces INV-D1).
- Re-registration: Idempotent on `node_id`. A repeat `RegisterNode` with the same `agent_address` is a no-op-but-ack; a repeat with a new `agent_address` atomically updates the record.
- Separate command: `UpdateNodeAddress { node_id, new_address }` for address-only changes without full re-registration.

**Failure modes:**
- Agent crash between `RegisterNode` commit and binding its gRPC listener → quorum has a stale address. The first `RunAllocation` attempt fails with connection-refused; absorbed by IP-02 retry/rollback.
- Concurrent `RegisterNode` from two agents claiming the same `node_id` → Raft serializes. Second is rejected with `NodeIdOwnedByDifferentAgent`. This is a misconfiguration, not a failure mode — flagged as an error, no retry.
- Agent repeatedly re-registers on different ports (flapping) → quorum rate-limits `UpdateNodeAddress` commits per `node_id` to one per 5s; excess are throttled.

**Ordering:** `RegisterNode` must commit before the node is eligible for placement (INV-D1). The scheduler is free to place on the new address once the commit is visible to its reader.

**Duplication:** Deduplicated by `(node_id, agent_address)` tuple; repeat registration with identical fields is a no-op-ack.

---

### IP-14: Scheduling ↔ Consensus (Rollback Dispatch)

**Direction:** Dispatcher (Node Management) submits a rollback proposal to Consensus when a Dispatch Failure occurs.

**Contract:**
- Input: `RollbackDispatch { allocation_id, observed_state_version, reason: String }`
- Raft-committed atomic operation (enforces INV-D6): in a single log entry, the quorum (a) transitions the allocation's state from `Running` to `Pending`, (b) increments `dispatch_retry_count`, (c) releases the allocation's node-ownership record.
- Optimistic concurrency: the proposal includes `observed_state_version`. If the allocation's current `state_version` differs (because a Completion Report or another proposal landed first), the quorum rejects the proposal with `StateVersionMismatch`.
- Retry budget cap (allocation-level only): if `allocation.dispatch_retry_count >= max_dispatch_retries` (default 3), the allocation transitions to `Failed` with reason `dispatch_exhausted` instead of `Pending`. This cap answers "is this workload broken?" — e.g., a bad image digest or bad uenv view that no node can start. Node health is tracked separately by INV-D11 / `node.consecutive_dispatch_failures`; a broken node gets marked Degraded independently of any one allocation's retry count.

**Failure modes:**
- Version mismatch → Dispatcher discards the rollback and moves on. Either completion succeeded (race in IP-D6 above) or another dispatcher instance already rolled back.
- Quorum unavailable → rollback proposal queued and retried on next dispatcher tick. The allocation stays Running-without-process until the proposal commits, which is bounded by INV-D8 silent-node sweep.
- Allocation already in terminal state at rollback time (edge case: completion + rollback racing both lose the version check) → rollback rejected, no-op.

**Ordering:** Rollback proposals for different allocations are independent. Rollback for the same allocation across multiple dispatcher instances or retries: serialized by Raft, first wins, others see version mismatch.

**Duplication:** Inherent in the retry model. Version check makes duplicates harmless.

---

### IP-15: Scheduling → Consensus (Silent Node Reconciliation)

**Direction:** Scheduler's reconciliation pass detects allocations Running on nodes that have gone silent, and proposes terminal-state transitions to Consensus.

**Contract:**
- Input (Raft-observed): allocations with `state == Running` whose assigned nodes have not delivered a heartbeat within `heartbeat_interval + grace_period` (INV-D8).
- Output: Raft proposal to transition the allocation to `Failed` with reason `node_silent`, or `Pending` with `requeue_count++` if RequeuePolicy permits (OnNodeFailure, Always).
- Enforcement: Runs on every scheduler cycle. At most one reconciliation attempt per (allocation, cycle).

**Failure modes:**
- Node resumes heartbeating during reconciliation → the heartbeat's Completion Reports may land first. Version-check race identical to IP-14: whichever lands first wins.
- Heartbeat and silent-sweep commit simultaneously at different Raft terms → Raft serializes; later becomes a no-op by INV-D4 (idempotency) or INV-D7 (phase monotonicity).
- Node comes back with a different `agent_address` and recovered allocations — handled in IP-13 and IP-02 respectively; reconciliation is only responsible for the "allocation is stuck" case.

**Ordering:** Reconciliation runs after normal placement within a scheduler cycle, so that a newly-placed allocation isn't immediately silent-swept.

**Duplication:** The reconciliation proposal is idempotent per (allocation_id, state_version). Multiple reconciliation attempts across cycles see monotonically-advancing state_version and only one commits.

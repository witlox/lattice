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

### IP-02: Consensus → Node Management (Assignment Notification)

**Direction:** Consensus notifies node agents of committed allocations.

**Contract:**
- Input: Committed allocation assignment (allocation spec, assigned nodes)
- Action: Node agent begins prologue (uenv pull, mount, data stage, scratch setup)
- Guarantee: Notification only after Raft commit (INV-O1)
- Node agent acknowledges receipt (gRPC response)

**Failure modes:**
- Node agent unreachable → quorum retries notification. If node becomes Down during retry, allocation requeued.
- Notification delivered but prologue fails → node agent reports failure to quorum → allocation retried on different nodes (max 3 retries).
- Duplicate notification → node agent idempotent (checks if allocation already running).

---

### IP-03: Node Management → Consensus (Heartbeat / State Report)

**Direction:** Node agents report to quorum.

**Contract:**
- Input: Heartbeat (health, conformance fingerprint, running allocation count, sequence number)
- Processing: Quorum updates in-memory capacity state (eventually consistent). NOT Raft-committed.
- Node ownership changes (Degraded, Down) triggered by missed heartbeats ARE Raft-committed.

**Failure modes:**
- Heartbeat missed → Degraded (after heartbeat_timeout). Down (after grace_period).
- Heartbeat storm → quorum-side rate limiting (max 1 per interval per node).
- Stale heartbeat (replay) → sequence number check rejects.

**Ordering:** Heartbeats are idempotent. Out-of-order heartbeats: latest sequence number wins.

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

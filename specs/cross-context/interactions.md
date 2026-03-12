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

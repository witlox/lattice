# Adversarial Architecture Review — Findings Report

**Date:** 2026-03-12
**Mode:** Architecture review (pre-implementation verification)
**Scope:** All specs/ and architecture/ artifacts cross-referenced against codebase

**Summary:** 5 Critical, 4 High, 5 Medium, 3 Low

---

## Critical Findings

### Finding ADV-01: GPU Memory Clear Not Implemented (INV-S6)

**Severity:** Critical
**Category:** Specification Compliance
**Location:** `crates/lattice-node-agent/src/epilogue.rs:65-81`
**Spec Reference:** INV-S6 (Sensitive Wipe Before Reuse), ESC-001

**Description:**
The `SensitiveWiper` trait is defined but the only implementation is `NoopSensitiveWiper`, which does nothing. No `nvidia-smi --gpu-reset` or `rocm-smi --resetgpu` calls exist anywhere in the codebase. The epilogue calls `sensitive_wiper.wipe()` but the call is a no-op.

**Evidence:**
```rust
// epilogue.rs:74-81
pub struct NoopSensitiveWiper;
impl SensitiveWiper for NoopSensitiveWiper {
    async fn wipe(&self, _alloc_id: &AllocId) -> Result<()> { Ok(()) }
}
```

**Impact:** Sensitive workload data remains in GPU HBM after release. Next tenant can access previous tenant's GPU memory. Regulatory compliance violation.

**Suggested Resolution:** Implement `GpuSensitiveWiper` with feature-gated NVIDIA/AMD paths. Must call GPU reset, verify clear, and report to quorum as auditable event before OpenCHAMI wipe.

---

### Finding ADV-02: Audit-Action Atomicity Gap (INV-O3)

**Severity:** Critical
**Category:** Concurrency
**Location:** `crates/lattice-api/src/` (gRPC handlers)
**Spec Reference:** INV-O3 (Audit Before Sensitive Action), INV-S3

**Description:**
The audit-then-act pattern for sensitive operations is not implemented. API handlers do not commit audit entries before executing actions. `RecordAudit` is a separate Raft command with no transactional binding to the subsequent action. If the process crashes between audit commit and action execution, the audit log claims an event occurred that did not.

**Evidence:**
- No `RecordAudit` calls found preceding `ClaimNode` or sensitive attach operations in API handlers
- `RecordAudit` and action commands are independent Raft entries — no two-phase commit

**Impact:** Audit trail diverges from reality. Sensitive node may be marked as claimed in audit but never actually reserved. Regulatory audit inadmissible.

**Suggested Resolution:** Either (a) combine audit + action into a single Raft command, or (b) implement idempotency tokens with crash-recovery replay of incomplete operations.

---

### Finding ADV-03: Audit Entries Not Cryptographically Signed (INV-S4)

**Severity:** Critical
**Category:** Specification Compliance
**Location:** `crates/lattice-quorum/src/global_state.rs:94`, `crates/lattice-common/src/types.rs` (AuditEntry)
**Spec Reference:** INV-S4 (Sensitive Audit Immutability)

**Description:**
The spec states audit entries are "cryptographically signed and chained." The implementation is `self.audit_log.push(entry)` — a plain Vec append. `AuditEntry` has no signature field. No PKI, HMAC, or hash chain infrastructure exists. Raft provides ordering and replication but not external verifiability.

**Evidence:**
- Zero matches for "sign", "crypto", "HMAC", "hash_chain" in lattice-quorum
- AuditEntry struct contains: id, timestamp, user, action, details — no signature

**Impact:** An operator with database access could tamper with the audit log without detection. Regulatory auditors cannot independently verify integrity without Raft infrastructure access.

**Suggested Resolution:** Add `previous_hash` and `signature` fields to AuditEntry. Compute hash chain on append. Sign with site PKI key. This enables external verification without Raft access.

---

### Finding ADV-04: Scheduler Proposal Stale-Read Race

**Severity:** Critical
**Category:** Concurrency
**Location:** `crates/lattice-scheduler/src/loop_runner.rs:117-156`
**Spec Reference:** IP-01 (Scheduling → Consensus), cross-context race "Concurrent vCluster Proposals"

**Description:**
The scheduler reads quorum state, computes placements, then proposes — with no optimistic concurrency control. Between read and propose, another scheduler (or heartbeat, or quota change) may have changed state. Rejected proposals are silently skipped with no retry.

**Evidence:**
- `run_once()` reads state at line 118-120, proposes at line 147-155
- `propose()` in client.rs has no version check
- On rejection, line 149-150 logs error and continues — no backoff, no retry

**Impact:** Under concurrent vCluster scheduling, quota violations are possible when two schedulers both read "8 nodes remaining" and both propose 8-node allocations. First committed wins, second rejected — but if validation happens per-proposal rather than atomic sum, both could commit.

**Suggested Resolution:** Add state version to quorum reads. Scheduler includes version in proposal. Quorum rejects stale proposals. Scheduler retries with fresh state. Document this as architectural requirement.

---

### Finding ADV-05: Sensitive Attach Session Not Limited to One (INV-C2)

**Severity:** Critical
**Category:** Specification Compliance
**Location:** `crates/lattice-node-agent/src/attach.rs:121-186`
**Spec Reference:** INV-C2 (Sensitive Attach Cardinality)

**Description:**
The attach handler validates that the allocation is Running and the user owns it, but does NOT check for existing sessions on sensitive allocations. The test `multiple_sessions_on_same_allocation` (line 575-607) explicitly tests and allows concurrent sessions.

**Evidence:**
- No `sensitive` check in attach validation path
- Test at line 575 confirms multiple concurrent sessions are allowed
- No session counter or mutex per sensitive allocation

**Impact:** Multiple terminals open on a sensitive allocation. Second session could observe sensitive data from first. Violates INV-C2.

**Suggested Resolution:** Add `if allocation.sensitive && active_sessions(alloc_id) > 0 { reject }` check in attach handler.

---

## High Findings

### Finding ADV-06: Heartbeat vs ClaimNode Race

**Severity:** High
**Category:** Concurrency
**Location:** `crates/lattice-quorum/src/global_state.rs:265-270` (RecordHeartbeat), lines 213-254 (ClaimNode)
**Spec Reference:** IP-03 (Heartbeat/State Report)

**Description:**
`RecordHeartbeat` updates `node.last_heartbeat` without checking node ownership. `ClaimNode` updates ownership without invalidating pending heartbeats. A heartbeat from a pre-claim agent can be accepted after ownership changes, creating a stale health record for a sensitive-claimed node.

**Evidence:** RecordHeartbeat (lines 265-270) only mutates `last_heartbeat` — no owner validation.

**Suggested Resolution:** Add `owner_version` to Node. Heartbeat carries version; quorum rejects stale heartbeats.

---

### Finding ADV-07: DAG Controller Double-Enqueue Race

**Severity:** High
**Category:** Concurrency
**Location:** `crates/lattice-scheduler/src/dag_controller.rs:80-113`
**Spec Reference:** INV-E3 (Dependency Condition Satisfaction)

**Description:**
The DAG controller's `processed` set is updated AFTER the `unblock_allocation()` call. If two evaluation cycles overlap, both can find the same allocation unblocked and enqueue it twice.

**Evidence:** Line 105 marks processed after line 103 calls unblock. The `&mut self` prevents true concurrency within one controller, but async scheduling could interleave.

**Suggested Resolution:** Mark as processed BEFORE calling unblock, or use an atomic check-and-set.

---

### Finding ADV-08: VNI Pool Not Managed Authoritatively (INV-C3)

**Severity:** High
**Category:** Specification Compliance
**Location:** `crates/lattice-node-agent/src/network.rs`, `crates/lattice-common/src/types.rs:270-281`
**Spec Reference:** INV-C3 (VNI Uniqueness)

**Description:**
VNI assignment is not managed by an authoritative allocator. Node-agent `VniManager` only validates local non-collision. `NetworkDomain` in GlobalState has a `vni` field but no allocation logic. VNI values appear to be manually/externally assigned.

**Evidence:**
- No `allocate_vni()` or pool manager found in quorum crate
- Acceptance tests use hardcoded VNI values
- Config has `vni_pool_start`/`vni_pool_end` but no consumption code

**Impact:** Two network domains could be assigned the same VNI without detection, violating L3 isolation.

**Suggested Resolution:** Implement VNI pool allocator in GlobalState with Raft-committed allocation/release.

---

### Finding ADV-09: Network Domain Tenant Scoping Not Enforced

**Severity:** High
**Category:** Specification Compliance
**Location:** `crates/lattice-common/src/types.rs:270-281`
**Spec Reference:** INV-E5 (Network Domain Tenant Scoping)

**Description:**
`NetworkDomain` has `tenant` and `name` fields, but no code enforces that domain names are scoped to tenants. No domain creation/lookup logic found in the quorum. A cross-tenant domain collision is structurally possible.

**Evidence:** No `create_network_domain` or `allocate_network_domain` function found in quorum.

**Suggested Resolution:** Add `CreateNetworkDomain` command to quorum. Key domains by `(tenant, name)` tuple.

---

## Medium Findings

### Finding ADV-10: walltime=0 Accepted Without Validation

**Severity:** Medium
**Category:** Missing Negative Case
**Location:** `crates/lattice-api/src/convert.rs:355-359`
**Spec Reference:** INV-E4 (Walltime Supremacy)

**Description:**
Zero or negative walltime for bounded allocations is accepted without error. Undefined behavior in scheduler — "time remaining" for walltime=0 is nonsensical.

**Suggested Resolution:** Reject walltime ≤ 0 at API admission with `InvalidAllocation` error.

---

### Finding ADV-11: min_nodes=0 Silently Clamped

**Severity:** Medium
**Category:** Missing Negative Case
**Location:** `crates/lattice-api/src/convert.rs:299-306`
**Spec Reference:** Allocation submission validation

**Description:**
`min_nodes=0` is silently upgraded to 1 via `.max(1)`. No error returned. User's intent is altered without notification.

**Suggested Resolution:** Reject with `InvalidAllocation("min_nodes must be >= 1")`.

---

### Finding ADV-12: Preemption Class Not Bounded to 0-10

**Severity:** Medium
**Category:** Missing Negative Case
**Location:** `crates/lattice-api/src/convert.rs:373`
**Spec Reference:** INV-E1, ubiquitous language "Preemption Class — An integer 0-10"

**Description:**
Protobuf u32 cast to u8 with no range validation. Values 11-255 are accepted and treated as "sensitive" (never preempted). A user can bypass the Tenant-contract-based class assignment.

**Suggested Resolution:** Validate `0 <= preemption_class <= 10` at API admission. Enforce that class is derived from Tenant contract, not user-submitted.

---

### Finding ADV-13: EventBus Late Subscriber Misses State Changes

**Severity:** Medium
**Category:** Edge Case
**Location:** `crates/lattice-api/src/events.rs:87-108`
**Spec Reference:** Watch RPC

**Description:**
Broadcast channel (capacity 256) drops old events. A Watch subscriber joining after a state transition never sees it. No mechanism to emit current state on subscription.

**Suggested Resolution:** On subscribe, emit synthetic event with current allocation state before streaming new events.

---

### Finding ADV-14: Walltime vs Checkpoint Coordination Gap

**Severity:** Medium
**Category:** Concurrency
**Location:** `crates/lattice-scheduler/src/walltime.rs`, `crates/lattice-node-agent/src/checkpoint_handler.rs`
**Spec Reference:** INV-E4, cross-context scenario "Walltime expires during active checkpoint"

**Description:**
WalltimeEnforcer and checkpoint handler operate independently with no shared state machine. An allocation can receive SIGTERM (walltime) and CHECKPOINT_HINT simultaneously, leading to a partial checkpoint.

**Suggested Resolution:** Add allocation-level coordinator that serializes walltime and checkpoint decisions. When walltime fires during checkpoint, apply the 30s grace period (per spec) rather than sending conflicting signals.

---

## Low Findings

### Finding ADV-15: max_requeue Unbounded

**Severity:** Low
**Category:** Edge Case
**Location:** `crates/lattice-common/src/types.rs:42-43`
**Spec Reference:** FM-16 (max_requeue, default 3)

**Description:**
No validation caps max_requeue. A user could set u32::MAX. The requeue logic itself may not be fully implemented, but when it is, this would create an infinite retry loop bounded only by quota exhaustion.

**Suggested Resolution:** Validate max_requeue ≤ configurable limit (e.g., 100) at admission.

---

### Finding ADV-16: Raft Snapshot Size Growth (A-U4)

**Severity:** Low
**Category:** Edge Case
**Location:** `specs/architecture/data-models/global-state.md`
**Spec Reference:** A-U4 (Raft Snapshot Size Manageable)

**Description:**
The audit log (~1.3GB over 7 years) grows unbounded in GlobalState. Snapshot/restore time will degrade. No archival strategy exists. This is documented as an unknown assumption but has no architectural mitigation.

**Suggested Resolution:** Implement audit log compaction: archive entries older than N days to cold storage (S3), keep only hash chain tip in GlobalState for integrity verification.

---

### Finding ADV-17: Lossy Translation — Protobuf u32 to u8 Cast

**Severity:** Low
**Category:** Semantic Drift
**Location:** `crates/lattice-api/src/convert.rs:373`
**Spec Reference:** Ubiquitous language (preemption class 0-10)

**Description:**
`preemption_class` is u32 in protobuf, u8 in Rust types. Cast truncates silently. While the practical impact is covered by ADV-12's validation fix, this is a semantic mismatch between wire format and domain model.

**Suggested Resolution:** Change proto to uint32 with documented range 0-10, or use an enum.

---

## Attack Vectors Applied and Results

| Attack Vector | Findings | Notes |
|---|---|---|
| Specification Compliance | ADV-01, ADV-03, ADV-05, ADV-08, ADV-09 | 5 invariants not enforced by code |
| Implicit Coupling | None found | Module boundaries are clean |
| Semantic Drift | ADV-17 | Minor proto↔domain type mismatch |
| Missing Negative Cases | ADV-10, ADV-11, ADV-12, ADV-15 | Input validation gaps |
| Concurrency & Ordering | ADV-02, ADV-04, ADV-06, ADV-07, ADV-14 | 5 race conditions |
| Edge Cases & Boundaries | ADV-13, ADV-16 | EventBus and snapshot growth |
| Failure Cascades | None found | Failure isolation is well-designed |

---

## Highest-Risk Area

**Sensitive workload isolation** is the highest-risk area. Of 5 Critical findings, 3 directly affect sensitive workloads (ADV-01 GPU wipe, ADV-02 audit atomicity, ADV-05 attach cardinality). Combined, these mean the system's stated regulatory compliance model has significant implementation gaps.

## Phase Gate Recommendation

**Findings that MUST block next phase (contract-gen):**
- ADV-01 (GPU wipe) — needs at minimum an architectural decision on implementation approach
- ADV-02 (audit atomicity) — needs architectural pattern for two-phase audit+action
- ADV-03 (audit signing) — needs AuditEntry schema change
- ADV-05 (attach cardinality) — needs validation code
- ADV-08 (VNI pool) — needs GlobalState allocator design

**Findings that SHOULD be addressed but don't block:**
- ADV-04 (stale read) — Raft serialization prevents actual double-commit; it's a performance/fairness issue
- ADV-06, ADV-07, ADV-09 through ADV-17 — trackable, addressable during implementation

---

*Review complete. 17 findings: 5 critical, 4 high, 5 medium, 3 low.*

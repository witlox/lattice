# Global State Data Model (lattice-quorum)

The Raft-replicated state machine. This is the authoritative source for all strongly-consistent state.

## GlobalState Structure

```
GlobalState {
  allocations:     HashMap<AllocId, Allocation>,
  nodes:           HashMap<NodeId, Node>,
  tenants:         HashMap<TenantId, Tenant>,
  vclusters:       HashMap<VClusterId, VCluster>,
  topology:        TopologyModel,
  audit_log:       Vec<AuditEntry>,          // Append-only (INV-S4)
  network_domains: HashMap<String, NetworkDomain>,
}
```

## Consistency Domains

| Field | Consistency | Mutated By | Read By |
|---|---|---|---|
| `allocations` | Strong (Raft) | SubmitAllocation, UpdateAllocationState, AssignNodes | Scheduler (read), API (query) |
| `nodes` | Strong (ownership); Eventual (capacity) | RegisterNode, ClaimNode, ReleaseNode, UpdateNodeState | Scheduler (placement), API (list) |
| `tenants` | Strong (quotas) | CreateTenant, UpdateTenant | Scheduler (quota check), API (admin) |
| `vclusters` | Eventually consistent | CreateVCluster, UpdateVCluster | Scheduler (weights), API (admin) |
| `topology` | Eventually consistent | UpdateTopology | Scheduler (f4 scoring) |
| `audit_log` | Strong (Raft, append-only) | RecordAudit | API (audit query), Regulatory export |
| `network_domains` | Eventually consistent | Created on first use, released on teardown | Node agent (VNI config), API (diagnostics) |

## State Size Considerations

**Spec source:** A-U4 (Raft snapshot size manageable)

Estimated steady-state sizes for a 10,000-node deployment:
- `nodes`: 10,000 entries × ~1KB = ~10MB
- `allocations`: ~50,000 entries (active + recently completed) × ~2KB = ~100MB
- `tenants`: ~100 entries × ~500B = ~50KB
- `vclusters`: ~20 entries × ~500B = ~10KB
- `topology`: ~200 groups × ~50 nodes × ~100B = ~1MB
- `audit_log`: Growing. ~1000 entries/day × 365 days × 7 years × ~500B = ~1.3GB

**Risk:** Audit log growth over 7 years dominates snapshot size. Investigation needed (A-U4): archival strategy for cold audit entries while maintaining tamper-evidence chain.

## Snapshot and Recovery

```
Snapshot = JSON-serialized GlobalState
WAL = Raft log entries since last snapshot
Recovery = load snapshot + replay WAL

Snapshot trigger: every N committed entries (configurable, default 10,000)
```

## Command Processing Contract

Every `Command` applied to `GlobalState` must:
1. Validate preconditions (invariant checks)
2. Return `CommandResponse` (success with updated state, or rejection with reason)
3. Be deterministic — same command on same state produces same result on every replica
4. Be idempotent where noted (e.g., duplicate `RegisterNode` for same NodeId)

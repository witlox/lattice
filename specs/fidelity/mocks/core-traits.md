# Mock Fidelity: Core Trait Boundaries
Assessed: 2026-03-20

## Summary

| Trait | Mock | Rating | Impact |
|-------|------|--------|--------|
| StorageService | MockStorageService | FAITHFUL | Low — records calls, configurable readiness |
| InfrastructureService | MockInfrastructureService | FAITHFUL | Low — records calls, always healthy |
| AccountingService | MockAccountingService | FAITHFUL | Low — records calls, configurable budgets |
| NodeRegistry | MockNodeRegistry | PARTIAL | **High** — skips owner_version, sensitive_pool_size |
| AllocationStore | MockAllocationStore | PARTIAL | **High** — skips quota checks, transition validation, service registry |
| AuditLog | MockAuditLog | PARTIAL | Medium — no signatures/hashes, missing action filter |
| VClusterScheduler | (no mock, real impls used) | N/A | None — tested via real schedulers |
| CheckpointBroker | MockCheckpointBroker | PARTIAL | Low — always-ok initiate, no cost logic |
| SchedulerStateReader | MockReader | FAITHFUL | None — returns cloned collections |
| SchedulerCommandSink | MockSink + variants | FAITHFUL | None — records all calls, error injection |
| Runtime | MockRuntime | FAITHFUL | None — per-alloc state, lifecycle rules |
| HeartbeatSink | RecordingSink | FAITHFUL | None — stores all heartbeats |
| HealthObserver | StaticHealthObserver | FAITHFUL | None — returns fixed observations |
| DataStageExecutor | NoopDataStageExecutor | FAITHFUL | None — always succeeds |
| OidcValidator | StubOidcValidator | FAITHFUL | None — accepts valid-*, rejects expired |
| SovraClient | StubSovraClient | FAITHFUL | None — dummy tokens |
| AccountingClient | InMemoryAccountingClient | FAITHFUL | None — in-memory store |

## Critical Divergences

### MockNodeRegistry (PARTIAL)
- Does NOT enforce `sensitive_pool_size` limit on claims
- Does NOT increment `owner_version` on claim/release
- Does NOT reject heartbeats with stale owner_version
- **Impact**: Unit tests using this mock cannot catch ownership version race bugs

### MockAllocationStore (PARTIAL)
- Does NOT enforce hard quota (max_nodes, max_concurrent) on insert
- Does NOT validate state transitions via `can_transition_to()`
- Does NOT set timestamps (started_at, completed_at)
- Does NOT manage service registry on state changes
- **Impact**: Unit tests may pass with invalid quota or illegal state transitions

### MockAuditLog (PARTIAL)
- Does NOT compute `previous_hash` or signature fields
- Does NOT support `action` or `allocation` filters in query
- **Impact**: Tests cannot verify audit chain integrity or fine-grained queries

## Mitigation

Integration tests in `lattice-quorum/tests/contracts.rs` and `integration.rs` use the real `GlobalState` and `QuorumClient`, which do enforce all invariants. The mock divergences primarily affect unit tests in scheduler, API, and checkpoint crates.

## Recommendation

Consider a `ValidatingMockAllocationStore` that wraps MockAllocationStore with transition validation, or migrate critical tests to use the real QuorumClient with in-memory Raft.

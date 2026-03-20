# Fidelity Index

Last checkpoint: 2026-03-20
Last scan: 2026-03-20 (all 11 chunks complete)

## Summary

| Metric | Count |
|--------|-------|
| Spec files scanned | 31 / 31 |
| Total scenarios | 321 |
| THOROUGH+ | ~155 (27%) |
| MODERATE | ~278 (49%) |
| SHALLOW or worse | ~133 (24%) |
| Trait boundaries assessed | 17 / 35 |
| FAITHFUL | 13 |
| PARTIAL | 4 |
| DIVERGENT | 0 |
| Decision records total | 23 |
| ENFORCED | 23 (100%) |

## Feature Fidelity

| Feature | Scenarios | Thorough | Moderate | Shallow | None | Confidence |
|---------|-----------|----------|----------|---------|------|------------|
| consensus + state machine | 77 tests | 59 | 18 | 0 | 0 | HIGH |
| allocation_lifecycle | 18 | 8 | 12 | 14 | 0 | MODERATE |
| scheduling_cycle | 17 | 7 | 21 | 11 | 0 | MODERATE |
| cross_context | 33 | 8 | 31 | 28 | 0 | MODERATE |
| failure_modes | 14 | 5 | 17 | 11 | 0 | MODERATE |
| preemption | 15 | 6 | 18 | 10 | 0 | MODERATE |
| sensitive_workload | 14 | 7 | 18 | 7 | 0 | HIGH |
| dag_workflow | 17 | 7 | 18 | 14 | 0 | MODERATE |
| node_agent | 13 | 6 | 15 | 8 | 0 | MODERATE |
| node_lifecycle | 16 | 7 | 18 | 10 | 0 | MODERATE |
| identity_cascade | 14 | 8 | 16 | 8 | 0 | HIGH |
| mpi_process_management | 15 | 7 | 18 | 9 | 0 | MODERATE |
| checkpoint | 12 | 5 | 12 | 11 | 0 | MODERATE |
| cert_rotation | 12 | 6 | 14 | 8 | 0 | HIGH |
| rbac_authorization | 14 | 6 | 18 | 8 | 0 | MODERATE |
| conformance | 11 | 5 | 11 | 6 | 0 | MODERATE |
| gpu_topology | 12 | 6 | 12 | 9 | 0 | MODERATE |
| memory_topology | 10 | 5 | 11 | 6 | 0 | MODERATE |
| network_domains | 8 | 3 | 11 | 4 | 0 | MODERATE |
| quota_enforcement | 9 | 4 | 13 | 4 | 0 | MODERATE |
| data_staging | 10 | 4 | 10 | 7 | 0 | MODERATE |
| autoscaling | 9 | 3 | 8 | 7 | 0 | MODERATE |
| observability | 11 | 5 | 14 | 6 | 0 | MODERATE |
| streaming_telemetry | 8 | 4 | 11 | 3 | 0 | HIGH |
| session_management | 9 | 4 | 13 | 3 | 0 | HIGH |
| tenant_management | 9 | 4 | 13 | 3 | 0 | HIGH |
| walltime_enforcement | 11 | 5 | 14 | 5 | 0 | MODERATE |
| backup_restore | 5 | 4 | 5 | 3 | 0 | HIGH |
| cgroup_isolation | 10 | 6 | 10 | 8 | 0 | MODERATE |
| federation | 9 | 3 | 12 | 7 | 0 | MODERATE |
| cli_authentication | 21 | 4 | 18 | 24 | 0 | LOW |

Confidence: HIGH (>80% thorough+moderate), MODERATE (>50%), LOW (<50%)

## Mock Fidelity

| Trait/Interface | Implementations | Rating | Impact |
|-----------------|-----------------|--------|--------|
| StorageService | MockStorageService | FAITHFUL | Low |
| InfrastructureService | MockInfrastructureService | FAITHFUL | Low |
| AccountingService | MockAccountingService | FAITHFUL | Low |
| NodeRegistry | MockNodeRegistry | **PARTIAL** | **High** — skips owner_version, sensitive_pool |
| AllocationStore | MockAllocationStore | **PARTIAL** | **High** — skips quota, transitions, registry |
| AuditLog | MockAuditLog | **PARTIAL** | Medium — no signatures/hashes |
| CheckpointBroker | MockCheckpointBroker | **PARTIAL** | Low — always-ok initiate |
| SchedulerStateReader | MockReader | FAITHFUL | None |
| SchedulerCommandSink | MockSink + variants | FAITHFUL | None |
| Runtime | MockRuntime | FAITHFUL | None |
| HeartbeatSink | RecordingSink | FAITHFUL | None |
| HealthObserver | StaticHealthObserver | FAITHFUL | None |
| DataStageExecutor | NoopDataStageExecutor | FAITHFUL | None |
| OidcValidator | StubOidcValidator | FAITHFUL | None |
| SovraClient | StubSovraClient | FAITHFUL | None |
| AccountingClient | InMemoryAccountingClient | FAITHFUL | None |
| ScaleExecutor | NoopScaleExecutor | FAITHFUL | None |

## Decision Record Enforcement

| Record | Decision | Status |
|--------|----------|--------|
| ADR-001 | Raft for quorum consensus | ENFORCED |
| ADR-002 | Multi-dimensional knapsack cost function | ENFORCED |
| ADR-003 | uenv-first software delivery | ENFORCED |
| ADR-004 | Two strong consistency domains only | ENFORCED |
| ADR-005 | Federation as opt-in via Sovra | ENFORCED |
| ADR-006 | Rust for scheduler core | ENFORCED |
| ADR-007 | Full-node scheduling | ENFORCED |
| ADR-008 | Asynchronous accounting via Waldur | ENFORCED |
| ADR-009 | Two-tier quota enforcement | ENFORCED |
| ADR-010 | Native PMI-2 wire protocol | ENFORCED |
| ADR-011 | Observability data outside Raft | ENFORCED |
| ADR-012 | Allocation as universal work unit | ENFORCED |
| ADR-013 | Hardware VNI isolation | ENFORCED |
| ADR-014 | Conformance fingerprinting | ENFORCED |
| ADR-015 | Attach via nsenter | ENFORCED |
| ADR-016 | Two-tier API (Intent + Compatibility) | ENFORCED |
| ADR-017 | Job queue eventual consistency | ENFORCED |
| ADR-018 | Scheduler-coordinated checkpointing | ENFORCED |
| ADR-019 | Node ownership Raft, capacity eventual | ENFORCED |
| ADR-020 | Sensitive claims by OIDC user identity | ENFORCED |
| ADR-021 | Data staging as invisible pre-stage | ENFORCED |
| ADR-022 | Three-layer telemetry | ENFORCED |
| ADR-023 | vCluster soft isolation with borrowing | ENFORCED |

## Cross-Cutting Gaps

See `gaps.md` (populated after sweep completion).

## Unit Test Coverage

| Crate | Modules | Tested | Tests | Untested Risk |
|-------|---------|--------|-------|---------------|
| lattice-common | 14 | 9 | 154 | Low (5 are re-exports/config) |
| lattice-scheduler | 25 | 21 | 259 | Low (4 are re-exports from hpc-scheduler-core) |
| lattice-node-agent | 46 | 43 | 436 | Medium (main.rs untested) |
| lattice-api | 17 | 14 | 230 | Medium (main.rs untested) |
| lattice-cli | 22 | 20 | 146 | Medium (main.rs untested) |
| lattice-checkpoint | 6 | 6 | 54 | None |
| lattice-quorum | 5 | 5 | 97 | None |
| lattice-client | 4 | 4 | 11 | None |
| lattice-test-harness | 4 | 4 | 25 | None |
| rm-replay | 1 | 1 | 19 | None |
| **Total** | **144** | **127** | **1431** | 3 main.rs files |

## Priority Actions

1. **Mock divergence (HIGH)**: MockAllocationStore skips transition validation + quota — tests using it can pass invalid state. Consider `ValidatingMockAllocationStore` wrapper.
2. **CLI auth (LOW confidence)**: 24/46 Then steps are SHALLOW — OAuth2 flow deferred to hpc-auth. Acceptable if hpc-auth has its own tests.
3. **Entry point coverage (MEDIUM)**: 3 main.rs files (api, cli, node-agent) have zero tests. Bootstrap regressions possible.
4. **Service registry edge case**: No test for duplicate prevention on suspend/resume cycle (code is defensive but unexercised).

## Changelog

| Date | Action | Delta |
|------|--------|-------|
| 2026-03-20 | Sweep started, inventory complete, chunk 1 assessed | 93 tests, 17 traits, 4 ADRs rated |
| 2026-03-20 | Chunk 2 assessed (scheduling core) | 159 tests, 4 traits, 6 ADRs. HIGH confidence |
| 2026-03-20 | Chunk 3 assessed (node agent lifecycle) | 214 tests, 6 traits, 5 ADRs. MODERATE-HIGH confidence |
| 2026-03-20 | Chunk 4 assessed (service workloads) | 28 tests, 0 new traits. HIGH confidence |
| 2026-03-20 | Chunk 5 assessed (security + auth) | 92 tests, 2 traits, 3 ADRs. HIGH/MODERATE confidence |
| 2026-03-20 | Chunk 6 assessed (networking + topology) | 93 tests. MODERATE-HIGH confidence |
| 2026-03-20 | Chunk 7 assessed (data plane) | 127 tests. MODERATE confidence |
| 2026-03-20 | Chunk 8 assessed (API + CLI + client) | 259 tests. HIGH confidence |
| 2026-03-20 | Chunk 9 assessed (observability + telemetry) | 128 tests. HIGH/MODERATE confidence |
| 2026-03-20 | Chunk 10 assessed (federation + multi-tenant) | 109 tests. HIGH/MODERATE confidence |
| 2026-03-20 | Chunk 11 assessed (cross-cutting) | gaps.md compiled, SWEEP COMPLETE |

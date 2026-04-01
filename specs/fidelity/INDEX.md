# Fidelity Index

Last checkpoint: 2026-04-01
Last scan: 2026-04-01 (full re-sweep, all 11 chunks)
Previous checkpoint: 2026-03-20

## Summary

| Metric | Previous | Current | Delta |
|--------|----------|---------|-------|
| Feature files | 31 | 33 | +2 (software_delivery, budget_tracking) |
| BDD scenarios | 321 | 392 | +71 |
| Unit/integration tests | 1,431 | 1,553 | +122 |
| Acceptance steps passing | ~1,400 | 1,832 | +432 |
| Trait boundaries assessed | 17/35 | 17/35 | — |
| FAITHFUL mocks | 13 | 13 | — |
| PARTIAL mocks | 4 | 4 | — (same 4) |
| ADRs enforced | 23/23 | 23/23 | — |
| INV-SD invariants | 0 | 10 | +10 |

## Feature Fidelity

| Feature | Scenarios | Confidence | Change |
|---------|-----------|------------|--------|
| consensus + state machine | 77 tests + 4 unit | **HIGH** | +bootstrap, +ResolveImage cmd |
| scheduling_cycle | 59 functions | **HIGH** | +drain completion, +deferred resolution |
| node_agent | 461 tests | **HIGH** | +PodmanRuntime, +state recovery, +mTLS, +image stager |
| service_workloads | ~4 unit + BDD | **LOW** | unchanged — bin-pack, probes untested |
| security_auth | 187 tests | **HIGH** | +HMAC, +JWKS sync, +pact_role, +SyncOidcValidator |
| networking_topology | 125+ BDD | **MODERATE** | unchanged |
| data_plane | 52 tests | **HIGH** | +image staging in DataStager |
| api_cli_client | 266+ tests | **MODERATE** | +new CLI flags, +proto rewrite, +Python SDK |
| observability | 19 BDD + 1 unit | **MODERATE** | unchanged |
| federation_multitenant | 33 BDD + 28 unit | **MODERATE** | +budget tracking (15 scenarios), +soft quota fix |
| software_delivery | 43 BDD + 80 unit | **HIGH** | **NEW** — full feature |
| budget_tracking | 15 BDD + 18 unit | **HIGH** | **NEW** — full feature |
| cross_cutting | ADRs, gaps | **MODERATE** | +INV-SD1-10, 3 mock divergences remain |

## Mock Fidelity (unchanged)

| Trait | Mock | Rating | Impact |
|-------|------|--------|--------|
| AllocationStore | MockAllocationStore | **PARTIAL** | Skips transition validation, quota |
| NodeRegistry | MockNodeRegistry | **PARTIAL** | Skips owner_version, sensitive_pool |
| AuditLog | MockAuditLog | **PARTIAL** | No signatures/hashes |
| CheckpointBroker | MockCheckpointBroker | **PARTIAL** | Always-ok initiate |
| All others (13 traits) | Various | FAITHFUL | — |

## High-Priority Gaps

| # | Gap | Chunk | Severity | Status |
|---|-----|-------|----------|--------|
| G-NEW-1 | ResolveImage command has no quorum-level test | 1 | HIGH | NEW |
| G-NEW-2 | Drain completion tests referenced but may not exist in loop_runner | 2 | HIGH | VERIFY |
| G-NEW-3 | PodmanRuntime integration test assertions unclear | 3 | MEDIUM | VERIFY |
| G-NEW-4 | CLI flag parsing (--uenv, --image, --edf) no dedicated unit tests | 8 | MEDIUM | NEW |
| G-NEW-5 | REST drain endpoint allocation count untested | 8 | MEDIUM | NEW |
| G-NEW-6 | Python SDK AllocationSpec new fields untested | 8 | MEDIUM | NEW |
| G-NEW-7 | Input validation edge cases (ADV-10/11/12, F15-18) no unit tests | 5 | MEDIUM | NEW |
| G69 | Federation + quota race condition | 10 | HIGH | OPEN |
| G70 | DAG + quota race condition | 10 | HIGH | OPEN |
| G35 | RealDataStageExecutor zero tests | 7 | HIGH | OPEN |
| G12 | flush_buffered() stub — quorum reconnect replay | 3 | HIGH | OPEN |
| G61 | EventBus pub/sub not tested E2E | 9 | HIGH | OPEN |
| G71 | Soft quota f3 scoring not verified with cost function | 10 | MEDIUM | PARTIAL (budget steps exist) |

## Medium-Priority Gaps

| # | Gap | Chunk | Status |
|---|-----|-------|--------|
| G-NEW-8 | INV-SD4 deferred resolution timeout not directly tested | 2 | NEW |
| G-NEW-9 | INV-SD6 single pull for shared store not E2E tested | 7 | NEW |
| G-NEW-10 | Topology fitness scoring (f4) never validated against placement | 6 | OPEN |
| G-NEW-11 | Service bin-pack density scoring untested | 4 | OPEN |
| G-NEW-12 | Session tracking lifecycle untested at quorum level | 1 | OPEN |
| G-NEW-13 | fail_allocation TODO in scheduler loop_runner | 2 | NEW |

## Entry Point Coverage

| Binary | Lines | Tests | Risk |
|--------|-------|-------|------|
| lattice-api/src/main.rs | 528 | 0 | MEDIUM |
| lattice-cli/src/main.rs | 141 | 0 | MEDIUM |
| lattice-node-agent/src/main.rs | 397 | 0 | MEDIUM |

## ADR Enforcement

All 23 ADRs: **ENFORCED** (unchanged from previous checkpoint)

## TODOs in Codebase

| Location | Content | Severity |
|----------|---------|----------|
| lattice-api/src/mpi.rs:163 | `cxi_credentials: None, // TODO: integrate fabric manager` | LOW |
| lattice-scheduler/src/loop_runner.rs | `// TODO: fail_allocation method on sink` | MEDIUM |

## Changelog

| Date | Action |
|------|--------|
| 2026-03-20 | Initial sweep complete — 11/11 chunks, 1,431 tests |
| 2026-04-01 | Full re-sweep — 1,553 tests + 392 BDD scenarios, 13 new gaps identified |

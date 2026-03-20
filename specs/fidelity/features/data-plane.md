# Fidelity: Data Plane (Chunk 7)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| checkpoint/cost_model.rs | 9 | 9 | 0 | 0 | None — cost/value math solid |
| checkpoint/broker.rs | 12 | 0 | 12 | 0 | gRPC delivery to agents untested |
| checkpoint/loop_runner.rs | 10 | 0 | 10 | 0 | Real store integration untested |
| checkpoint/policy.rs | 9 | 0 | 9 | 0 | Protocol registry missing |
| checkpoint/protocol.rs | 4 | 0 | 0 | 4 | Only Signal protocol; no callback |
| pmi2/kvs.rs | 9 | 9 | 0 | 0 | Excellent barrier logic |
| pmi2/protocol.rs | 21 | 21 | 0 | 0 | Complete wire format coverage |
| pmi2/server.rs | 6 | 0 | 0 | 6 | Accept loop + rank handler untested |
| pmi2/fence.rs | 7 | 0 | 7 | 0 | Multi-node merging untested |
| scheduler/data_staging.rs | 18 | 18 | 0 | 0 | Staging plan logic solid |
| node-agent/data_stage.rs | 0 | 0 | 0 | 0 | **ZERO TESTS** — RealDataStageExecutor |
| node-agent/attach.rs | 14 | 0 | 14 | 0 | PTY integration untested |
| node-agent/pty.rs | 8 | 0 | 8 | 0 | Real nix PTY untested |
| **TOTAL** | **127** | **57** | **60** | **10** | |

## BDD Coverage

| Feature | Scenarios | Depth |
|---------|-----------|-------|
| data_staging | 9 | MODERATE |
| checkpoint | 12 | MODERATE |
| mpi_process_management | 10 | MODERATE |
| **Total** | **31** | |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G35 | RealDataStageExecutor has ZERO unit tests | High | node-agent/data_stage.rs |
| G36 | Checkpoint broker→agent gRPC delivery untested | Medium | checkpoint/broker.rs |
| G37 | PMI-2 server accept loop + rank handler untested | Medium | pmi2/server.rs |
| G38 | Multi-node fence coordination untested | Medium | pmi2/fence.rs |
| G39 | Checkpoint timeout extension logic missing | Medium | checkpoint feature |
| G40 | Data readiness threshold (0.95) not enforced in tests | Medium | data_stage.rs |
| G41 | DMTCP fallback protocol unimplemented | Low | checkpoint/protocol.rs |
| G42 | Attach session PTY integration stub only | Low | attach.rs |

## Confidence: MODERATE

Strong unit coverage in cost model, KVS, wire protocol, and staging plan logic. Weak in real data stage execution, checkpoint delivery, PMI-2 server lifecycle, and PTY integration. The math is right; the plumbing needs integration tests.

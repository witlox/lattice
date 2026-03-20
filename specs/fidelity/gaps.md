# Cross-Cutting Gaps
Compiled: 2026-03-20

## Dead Specs (no step definitions)
None found. All 31 feature files have corresponding step definition files.

## Orphan Tests (no spec)
None found. All unit/integration tests map to source modules or BDD features.

## Stale Specs
None critical. BDD scenario language matches current code naming (sensitive, not medical).

## Feature Flag Gaps
| Flag | Gated Code | Gated Tests | Gap |
|------|-----------|-------------|-----|
| `nvidia` | NvidiaDiscovery (nvml-wrapper) | Stub tests only | Real NVML not tested |
| `rocm` | AmdDiscovery (rocm-smi) | Stub tests only | Real rocm-smi not tested |
| `ebpf` | AyaEbpfCollector (aya) | Stub + binary layout tests | aya requires Linux |
| `federation` | HttpSovraClient | StubSovraClient in tests | No real Sovra integration |
| `accounting` | HttpWaldurClient | InMemoryAccountingClient | No real Waldur integration |
| `oidc` | JwtOidcValidator | wiremock-based tests | Solid E2E with mock OIDC |
| `scheduler-core` | Job/ComputeNode trait impls | Integration tests exist | Tests in external crate |

## Untested Entry Points
| Binary | File | Lines | Risk |
|--------|------|-------|------|
| lattice-api server | crates/lattice-api/src/main.rs | ~250 | Medium — bootstrap wiring |
| lattice-cli | crates/lattice-cli/src/main.rs | ~100 | Medium — auth + dispatch |
| lattice-node-agent | crates/lattice-node-agent/src/main.rs | ~150 | Medium — registration wiring |

## High-Severity Gaps (sorted by impact)

| # | Gap | Chunk | Severity | Impact |
|---|-----|-------|----------|--------|
| G35 | RealDataStageExecutor has ZERO unit tests | 7 | High | Hot-tier staging, VAST API, QoS untested |
| G12 | flush_buffered() is a stub — quorum reconnect replay never exercised | 3 | High | State loss on quorum outage |
| G69 | Federation + quota race: offer accepted but quorum rejects | 10 | High | Inconsistent user experience |
| G70 | DAG + quota race: downstream proposed after quota reduced | 10 | High | Silent DAG failure |
| G71 | Soft quota scoring (f3) not verified with cost function | 10 | High | Fair-share may not work |
| G20 | No E2E mTLS handshake test with peer certificate | 5 | High | TLS config untested in practice |
| G21 | Image signature verification is stub-only | 5 | High | Sensitive workload security gap |
| G61 | EventBus pub/sub not tested end-to-end | 9 | High | Streaming features may not work |
| G13 | Real UenvRuntime/SarusRuntime have <10 tests | 3 | High | Production runtime untested |

## Medium-Severity Gaps (top 10)

| # | Gap | Chunk | Impact |
|---|-----|-------|--------|
| G5 | InfraScaleExecutor has no unit tests | 2 | Scale-up/down via OpenCHAMI untested |
| G6 | No preemption victim cost ordering unit test | 2 | Preemption may pick wrong victims |
| G14 | CgroupManager stubbed — real resource limits not verified | 3 | Container breakout risk |
| G15 | owner_version never set in heartbeat (ADR-06) | 3 | Stale heartbeats accepted |
| G29 | No full-chain acceptance test (probe fail → requeue) | 4 | Service reconciliation untested E2E |
| G36 | Checkpoint broker→agent gRPC delivery untested | 7 | Checkpoint signaling may fail |
| G50 | DAG rollback incomplete — partial insert orphans | 8 | Dangling allocations in store |
| G51 | TaskGroup step overflow (step=0 → infinite loop) | 8 | API crash on malformed input |
| G57 | Rate limiter counter race under concurrent bursts | 8 | Rate limit bypass under load |
| G73 | Borrowing return grace period untested | 10 | Nodes may not be returned |

## Mock Divergences (High Impact)

| Mock | Divergence | Impact |
|------|-----------|--------|
| MockAllocationStore | Skips quota, transitions, timestamps, service registry | Tests using it can pass invalid state |
| MockNodeRegistry | Skips owner_version, sensitive_pool_size | ADV-06 not verified in unit tests |
| MockAuditLog | No signatures/hashes, missing filters | Audit integrity untested at unit level |

## Priority Actions (ordered)

1. **Add RealDataStageExecutor unit tests** with mock StorageService (G35)
2. **Implement flush_buffered()** with real replay logic (G12)
3. **Add ValidatingMockAllocationStore** that enforces transitions + quota (mock divergence)
4. **Add mTLS handshake E2E test** (G20)
5. **Add soft quota scoring integration test** — verify f3 uses compute_tenant_usage() (G71)
6. **Add full-chain service lifecycle acceptance test** (probe → fail → requeue) (G29)
7. **Add federation+quota race scenario** (G69)
8. **Add TaskGroup step=0 validation** in allocation_from_proto (G51)
9. **Add InfraScaleExecutor unit tests** with mock InfraService (G5)
10. **Add entry point smoke tests** for 3 main.rs binaries

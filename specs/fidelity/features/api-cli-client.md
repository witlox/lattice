# Fidelity: API + CLI + Client (Chunk 8)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| REST (rest.rs) | 21 | 15 | 6 | 0 | Malformed JSON, SSE backpressure |
| gRPC AllocationService | 26 | 20 | 6 | 0 | DAG rollback, TaskGroup overflow |
| gRPC AdminService | 10 | 8 | 2 | 0 | Quota validation, scheduler_type enum |
| gRPC NodeService | 10 | 8 | 2 | 0 | EnableNode without alloc check |
| Proto convert.rs | 14 | 10 | 4 | 0 | NULL handling, liveness round-trip |
| Diagnostics | 4 | 2 | 2 | 0 | TSDB timeout, stale heartbeat |
| Events (EventBus) | 2 | 1 | 1 | 0 | Broadcast overflow, late subscriber |
| API integration.rs | 54 | 45 | 9 | 0 | Excellent cross-module coverage |
| CLI client.rs | 5 | 3 | 2 | 0 | Auth flow, endpoint parsing |
| CLI integration.rs | 15 | 12 | 3 | 0 | SBATCH, time, compat |
| CLI commands/*.rs | 0 | 0 | 0 | 0 | No inline tests (integration-only) |
| lattice-client | 1 | 1 | 0 | 0 | Only connect test; 42 RPCs untested |
| Python SDK | 97 | 80 | 17 | 0 | 45+ methods, httpx mock, error types |
| **TOTAL** | **259** | **205** | **54** | **0** | |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G50 | DAG rollback incomplete — partial insert leaves orphaned allocations | Medium | allocation_service.rs |
| G51 | TaskGroup step overflow (step=0 → infinite loop, i64 overflow) | Medium | allocation_service.rs |
| G52 | Proto NULL handling: Option fields silently default on None | Medium | convert.rs |
| G53 | No dependency cycle detection in allocation_from_proto | Medium | convert.rs |
| G54 | REST: no malformed JSON / bad UUID / missing Content-Type tests | Medium | rest.rs |
| G55 | SSE watch/stream: no backpressure, client disconnect, or cleanup tests | Medium | rest.rs |
| G56 | Attach RPC: no ownership validation (any user can attach) | Medium | allocation_service.rs |
| G57 | Rate limiter counter race under concurrent bursts | Medium | rate_limit.rs |
| G58 | lattice-client: 42 RPCs but only 1 test (connect error) | Low | client.rs |
| G59 | CLI command handlers have zero inline tests | Low | commands/*.rs |
| G60 | Python SDK: no httpx.HTTPError distinction (network vs API) | Low | sdk/python |

## Confidence: HIGH

205/259 tests (79%) are THOROUGH. API integration suite (54 tests) provides strong cross-module coverage. Proto conversions well-tested with ADV-10/11/12 validation. Python SDK has excellent httpx mock coverage. Main gaps are edge cases (DAG rollback, step overflow, NULL proto fields) and missing error-path tests.

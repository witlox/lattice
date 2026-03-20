# Fidelity: Observability + Telemetry (Chunk 9)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| log_buffer.rs | 11 | 11 | 0 | 0 | Ring buffer, wrap, S3 flush |
| ebpf_stubs.rs | 10 | 10 | 0 | 0 | Collector lifecycle FSM |
| proc_collector.rs | 32 | 32 | 0 | 0 | CPU/mem/net/disk/vmstat parsing |
| gpu_discovery.rs | 8 | 8 | 0 | 0 | ROCM parsing, PCIe fallback |
| memory_discovery.rs | 14 | 14 | 0 | 0 | NUMA, CXL, superchip detection |
| aya_collector.rs | 6 | 6 | 0 | 0 | Binary event layout validation |
| TelemetryCollector (mod.rs) | 7 | 7 | 0 | 0 | Mode switching, aggregation |
| metrics.rs | 8 | 0 | 8 | 0 | Global OnceLock not test-isolated |
| tsdb_client.rs | 14 | 14 | 0 | 0 | wiremock HTTP, JSON parsing |
| BDD: streaming_telemetry | 8 scen | 0 | 2 | 6 | EventBus not wired; steps shallow |
| BDD: observability | 10 scen | 2 | 4 | 4 | Attach PTY, diagnostics mocked |
| **TOTAL** | **128** | **104** | **14** | **10** | |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G61 | EventBus pub/sub not tested end-to-end | High | streaming telemetry BDD |
| G62 | Metrics resolution switching: feature exists, agent never uses it | High | observability BDD + node-agent |
| G63 | Audit entry creation not wired in attach handler | High | observability BDD |
| G64 | Metrics global OnceLock not test-isolated (parallel test risk) | Medium | metrics.rs |
| G65 | Attach RPC→PTY terminal binding not E2E tested | Medium | observability BDD |
| G66 | Cross-tenant metric comparison: no actual RPC exists | Medium | observability BDD |
| G67 | Diagnostics query returns hardcoded data in steps | Low | observability BDD |
| G68 | Superchip detection fragile (string contains vs pattern) | Low | memory_discovery.rs |

## Confidence: HIGH (unit) / MODERATE (integration)

Unit-level telemetry parsing and collection are excellently tested (104 THOROUGH). The weak spots are at the integration boundary: EventBus wiring, attach terminal binding, and metrics resolution switching are declared in BDD but not connected to real implementation.

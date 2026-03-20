# Fidelity: Networking + Topology (Chunk 6)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| VNI Manager (network.rs) | 17 | 17 | 0 | 0 | No concurrent access tests |
| Conformance fingerprint | 7 | 0 | 7 | 0 | No large dataset testing |
| GPU discovery | 7 | 0 | 7 | 0 | Feature flag stubs untested |
| Memory discovery | 9 | 0 | 9 | 0 | NUMA graph validity unchecked |
| Topology selection | 5 | 0 | 5 | 0 | Thin wrapper over hpc-core |
| Conformance groups | 16 | 16 | 0 | 0 | Solid constraint logic |
| BDD: network_domains | 8 scen | 0 | 8 | 0 | No concurrent VNI contention |
| BDD: gpu_topology | 8 scen | 0 | 8 | 0 | NVLink inference unvalidated |
| BDD: memory_topology | 8 scen | 0 | 8 | 0 | CXL topology assumptions |
| BDD: conformance | 8 scen | 8 | 0 | 0 | Drift + reimage + sensitive |
| **TOTAL** | **93** | **41** | **52** | **0** | |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G43 | Concurrent VNI release race conditions untested | Medium | network.rs |
| G44 | GPU discovery feature flag stubs (nvidia/rocm) untested | Medium | gpu_discovery.rs |
| G45 | NUMA interconnect graph not validated for correctness | Medium | memory_discovery.rs |
| G46 | Memory topology not tested in cost function f4 with real NUMA variance | Medium | cycle.rs / cost.rs |
| G47 | Combined GPU+memory+conformance constraints never tested together | Low | BDD |
| G48 | CXL-to-NUMA mapping assumes node 0 only | Low | memory_discovery.rs |
| G49 | NVLink/NVSwitch level inference unvalidated against hardware | Low | gpu_discovery.rs |

## Confidence: MODERATE-HIGH

VNI manager and conformance groups are thoroughly tested. GPU/memory discovery has solid happy-path coverage but lacks feature-flag-gated testing and real hardware validation. Topology selection delegates to proven hpc-scheduler-core.

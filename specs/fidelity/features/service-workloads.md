# Fidelity: Service Workloads (Chunk 4)
Assessed: 2026-03-20

## Test Inventory

| Component | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|-----------|-------|----------|----------|---------|----------|
| Liveness probe (liveness.rs) | 6 | 5 | 1 | 0 | No successful TCP/HTTP connect test |
| Probe manager (probe_manager.rs) | 4 | 3 | 1 | 0 | Period scheduling precision untested |
| Service bin-pack (service_binpack.rs) | 4 | 4 | 0 | 0 | Complete — group packing verified |
| Reconciler (loop_runner.rs) | 7 | 7 | 0 | 0 | Complete — all lifecycle+policy combos |
| Service registry (global_state.rs) | 7 | 7 | 0 | 0 | Complete — register/deregister/list |
| Proto conversion (convert.rs) | 0 | 0 | 0 | 0 | No dedicated round-trip test |
| REST service routes (rest.rs) | 0 | 0 | 0 | 0 | Routes declared, no integration test |
| gRPC service RPCs (admin_service.rs) | 0 | 0 | 0 | 0 | RPCs declared, no test |
| **TOTAL** | **28** | **26** | **2** | **0** | |

## End-to-End Service Lifecycle

| Scenario | Tested | Depth |
|----------|--------|-------|
| Service submission with expose endpoints | Yes | THOROUGH |
| Registration on Running | Yes | THOROUGH |
| Deregistration on completion | Yes | THOROUGH |
| Deregistration on requeue | Yes | THOROUGH |
| Multiple instances same service | Yes | THOROUGH |
| Liveness probe success recovery | Yes | THOROUGH |
| Liveness probe threshold trigger | Yes | THOROUGH |
| TCP probe (failure path) | Yes | MODERATE |
| HTTP probe (failure path) | Yes | MODERATE |
| Probe manager initial delay | Yes | THOROUGH |
| Bin-pack group concentration | Yes | THOROUGH |
| Bin-pack spill to next group | Yes | THOROUGH |
| Reconciler: all policy combos | Yes | THOROUGH |
| Reconciler: max_requeue boundary | Yes | THOROUGH |
| Full chain: probe fail → Failed → requeue | No | MISSING |
| REST/gRPC service discovery E2E | No | MISSING |
| Proto LivenessProbeSpec round-trip | No | MISSING |
| Reactive scaling with bin-pack | No | MISSING |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G29 | No full-chain acceptance test (probe fail → requeue) | Medium | Cross: liveness.rs + loop_runner.rs |
| G30 | No REST/gRPC integration test for service discovery | Medium | rest.rs, admin_service.rs |
| G31 | No proto LivenessProbeSpec round-trip unit test | Low | convert.rs |
| G32 | No successful TCP/HTTP probe test (only closed-port failures) | Low | liveness.rs |
| G33 | Probe period scheduling only tested with period=0 | Low | probe_manager.rs |
| G34 | Reactive min/max_nodes interaction with bin-pack untested | Low | service_binpack.rs |

## Confidence: HIGH

Core service components (reconciler, registry, probes, bin-pack) are thoroughly unit-tested. Gaps are at integration boundaries (REST/gRPC, proto conversion, full-chain E2E). No blocking issues.

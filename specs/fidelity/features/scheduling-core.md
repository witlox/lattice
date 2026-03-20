# Fidelity: Scheduling Core (Chunk 2)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| cost.rs (9 factors) | 47 | 45 | 2 | 0 | None — all f1-f9 + budget penalty |
| knapsack.rs (two-pass) | 15 | 15 | 0 | 0 | None — greedy, backfill, reservation |
| cycle.rs | 7 | 7 | 0 | 0 | None — empty to timeline-aware |
| autoscaler.rs | 21 | 21 | 0 | 0 | None — all paths + boundaries |
| scale_executor.rs | 1 | 0 | 0 | 1 | Only NoopScaleExecutor tested |
| loop_runner.rs (reconciler) | 17 | 12 | 5 | 0 | Reconciler unit tests exist; InfraScaleExecutor untested |
| HpcBackfill scheduler | 3 | 3 | 0 | 0 | None |
| ServiceBinPack scheduler | 7 | 7 | 0 | 0 | Density packing, groups |
| SensitiveReservation | 10 | 10 | 0 | 0 | Conformance strict |
| InteractiveFifo | 2 | 2 | 0 | 0 | FIFO order |
| preemption (delegated) | 0 | 0 | 0 | 0 | hpc-scheduler-core; BDD covers |
| integration.rs | 22 | 15 | 7 | 0 | Solid cross-module coverage |
| contracts.rs | 7 | 7 | 0 | 0 | Score invariants |
| **TOTAL** | **~159** | **~144** | **~14** | **~1** | |

## Cost Factor Coverage (9/9 = 100%)

| Factor | Name | Unit Test | Edge Case | Gap |
|--------|------|-----------|-----------|-----|
| f1 | priority_class | 3 tests | 0, 5, 10 boundary | None |
| f2 | wait_time_factor | 3 tests | zero wait, ln2, monotonic | None |
| f3 | fair_share_deficit | 8 tests | target, above, zero, burst | Tenant cascade mocked |
| f4 | topology_fitness | 3 tests | single/many nodes | memory_locality shallow |
| f5 | data_readiness | 2 tests | known, unknown default | No malformed input test |
| f6 | backlog_pressure | 3 tests | no backlog, full, overflow | None |
| f7 | energy_cost | 2 tests | cheap, expensive | Edge prices 0.0/1.0 untested |
| f8 | checkpoint_efficiency | 3 tests | None, Auto, Manual | None |
| f9 | conformance_fitness | implicit | In sensitive scheduler | No direct unit test |
| budget | penalty multiplier | 7 tests | 0-120%, non-linear | None |

## Algorithm Completeness

| Algorithm | Happy | Error | Edge | Gap |
|-----------|-------|-------|------|-----|
| Cost scoring | THOROUGH | N/A (pure) | THOROUGH | None |
| Greedy knapsack | THOROUGH | THOROUGH | THOROUGH | None |
| Backfill (two-pass) | THOROUGH | THOROUGH | THOROUGH | None |
| Autoscaler | THOROUGH | N/A | THOROUGH | None |
| Preemption | MODERATE | MODERATE | SHALLOW | No victim cost ordering unit test |
| Scale executor | SHALLOW | None | None | InfraScaleExecutor untested |
| Reconciliation | MODERATE | MODERATE | MODERATE | should_requeue tested; edge cases covered |

## BDD Coverage (41 scenarios)

| Feature | Scenarios | THOROUGH | MODERATE | SHALLOW |
|---------|-----------|----------|----------|---------|
| scheduling_cycle | 12 | 5 | 6 | 1 |
| preemption | 13 | 2 | 9 | 2 |
| autoscaling | 8 | 4 | 1 | 3 |
| walltime_enforcement | 7 | 6 | 1 | 0 |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G5 | InfraScaleExecutor has no unit tests (only NoopScaleExecutor) | Medium | scale_executor.rs |
| G6 | No unit test for preemption victim cost ordering | Medium | Delegated to hpc-scheduler-core |
| G7 | f9 conformance_fitness has no direct unit test | Low | cost.rs / conformance.rs |
| G8 | memory_locality uses mocked values in cycle tests | Low | cycle.rs |
| G9 | Backfill deadline enforcement not explicitly verified | Low | loop_runner.rs |
| G10 | Gang preemption partial-failure cascade untested | Low | loop_runner.rs |
| G11 | Reactive allocation per-alloc min_nodes vs config min_nodes | Low | autoscaler.rs |

## Confidence: HIGH

Cost function and knapsack solver are extremely well tested. Autoscaler comprehensive. Scheduler implementations all covered. Preemption and scale executor are the weaker areas but covered by BDD/integration tests.

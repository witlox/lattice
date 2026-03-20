# Fidelity: Federation + Multi-tenant (Chunk 10)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| federation.rs | 12 | 12 | 0 | 0 | Trust, capacity, sensitivity, expiry |
| borrowing.rs | 12 | 12 | 0 | 0 | Capacity, lending/borrowing flags |
| quota.rs | 7 | 0 | 7 | 0 | GPU-hours cumulative missing |
| dag.rs | 12 | 12 | 0 | 0 | Cycle detection, topo sort |
| dag_controller.rs | 13 | 13 | 0 | 0 | Unblock, cancel, unsatisfiable |
| quota_store.rs | 8 | 8 | 0 | 0 | Range nodes, tenant lifecycle |
| BDD: federation | 10 scen | 2 | 5 | 3 | Sovra mock-only; retry backoff |
| BDD: quota_enforcement | 9 scen | 2 | 5 | 2 | Soft quota scoring not evaluated |
| BDD: tenant_management | 8 scen | 1 | 6 | 1 | Strict isolation not enforced |
| BDD: session_management | 7 scen | 0 | 5 | 2 | Durability, timeout missing |
| BDD: dag_workflow | 11 scen | 3 | 7 | 1 | Parallelism structural only |
| **TOTAL** | **109** | **65** | **35** | **9** | |

## Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G69 | Federation + quota race: offer accepted but quorum rejects at commit | High | federation.rs + quota.rs |
| G70 | DAG + quota race: downstream proposed after quota reduced mid-DAG | High | dag_controller.rs + quota.rs |
| G71 | Soft quota scoring (f3) not verified in integration with cost function | High | quota.rs + cost.rs |
| G72 | Borrowing priority field unused (stub) | Medium | borrowing.rs:49 |
| G73 | Borrowing return grace period (return_grace_secs) untested | Medium | borrowing.rs |
| G74 | DAG Corresponding condition: exists in code, never tested | Medium | dag.rs |
| G75 | Session durability on node failure not tested | Medium | session BDD |
| G76 | Session idle timeout not implemented | Medium | session BDD |
| G77 | GPU-hours cumulative consumption model simplified (count-based) | Low | quota.rs |
| G78 | Quota reduction in-flight proposal race untested | Low | quota_enforcement BDD |

## Confidence: HIGH (unit) / MODERATE (BDD integration)

Core logic (federation evaluation, borrowing limits, DAG validation, controller lifecycle) thoroughly unit-tested. BDD scenarios cover happy paths and main error cases. Cross-context races (federation+quota, DAG+quota) are the primary risk area.

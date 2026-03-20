# Adversarial Findings
Last sweep: 2026-03-20
Status: COMPLETE

## Summary

| Severity | Count | Resolved | Open |
|----------|-------|----------|------|
| Critical | 2 | 0 | 2 |
| High | 8 | 0 | 8 |
| Medium | 10 | 0 | 10 |
| Low | 0 | 0 | 0 |

## Open findings (sorted by severity)

| # | Title | Severity | Category | Location | Status |
|---|-------|----------|----------|----------|--------|
| F01 | REST endpoints lack OIDC/RBAC middleware | Critical | Auth Bypass | rest.rs + server.rs | Open |
| F02 | Missing HTTP timeout on TSDB requests | Critical | Resource Exhaustion | tsdb_client.rs:135 | Open |
| F03 | Missing authorization on cancel/update RPCs | High | Trust Boundary | allocation_service.rs:199,216 | Open |
| F04 | Cross-tenant allocation list leak | High | Trust Boundary | allocation_service.rs:174 | Open |
| F05 | Service registry exposes all tenants | High | Information Disclosure | rest.rs + admin_service.rs | Open |
| F06 | DAG dependency validation missing server-side | High | Input Validation | allocation_service.rs:66 | Open |
| F07 | TaskGroup range_start > range_end accepted | High | Input Validation | allocation_service.rs:104 | Open |
| F08 | Empty DAG allocations list accepted | High | Input Validation | allocation_service.rs:66 | Open |
| F09 | JWKS cache poisoning via HTTP redirect | High | Token Forgery | oidc.rs:232 | Open |
| F10 | Audit signing key not persisted across restart | High | Audit Integrity | global_state.rs:82 | Open |
| F11 | Unwrap after checked borrow in assign_nodes | Medium | Panic | global_state.rs:346 | Open |
| F12 | Unwrap after checked borrow in claim_node | Medium | Panic | global_state.rs:455 | Open |
| F13 | Reconciler/scheduler TOCTOU window | Medium | Concurrency | loop_runner.rs:161-171 | Open |
| F14 | Requeue count double-increment risk | Medium | Concurrency | global_state.rs:360 | Open |
| F15 | Empty tenant/project/entrypoint accepted | Medium | Input Validation | convert.rs:97 | Open |
| F16 | Reactive lifecycle min > max accepted | Medium | Input Validation | convert.rs:397 | Open |
| F17 | Service endpoint port=0 or >65535 truncated | Medium | Input Validation | convert.rs:522 | Open |
| F18 | max_requeue unbounded (u32::MAX) | Medium | Input Validation | convert.rs:108 | Open |
| F19 | O(n) VNI pool allocation scan | Medium | Performance | global_state.rs:114 | Open |
| F20 | Sensitive session limit per-node only (not global) | Medium | Isolation Bypass | attach.rs:167 | Open |

## Resolved findings

(none yet)

## Detail Files

- [api-input-validation.md](api-input-validation.md) — Findings F03-F08, F15-F18
- [security-auth.md](security-auth.md) — Findings F01, F05, F09, F10, F20
- [concurrency-races.md](concurrency-races.md) — Findings F13, F14
- [robustness-resources.md](robustness-resources.md) — Findings F02, F11, F12, F19

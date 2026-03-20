# Adversarial Findings
Last sweep: 2026-03-20
Status: COMPLETE — ALL RESOLVED

## Summary

| Severity | Count | Resolved | Open |
|----------|-------|----------|------|
| Critical | 2 | 2 | 0 |
| High | 8 | 8 | 0 |
| Medium | 10 | 10 | 0 |
| Low | 0 | 0 | 0 |

## Open findings

(none)

## Resolved findings

| # | Title | Severity | Resolution | Resolved in |
|---|-------|----------|------------|-------------|
| F01 | REST endpoints lack OIDC/RBAC middleware | Critical | Added rest_auth_middleware layer (Bearer token required when OIDC configured) | df722a5 |
| F02 | Missing HTTP timeout on TSDB requests | Critical | VictoriaMetricsClient now uses configured timeout_secs via reqwest::Client::builder() | df722a5 |
| F03 | Missing authorization on cancel/update RPCs | High | Added ownership check (alloc.user == requester) | df722a5 |
| F04 | Cross-tenant allocation list leak | High | Added x-lattice-tenant extraction + filter enforcement on list RPC | this commit |
| F05 | Service registry exposes all tenants | High | Tenant filtering on LookupService/ListServices via x-lattice-tenant header | df722a5 |
| F06 | DAG dependency validation missing server-side | High | Added unknown ref + self-cycle detection in DAG submit handler | df722a5 |
| F07 | TaskGroup range_start > range_end accepted | High | Added explicit validation, step=0 rejected | df722a5 |
| F08 | Empty DAG allocations list accepted | High | Added empty check before processing | df722a5 |
| F09 | JWKS cache poisoning via HTTP redirect | High | reqwest redirect Policy::none(), HTTPS warning on non-https issuer | this commit |
| F10 | Audit signing key not persisted across restart | High | audit_signing_key_path in QuorumConfig, load_signing_key_from_file() on startup | this commit |
| F11 | Unwrap after checked borrow in assign_nodes | Medium | Replaced with proper error return | df722a5 |
| F12 | Unwrap after checked borrow in claim_node | Medium | Replaced with proper error return | df722a5 |
| F13 | Reconciler/scheduler TOCTOU window | Medium | Requeued alloc IDs excluded from pending via HashSet filter | df722a5 |
| F14 | Requeue count double-increment risk | Medium | Added expected_requeue_count to RequeueAllocation command (optimistic concurrency) | this commit |
| F15 | Empty tenant/project/entrypoint accepted | Medium | Validation added in allocation_from_proto | df722a5 |
| F16 | Reactive lifecycle min > max accepted | Medium | Validation added in allocation_from_proto | df722a5 |
| F17 | Service endpoint port=0 or >65535 truncated | Medium | Port range 1-65535 validated | df722a5 |
| F18 | max_requeue unbounded (u32::MAX) | Medium | Capped at 100 | df722a5 |
| F19 | O(n) VNI pool allocation scan | Medium | next_candidate pointer for O(1) amortized | df722a5 |
| F20 | Sensitive session limit per-node only | Medium | Global session tracking via Raft state (sessions HashMap + CreateSession/DeleteSession commands) | this commit |

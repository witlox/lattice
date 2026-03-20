# Fidelity: Security + Auth (Chunk 5)
Assessed: 2026-03-20

## Test Inventory

| Component | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|-----------|-------|----------|----------|---------|----------|
| OIDC token validation | 11 | 11 | 0 | 0 | JWKS refresh on kid miss, clock skew |
| RBAC authorization | 12 | 12 | 0 | 0 | ReadOnly role untested, 20 ops untested |
| Rate limiting | 10 | 10 | 0 | 0 | Concurrent access, API integration |
| TLS configuration | 14 | 14 | 0 | 0 | mTLS handshake E2E, PEM format |
| Audit log + hash chain | 11 | 11 | 0 | 0 | Retention enforcement, concurrent writes |
| Sensitive workload (BDD) | 12 | 0 | 7 | 5 | Real image crypto, storage encryption |
| Identity cascade (BDD) | 13 | 0 | 8 | 5 | Real SPIRE calls, cache persistence |
| Certificate rotation (BDD) | 9 | 0 | 6 | 3 | Concurrent rotation, connection disruption |
| **TOTAL** | **92** | **58** | **21** | **13** | |

## Attack Vector Coverage

| Vector | Covered | Gap |
|--------|---------|-----|
| Stolen OIDC token | Partial — expiry tested, revocation NOT tested | Token revocation check missing |
| Rogue node agent (wrong cert CN) | No — mTLS config tested, peer CN validation NOT | Critical gap |
| Fake heartbeat | Yes — ADV-06 owner_version rejection tested | Solid |
| Compromised uenv image | Partial — sign_required flag, no real crypto | Image signature verification stub-only |
| Audit log manipulation | Yes — hash chain + ed25519 signatures verified | Solid |
| Cross-tenant data access | Yes — RBAC tenant scope enforcement tested | Solid |
| API flooding | Yes — rate limiter tested | No concurrent stress test |
| Container/namespace escape | No — Sarus seccomp/uenv namespace not tested | Out of scope (kernel-level) |
| Priority escalation | No — RBAC prevents setting, not enforcement tested | Medium gap |

## Critical Findings

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G20 | No E2E mTLS handshake test with peer certificate validation | High | server.rs |
| G21 | Image signature verification is stub-only (sign_required flag, no crypto) | High | sensitive workload flow |
| G22 | OIDC token revocation/introspection not tested | Medium | oidc.rs |
| G23 | ReadOnly RBAC role has no allow test | Medium | rbac.rs |
| G24 | Only 7/27 RBAC operations tested via gRPC method mapping | Medium | rbac.rs |
| G25 | Audit retention limit exists but enforcement not tested | Medium | global_state.rs |
| G26 | Sensitive node quarantine on wipe failure not verified in scheduler | Medium | epilogue.rs + scheduler |
| G27 | No concurrent rate limiter test | Low | rate_limit.rs |
| G28 | JWKS refresh on kid miss not tested | Low | oidc.rs |

## Confidence: HIGH (core auth) / MODERATE (sensitive isolation)

OIDC, RBAC, rate limiting, TLS config, and audit hash chain are all thoroughly unit-tested. The weaker areas are sensitive workload isolation (mock-only wipe/crypto) and identity cascade (stub providers). Production-ready for baseline auth; sensitive features need hardening.

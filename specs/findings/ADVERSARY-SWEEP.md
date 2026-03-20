# Adversarial Sweep Plan
Status: COMPLETE
Started: 2026-03-20

## Attack surface

| Surface | Entry points | Trust level | Fidelity |
|---------|-------------|-------------|----------|
| gRPC API (allocation/node/admin) | 30+ RPCs | Authenticated (OIDC+RBAC) | HIGH |
| REST API | 25+ routes | **UNAUTHENTICATED** | HIGH (logic) / CRITICAL (auth) |
| Proto conversion (input parsing) | allocation_from_proto | Semi-trusted (user input) | HIGH |
| Raft state machine | 18 commands | Trusted (internal) | HIGH |
| Node agent commands | 6 command types | Trusted (internal) | MODERATE |
| Service discovery | 2 RPCs + 2 REST | **NO TENANT FILTERING** | LOW |
| Liveness probes | TCP/HTTP to localhost | Trusted (agent-local) | MODERATE |

## Chunks

| # | Scope | Attack vectors | Status | Session |
|---|-------|---------------|--------|---------|
| 1 | API + Input Validation | Input validation, trust boundaries, state machine | DONE | 2026-03-20 |
| 2 | Concurrency + Races | TOCTOU, scheduler ordering, clock skew | DONE | 2026-03-20 |
| 3 | Security + Auth Bypass | RBAC coverage, OIDC cache, audit forgery, cross-tenant | DONE | 2026-03-20 |
| 4 | Robustness + Resources | Unbounded growth, timeouts, panics, error swallowing | DONE | 2026-03-20 |

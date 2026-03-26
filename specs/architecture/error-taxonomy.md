# Error Taxonomy

Per-module error types and their semantics. All errors flow through the unified `LatticeError` enum in lattice-common, ensuring consistent error handling across the system.

## Unified Error Enum (lattice-common)

```
LatticeError
  ├── AllocationNotFound(AllocId)
  │     Returned when: Get, Cancel, Update, Watch, Checkpoint on unknown ID
  │     HTTP: 404    gRPC: NOT_FOUND
  │
  ├── NodeNotFound(NodeId)
  │     Returned when: GetNode, DrainNode on unknown xname
  │     HTTP: 404    gRPC: NOT_FOUND
  │
  ├── TenantNotFound(TenantId)
  │     Returned when: Operations referencing non-existent tenant
  │     HTTP: 404    gRPC: NOT_FOUND
  │
  ├── QuotaExceeded { tenant, quota_type, current, limit }
  │     Returned when: Proposal would violate INV-S2
  │     HTTP: 429    gRPC: RESOURCE_EXHAUSTED
  │
  ├── OwnershipConflict { node_id, current_owner }
  │     Returned when: ClaimNode/AssignNodes on already-owned node (INV-S1)
  │     HTTP: 409    gRPC: ALREADY_EXISTS
  │
  ├── SensitiveIsolation(String)
  │     Returned when: Operation violates INV-S5 or INV-S6
  │     HTTP: 403    gRPC: PERMISSION_DENIED
  │
  ├── SensitiveAuditRequired(String)
  │     Returned when: Audit commit failed, action cannot proceed (INV-O3)
  │     HTTP: 503    gRPC: UNAVAILABLE
  │
  ├── InvalidDag(String)
  │     Returned when: DAG has cycle (INV-E2) or exceeds size limit (INV-C5)
  │     HTTP: 400    gRPC: INVALID_ARGUMENT
  │
  ├── InvalidAllocation(String)
  │     Returned when: Submission validation fails (bad resources, missing fields)
  │     HTTP: 400    gRPC: INVALID_ARGUMENT
  │
  ├── QuorumUnavailable
  │     Returned when: Raft quorum has no leader (FM-02, FM-03)
  │     HTTP: 503    gRPC: UNAVAILABLE
  │
  ├── LeaderElection
  │     Returned when: Leader election in progress
  │     HTTP: 503    gRPC: UNAVAILABLE
  │
  ├── AuthenticationFailed(String)
  │     Returned when: OIDC token invalid/expired
  │     HTTP: 401    gRPC: UNAUTHENTICATED
  │
  ├── AuthorizationFailed { user, operation, required_role }
  │     Returned when: RBAC check fails
  │     HTTP: 403    gRPC: PERMISSION_DENIED
  │
  ├── RateLimited
  │     Returned when: Token bucket exhausted
  │     HTTP: 429    gRPC: RESOURCE_EXHAUSTED
  │
  ├── PreemptionClassViolation { requester_class, victim_class }
  │     Returned when: INV-E1 violation attempt
  │     HTTP: 400    gRPC: INVALID_ARGUMENT
  │
  ├── NodeUnavailable { node_id, state }
  │     Returned when: Operation on Down/Degraded/Draining node
  │     HTTP: 409    gRPC: FAILED_PRECONDITION
  │
  ├── WalltimeExceeded(AllocId)
  │     Returned when: Allocation terminated by walltime (INV-E4)
  │     HTTP: N/A (internal event, not API error)
  │
  ├── CheckpointFailed { alloc_id, reason }
  │     Returned when: Checkpoint times out or node crashes
  │     HTTP: 500    gRPC: INTERNAL
  │
  ├── StorageError(String)
  │     Returned when: VAST/S3 operation fails (FM-11)
  │     HTTP: 502    gRPC: UNAVAILABLE
  │
  ├── InfrastructureError(String)
  │     Returned when: OpenCHAMI operation fails (FM-12)
  │     HTTP: 502    gRPC: UNAVAILABLE
  │
  ├── FederationError(String)
  │     Returned when: Cross-site request fails (feature-gated)
  │     HTTP: 502    gRPC: UNAVAILABLE
  │
  ├── DataSovereigntyViolation(String)
  │     Returned when: Sensitive data would leave jurisdiction (INV-N3)
  │     HTTP: 403    gRPC: PERMISSION_DENIED
  │
  └── Internal(String)
        Returned when: Unexpected internal error
        HTTP: 500    gRPC: INTERNAL
```

## Startup Errors (not propagated to API)

### lattice-common: SecretResolutionError

```
SecretResolutionError
  ├── ConnectionFailed { address, source }   — Vault TCP/TLS connection failed
  ├── AuthenticationFailed { address, detail } — Vault AppRole auth rejected
  ├── PathNotFound { path }                   — Vault KV v2 path does not exist
  ├── KeyNotFound { path, key }               — Path exists, field missing
  ├── NotFound { section, field }             — Env var unset AND config empty
  ├── InvalidFormat { section, field, reason } — Base64 decode failed (binary secret)
  └── VaultError { path, status, body }       — Unexpected Vault response
```

All variants are fatal startup errors. Not part of `LatticeError` — never reaches API layer. Used in `main()` to print descriptive error and exit. No variant contains resolved secret values (INV-SEC2).

## Module-Specific Errors

### lattice-node-agent: RuntimeError

```
RuntimeError
  ├── PrepareFailed { alloc_id, reason }     — Prologue failed (image pull, mount, staging)
  ├── SpawnFailed { alloc_id, reason }       — Process spawn failed
  ├── StopFailed { alloc_id, reason }        — Graceful stop failed
  ├── SignalFailed { alloc_id, signal }      — Signal delivery failed
  └── NotFound { alloc_id }                  — Allocation not running on this node
```

Maps to `LatticeError::Internal` when propagated to API layer.

### lattice-scheduler: Placement Errors

Not errors per se — expressed as `PlacementDecision::Defer { reason }`:

| Reason | Meaning | Resolution |
|---|---|---|
| `no_available_nodes` | All nodes owned or unavailable | Wait for releases |
| `quota_exceeded` | Hard quota would be violated | Wait for completions |
| `topology_constraint` | Cannot fit in required groups | Wait or relax constraint |
| `conformance_mismatch` | No conformant nodes available | Wait for conformant nodes |
| `vni_pool_exhausted` | No VNIs available | Wait for domain teardown |
| `data_not_staged` | Data staging incomplete | Wait for staging (f5 will improve) |

### lattice-checkpoint: Evaluation Errors

Checkpoint evaluation returns `None` (don't checkpoint) rather than errors. Actual errors in delivery:

| Error | Meaning | Handling |
|---|---|---|
| NodeAgentUnreachable | Cannot deliver hint | Skip (allocation non-preemptible) |
| PartialFailure | Some nodes accepted, some didn't | Report partial, proceed with what succeeded |
| TimeoutExceeded | Application didn't respond | SIGTERM → SIGKILL → Failed |

## Error Handling Principles

1. **Typed over stringly-typed.** Every error variant carries structured data for programmatic handling.
2. **Caller decides severity.** `LatticeError` is neutral — the API layer maps to HTTP/gRPC codes; the scheduler treats rejections as retry-next-cycle.
3. **No silent swallowing** (INV-N4). Every error path either returns to the caller, increments a metric, or writes an audit entry (sensitive).
4. **Retry guidance.** Transient errors (QuorumUnavailable, LeaderElection, RateLimited) → client retries with backoff. Permanent errors (InvalidAllocation, AuthorizationFailed) → no retry.

## Error → Metric Mapping

| Error Category | Metric |
|---|---|
| QuotaExceeded | `lattice_quota_rejections_total{tenant, type}` |
| OwnershipConflict | `lattice_ownership_conflicts_total` |
| QuorumUnavailable | `lattice_quorum_unavailable_total` |
| CheckpointFailed | `lattice_checkpoint_failures_total{reason}` |
| StorageError | `lattice_storage_errors_total` |
| AuthenticationFailed | `lattice_auth_failures_total{type=authn}` |
| AuthorizationFailed | `lattice_auth_failures_total{type=authz}` |
| RateLimited | `lattice_rate_limit_rejections_total` |

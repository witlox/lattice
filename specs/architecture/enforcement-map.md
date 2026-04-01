# Enforcement Map

Maps every invariant from `specs/invariants.md` to its enforcement point(s) in the architecture. An invariant without a clear enforcement point is an invariant that will be violated.

## Strong Consistency Invariants

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-S1** Exclusive node ownership | lattice-quorum | `Command::ClaimNode` / `Command::AssignNodes` validation in `GlobalState::apply()` — rejects if node already owned | Unit tests (quorum), acceptance tests (scheduling_cycle.feature) |
| **INV-S2** Hard quota non-violation | lattice-quorum | `Command::AssignNodes` sums current ownership per tenant, rejects if exceeds `max_nodes` or `max_concurrent_allocations` | Unit tests (quorum), acceptance tests (quota_enforcement.feature) |
| **INV-S3** Sensitive audit completeness | lattice-quorum + lattice-api | API handlers for sensitive operations call `AuditLog::record()` (Raft commit) BEFORE executing the action. Gate pattern: audit commit → action. | Acceptance tests (sensitive_workload.feature), cross-context (audit ordering scenarios) |
| **INV-S4** Sensitive audit immutability | lattice-quorum | `GlobalState.audit_log` is `Vec<AuditEntry>` — append-only. No `Command` variant exists to modify or delete entries. Raft log structure provides tamper evidence. | Structural (no mutation API exists) |
| **INV-S5** Sensitive node isolation | lattice-quorum + lattice-scheduler | Quorum rejects proposals assigning sensitive-claimed nodes to different allocations. `SensitiveReservationScheduler` never participates in borrowing. | Unit tests (scheduler), acceptance tests (sensitive_workload.feature) |
| **INV-S6** Sensitive wipe before reuse | lattice-node-agent + lattice-quorum | Node agent epilogue: GPU clear (`nvidia-smi --gpu-reset` / `rocm-smi --resetgpu`) → report to quorum → OpenCHAMI NVMe erase → wipe confirmation Raft-committed → node returns to pool. Wipe failure → quarantine (Down). | Acceptance tests (sensitive_workload.feature). **GAP: ESC-001 GPU HBM wipe step needs implementation verification.** |

## Eventual Consistency Invariants

| Invariant | Enforcement Module | Enforcement Mechanism | Convergence Bound |
|---|---|---|---|
| **INV-E1** Preemption class ordering | lattice-scheduler | `evaluate_preemption()` filters candidates where `victim.preemption_class < requester.preemption_class`. API admission validates class 0-10 range. Sensitive = class 10 (never preempted). | Immediate (evaluated each cycle) |
| **INV-E2** DAG acyclicity | lattice-scheduler + lattice-cli | `validate_dag()` runs Kahn's algorithm at submission time. CLI `validate_dag_yaml()` also checks. API rejects cyclic DAGs. | At submission (rejected before entering queue) |
| **INV-E3** Dependency satisfaction | lattice-scheduler | `DagController` evaluates edges on each allocation state change. Downstream allocations enter queue only when all incoming conditions are met. | ≤1 scheduling cycle (~5-30s) |
| **INV-E4** Walltime supremacy | lattice-node-agent | `WalltimeEnforcer` timer runs independently of checkpoint broker. SIGTERM → 30s grace → SIGKILL. Not extendable by checkpoint. | Immediate (OS timer) |
| **INV-E5** Network domain tenant scoping | lattice-api | API validation on submission: domain name scoped to tenant ID internally. Cross-tenant domain names resolve to different VNIs. | At submission |
| **INV-E6** Soft quota self-correction | lattice-scheduler | Cost function f3 (fair_share_deficit) penalizes over-budget tenants. Budget penalty multiplier reduces scores for tenants exceeding soft quotas. | ≤1 scheduling cycle |

## Ordering Invariants

| Invariant | Enforcement Module | Enforcement Mechanism |
|---|---|---|
| **INV-O1** Proposal before execution | lattice-quorum → lattice-node-agent | Quorum notifies node agents AFTER Raft commit (gRPC `RunAllocation` sent only after `Command::AssignNodes` committed). Node agent waits for this notification. |
| **INV-O2** Prologue before entrypoint | lattice-node-agent | `AllocationRunner` state machine: `Staging` → prologue → `Running` → entrypoint. Prologue failure → retry or fail. Entrypoint never starts without successful prologue. |
| **INV-O3** Audit before sensitive action | lattice-api + lattice-quorum | Sensitive endpoint handlers: `audit.record(entry).await?` (Raft commit) → proceed with action. If audit commit fails, action is not performed. |

## Cardinality Invariants

| Invariant | Enforcement Module | Enforcement Mechanism |
|---|---|---|
| **INV-C1** Node ownership cardinality (0:1) | lattice-quorum | Same as INV-S1. `GlobalState` stores `NodeOwnership` as `Option` — at most one owner. |
| **INV-C2** Sensitive attach cardinality (0:1) | lattice-node-agent | Attach handler checks `sensitive && active_sessions > 0` → reject. Single-session guard per sensitive allocation. |
| **INV-C3** VNI uniqueness | lattice-node-agent (network) + lattice-quorum | VNI pool allocator: sequential allocation, `HashMap<VNI, DomainId>` ensures 1:1 mapping. Released on domain teardown. |
| **INV-C4** Allocation-vCluster binding | lattice-quorum | `Allocation.vcluster` is immutable after creation. No `Command` variant exists to change it. |
| **INV-C5** DAG size limit | lattice-api + lattice-cli | API admission checks `dag.allocations.len() <= max_dag_size` (default 1000). CLI `validate_dag_yaml()` also checks. |

## Negative Invariants

| Invariant | Enforcement Module | Enforcement Mechanism |
|---|---|---|
| **INV-N1** No Kubernetes dependencies | Build system | `deny.toml` bans K8s-related crates. Code review. |
| **INV-N2** No SSH between compute | lattice-node-agent | PMI-2 over gRPC for inter-node coordination. No `ssh` or `sshd` invocations. Sensitive hardened images have no SSH daemon. |
| **INV-N3** No sensitive data federation | lattice-scheduler (federation) | `FederationBroker` policy check: rejects requests involving sensitive data sovereignty violations. `DataStager` refuses cross-site transfers for sensitive allocations. |
| **INV-N4** No silent failure | All modules | `LatticeError` typed error enum. Metrics for all failure paths (`lattice_*` counters). Alert rules in `infra/`. Audit log for sensitive. |
| **INV-N5** Accounting never blocks scheduling | lattice-common (Waldur client) + lattice-scheduler | Async push with bounded buffer (10K memory + 100K disk). Events dropped with `lattice_accounting_events_dropped_total` counter rather than blocking. Scheduling cycle never awaits Waldur response. |

## Network Topology Invariants

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-NET1** HSN binding | lattice-common (config) | `BindNetwork` enum on `QuorumConfig`, `ApiConfig`, `NodeAgentConfig`. Default `Any` (dev); production sets `Hsn`. Config validation logs warning if `Any` used with >10 nodes. | Config tests, deployment validation |
| **INV-NET2** No port conflicts (co-located) | Deployment config | Lattice ports (50051, 9000, 8080) vs PACT ports (9443, 9444) on different NICs. Startup bind failure is immediate and fatal. | Docker compose tests, deployment docs |

## Resource Isolation Invariants (hpc-node)

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-RI1** Cgroup slice ownership | lattice-node-agent | `CgroupManager::create_scope()` only creates under `workload.slice/`. Standalone: creates hierarchy at startup. PACT-managed: verifies hierarchy exists. | Unit tests (cgroup manager), acceptance tests |
| **INV-RI2** Namespace handoff fallback | lattice-node-agent | `NamespaceConsumer::request_namespaces()` attempts socket → 1s timeout → self-service via `unshare(2)`. Audit event on fallback. | Unit tests (dual-mode consumer), integration tests |
| **INV-RI3** Mount refcount consistency | lattice-node-agent | `MountManager` tracks refcounts in HashMap. `reconstruct_state()` on agent restart scans `/proc/mounts`. Orphaned mounts get hold timer. | Unit tests (refcount tracking), property tests |
| **INV-RI4** Readiness gate non-blocking | lattice-node-agent | `ReadinessGate::wait_ready()` polls with `readiness_timeout` (default 30s). Timeout → proceed + log warning. Standalone: `NoopReadinessGate` always ready. | Unit tests, acceptance tests |

## Audit Format Invariants (hpc-audit)

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-AF1** Standardized audit event schema | lattice-common + lattice-quorum | `AuditEntry` wraps `hpc_audit::AuditEvent`. All audit code paths construct `AuditEvent` with required fields. `AuditSource` enum set per component. | Unit tests (serialization round-trip), acceptance tests (audit scenarios) |
| **INV-AF2** Well-known action constants | All crates emitting audit | Action strings imported from `hpc_audit::actions::*`. Custom actions use `lattice.` prefix, defined as constants in lattice-common. | Code review, grep for raw string action literals |
| **INV-AF3** Audit sink non-blocking | lattice-common | `AuditSink::emit()` buffers in-memory, proposes to Raft async. Exception: sensitive events block on Raft commit (INV-S3). Buffer overflow: drop non-sensitive + counter metric. | Unit tests (buffer behavior), load tests |

## Identity Invariants (hpc-identity)

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-ID1** Identity cascade order | lattice-node-agent, lattice-quorum | `IdentityCascade::new()` called with `[SpireProvider, SelfSignedProvider, StaticProvider]`. Provider list immutable after construction. | Unit tests (cascade ordering), integration tests |
| **INV-ID2** Private key locality | lattice-node-agent, lattice-quorum | `IdentityProvider::get_identity()` generates keys in-process. CSR signing sends only public key. `WorkloadIdentity` Debug redacts `private_key_pem`. | Code review, unit tests (no key in logs) |
| **INV-ID3** Cert rotation non-disruptive | lattice-node-agent, lattice-quorum | `CertRotator::rotate()` uses dual-channel swap: build → health-check → swap → drain. Failure leaves active channel unchanged. | Integration tests (rotation under load) |

## Secret Resolution Invariants

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-SEC1** Resolution before serving | lattice-api, lattice-quorum (`main()`) | `SecretResolver::resolve_all()` called before server construction. `Err` → `process::exit(1)`. | Acceptance tests (secret_resolution.feature), unit tests |
| **INV-SEC2** Secrets never logged | lattice-common (`SecretValue`) | `SecretValue` wrapper: `Debug`/`Display` emit `[REDACTED]`. Only `expose()` reveals value. No `Serialize` impl. | Unit tests (debug output), code review (grep for `.expose()` usage) |
| **INV-SEC3** Vault down = fatal at startup | lattice-common (`VaultBackend::new()`) | AppRole auth and first read attempted at construction. Failure → `SecretResolutionError::ConnectionFailed` or `AuthenticationFailed`. No retry. | Integration tests (Vault mock down), acceptance tests |
| **INV-SEC4** Vault global override | lattice-common (`SecretResolver::new()`) | If `config.vault.is_some()`, backend is `VaultBackend` only. `FallbackChain` not constructed. | Unit tests (backend selection), acceptance tests |
| **INV-SEC5** Convention-based paths | lattice-common (`VaultBackend::fetch()`) | Path = `{prefix}/{section}`, key = `{field}`. Pure function. No lookup tables. | Unit tests (path mapping), acceptance tests |
| **INV-SEC6** Functional without Vault | lattice-common (`FallbackChain`) | When `config.vault.is_none()`, `EnvBackend` → `ConfigBackend` chain used. All tests run without Vault. | All existing tests (no Vault in CI), acceptance tests |

## Cross-Context Enforcement

Some invariants require coordination across modules:

### INV-S1 + INV-S5 (Ownership + Sensitive Isolation)

```
lattice-scheduler                    lattice-quorum
     │                                    │
     ├─ SensitiveReservationScheduler     │
     │  never borrows/lends nodes ────────┤
     │                                    │
     ├─ proposes ClaimNode ──────────────►│
     │                                    ├─ validates: node unowned
     │                                    ├─ validates: sensitive isolation
     │                                    └─ Raft-commits or rejects
```

### INV-S3 + INV-O3 (Audit Completeness + Ordering)

```
lattice-api                    lattice-quorum
     │                              │
     ├─ sensitive claim request     │
     ├─ audit.record(claim) ──────►│
     │                              ├─ Raft-commit audit entry
     │  ◄── commit confirmed ──────┤
     ├─ NOW execute claim           │
     ├─ quorum.propose(ClaimNode) ►│
```

### INV-S6 (Wipe Before Reuse — Multi-Step)

```
lattice-node-agent              lattice-quorum        OpenCHAMI
     │                              │                     │
     ├─ GPU HBM clear              │                     │
     │  (nvidia-smi/rocm-smi)     │                     │
     ├─ report GPU clear ─────────►│                     │
     │                              ├─ Raft-commit       │
     ├─ request NVMe wipe ─────────┼────────────────────►│
     │                              │                     ├─ SecureErase
     │  ◄── wipe complete ─────────┼─────────────────────┤
     ├─ report wipe complete ──────►│                     │
     │                              ├─ Raft-commit       │
     │                              ├─ node → Ready      │
```

### INV-RI2 (Namespace Handoff Fallback — Dual-Mode)

```
lattice-node-agent
     │
     ├─ probe /run/pact/handoff.sock
     │
     ├─ [socket exists] ─────────────────────► PACT agent
     │  NamespaceConsumer.request_namespaces()   │
     │  via unix socket + SCM_RIGHTS             │
     │  ◄── FDs returned ───────────────────────┤
     │
     └─ [socket absent / timeout 1s]
        NamespaceConsumer self-service mode
        unshare(2) + mount uenv directly
        emit audit: namespace.handoff_failed
```

### INV-AF1 + INV-S3 (Audit Event Wrapping + Sensitive Ordering)

```
Any lattice component              lattice-common           lattice-quorum
     │                                  │                       │
     ├─ construct AuditEvent           │                       │
     │  (hpc_audit::AuditEvent)        │                       │
     ├─ call AuditSink::emit(event) ──►│                       │
     │                                  ├─ wrap in AuditEntry  │
     │                                  │  (ed25519 sign,      │
     │                                  │   chain hash)        │
     │                                  ├─ [non-sensitive]     │
     │                                  │  buffer + async ─────►│ Raft propose
     │                                  ├─ [sensitive]         │
     │                                  │  sync commit ────────►│ Raft commit
     │                                  │  ◄── confirmed ──────┤
     │  ◄── proceed with action ────────┤                       │
```

### INV-ID1 (Identity Cascade — Startup)

```
lattice-node-agent                    SPIRE      Quorum/Journal    Bootstrap
     │                                  │              │              │
     ├─ SpireProvider.is_available() ──►│              │              │
     │  [socket exists?]                │              │              │
     │                                  │              │              │
     ├─ [yes] get_identity() ──────────►│              │              │
     │  ◄── WorkloadIdentity ──────────┤              │              │
     │  source: Spire ✓                │              │              │
     │                                  │              │              │
     ├─ [no] SelfSignedProvider ───────┼──────────────►│              │
     │  generate key, send CSR         │              │              │
     │  ◄── signed cert ──────────────┼──────────────┤              │
     │  source: SelfSigned ✓           │              │              │
     │                                  │              │              │
     └─ [no] StaticProvider ───────────┼──────────────┼──────────────►│
        read files from disk           │              │              │
        ◄── WorkloadIdentity ──────────┼──────────────┼──────────────┤
        source: Bootstrap ✓            │              │              │
```

## Software Delivery Invariants

| Invariant | Enforcement Module | Enforcement Mechanism | Verified By |
|---|---|---|---|
| **INV-SD1** Content-addressed pinning | lattice-api | `ImageResolver.resolve()` pins sha256 at submit time; node agent pulls by sha256 | software_delivery.feature |
| **INV-SD2** View validation | lattice-api | `ImageResolver.metadata()` extracts views; submit rejects unknown view names | software_delivery.feature |
| **INV-SD3** Mount non-overlap | lattice-api | Prefix containment check on `ImageRef.mount_point` in `allocation_from_proto()` | software_delivery.feature |
| **INV-SD4** Deferred resolution timeout | lattice-scheduler | `run_once()` checks unresolved images each cycle; timeout from `submitted_at` | software_delivery.feature |
| **INV-SD5** Sensitive image integrity | lattice-api | Sensitive validation at submit: sign_required, digest ref, writable=false, no gpu=all | software_delivery.feature |
| **INV-SD6** Single pull for shared store | lattice-scheduler + lattice-node-agent | `DataStager` creates one staging request per image; `ImageStager.is_cached()` checks shared store first | software_delivery.feature |
| **INV-SD7** Env patch ordering | lattice-node-agent | Prologue iterates `env_patches` in declaration order, no sorting | software_delivery.feature |
| **INV-SD8** EDF inheritance bounded | lattice-api | Depth counter + visited set during EDF chain resolution; max 10 levels | software_delivery.feature |
| **INV-SD9** Image staging reuses data mover | lattice-scheduler | `DataStager.should_prefetch()` checks for uncached images alongside data mounts | software_delivery.feature |
| **INV-SD10** Namespace joining parentage | lattice-node-agent | `PodmanRuntime.spawn()` uses `setns()` then `exec()` — workload is agent-parented | software_delivery.feature |

## Gaps Identified

1. **ESC-001: GPU HBM wipe** — The epilogue path exists in lattice-node-agent but the GPU memory clear step for sensitive allocations needs verification that it actually calls `nvidia-smi --gpu-reset` / `rocm-smi --resetgpu` before the OpenCHAMI wipe. This is a regulatory compliance requirement (INV-S6).

2. **INV-S4 cryptographic signing** — The spec states audit entries are "cryptographically signed and chained." The current implementation uses `Vec<AuditEntry>` in the Raft log, which provides ordering and tamper evidence via Raft's own guarantees. Explicit per-entry cryptographic signatures (site PKI or Sovra keys) are not yet implemented. The Raft log itself provides equivalent integrity guarantees within a single cluster, but external verification (for regulatory auditors) may require explicit signatures.

3. **VNI uniqueness (INV-C3)** — Enforcement is split between node-agent (VNI pool) and quorum (network domain state). The authoritative VNI↔Domain mapping should be in the quorum to survive node agent restarts. Current implementation stores `network_domains: HashMap<String, NetworkDomain>` in GlobalState, which is correct.

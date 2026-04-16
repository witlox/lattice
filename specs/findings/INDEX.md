# Adversarial Findings
Last sweep: 2026-03-20
Last targeted review: 2026-04-16 (dispatch analyst output)
Status: ALL PRIOR SWEEP FINDINGS RESOLVED; DISPATCH REVIEW OPEN

## Summary

| Severity | Count | Resolved | Open |
|----------|-------|----------|------|
| Critical | 10 | 10 | 0 |
| High | 22 | 22 | 0 |
| Medium | 20 | 19 | 1 |
| Low | 4 | 3 | 1 |

Gate 2 adversary pass (dispatch-impl-review.md, 2026-04-16) found
3 Critical + 4 High + 3 Medium + 2 Low defects. All Criticals + Highs
now fixed. Remaining open: Medium-IMPL-08 (timeouts configurable but
not plumbed through LatticeConfig → SchedulerLoopConfig in main.rs;
needs a small wiring PR) and Low-IMPL-12 (automated step-coverage
test — nice-to-have, not blocking).

## Open findings (dispatch-impl-review.md — 2026-04-16)

| # | Title | Severity | Plan |
|---|---|---|---|
| D-ADV-IMPL-08 (residual) | Silent-sweep config is on SchedulerLoopConfig but main.rs doesn't map NodeAgentConfig.heartbeat_interval_seconds into it | Medium | Trivial wiring in lattice-api/src/main.rs — thread config.node_agent.heartbeat_interval_seconds into SchedulerLoopConfig. |
| D-ADV-IMPL-12 | No automated test that every feature-file phrase has a step | Low | Add tests/step_coverage.rs integration test. |

## Resolved during adversary gate 2 (dispatch, 2026-04-16)

| # | Title | Severity | Resolution |
|---|---|---|---|
| D-ADV-IMPL-01 | Dispatcher was dead code (never instantiated) | Critical | main.rs constructs Dispatcher + spawns tick loop after cancel_rx is available; on_leadership_change(true) for single-replica dev. |
| D-ADV-IMPL-02 | BareProcessRuntime couldn't run multi-word entrypoints | Critical | grpc_server handler now splits entrypoint on whitespace into (program, args) before spawning the Runtime. |
| D-ADV-IMPL-03 | Uenv/Podman unreachable (Dispatcher sent empty fields) | Critical | Dispatcher extracts uenv/image specs from allocation.environment.images (by ImageType); agent's run_allocation_monitor propagates them into PrepareConfig (IMPL-09 fixed simultaneously). |
| D-ADV-IMPL-04 | Silent-sweep bypassed INV-D12/D7/D4 validation | High | QuorumCommandSink::fail_allocation now uses Command::UpdateAllocationState with the reason carried through into allocation.message, documented as a scheduler-owned path (INV-D12 source-auth intentionally doesn't apply to scheduler decisions). |
| D-ADV-IMPL-05 | Dispatcher had no concurrency cap | High | Dispatcher holds a global tokio::Semaphore (max_concurrent_attempts) + per-agent Semaphore map (per_agent_concurrency). Permits acquired before each attempt. |
| D-ADV-IMPL-06 | DEC-DISP-11 multi-node aggregation simplified | High | Allocation.per_node_phase: HashMap<NodeId, CompletionPhase> added. ApplyCompletionReport updates per-node phase with monotonicity check, then recomputes aggregate state: Staging if any node unreported; Running only when ALL are Running+; Completed only when ALL Completed with exit 0; Failed if any node Failed or any non-zero exit. RollbackDispatch clears per_node_phase. |
| D-ADV-IMPL-07 | Silent-sweep fresh-exemption used wrong timestamp | High | Allocation.assigned_at field added, set by AssignNodes apply-step. Silent-sweep uses it via fallback chain (assigned_at → started_at → created_at). |
| D-ADV-IMPL-09 | PrepareConfig always empty on monitor | Medium | run_allocation_monitor takes uenv/image params from the RPC request and sets them on PrepareConfig. |
| D-ADV-IMPL-10 | RunAllocation idempotency had TOCTOU lock-drop | Medium | Handler now holds the lock across contains_active → start in a single critical section. Runtime selection lifted above the lock (pure function of req). |
| D-ADV-IMPL-11 | DispatchBridge uenv/podman previously unreachable | Low | Closed by IMPL-03 fix. |

## Resolved during analyst text-edit phase (dispatch, 2026-04-16)

| # | Title | Severity | Resolution |
|---|---|---|---|
| D-ADV-ARCH-04 | Leadership flap effectively doubles retry budget | High | DEC-DISP-04 amended: new leader pauses Dispatcher for one heartbeat_interval before first tick, allowing in-flight Completion Reports from previous leader to land. Budget amplification bounded to ~1 extra attempt per flap. |
| D-ADV-ARCH-06 | Malicious agent suppresses silent-sweep via reattach flag | High | INV-D5 reattach flag lifecycle: `reattach_in_progress` may only be set to true on first heartbeat after re-registration; grace timer is absolute from first-true heartbeat; set-to-false is one-way. Cannot be indefinitely suppressed. New field `reattach_first_set_at` on Node. |
| D-ADV-ARCH-07 | Silent-sweep races freshly-placed allocations | High | INV-D8 amended: silent-sweep only considers allocations whose `assigned_at` is older than `heartbeat_interval + grace_period`. Freshly-placed allocations exempt until they've had a chance to heartbeat. |
| D-ADV-ARCH-09 | ALREADY_RUNNING ghost case has long detection window | High | INV-D8 extended with second predicate: node heartbeating + no Completion Report advancing this allocation's phase for grace_period → also silent-sweep. New `allocation.last_completion_report_at` field on Allocation. Detects ghosting agents even when node is alive. |
| D-ADV-ARCH-10 | Unknown `refusal_reason` has no defined handling | Medium | DEC-DISP-05 amended: unknown `refusal_reason` values treated as MALFORMED_REQUEST (safe-fail). Counter `lattice_dispatch_unknown_refusal_reason_total{reason_code}` surfaces rolling-upgrade skew. |
| D-ADV-ARCH-11 | BareProcessRuntime env allow-list omits common HPC variables | Medium | Env scrubbing contract extended: allow-list now includes OMP_*, CUDA_*, ROCR_*, HIP_*, MKL_*, PMI_*, MPI_*, OMPI_*, MPICH_*, FI_*, NCCL_*, CXI_*, TMPDIR, XDG_RUNTIME_DIR, LD_LIBRARY_PATH, LD_PRELOAD. Merge semantics: user env_vars > site allow-list > agent env, all filtered by block-list. User key in block-list rejected as MALFORMED_REQUEST at prologue. |

## Resolved during architect re-entry (dispatch, 2026-04-16)

| # | Title | Severity | Resolution |
|---|---|---|---|
| D-ADV-ARCH-01 | Rollback semantics on multi-node allocations undefined | Critical | DEC-DISP-07: RollbackDispatch is all-or-nothing — releases ALL assigned nodes atomically in one Raft command. Command signature changed to take `released_nodes: Vec<NodeId>`. |
| D-ADV-ARCH-02 | Orphaned Workload Process after rollback race | Critical | DEC-DISP-08: Dispatcher fires best-effort StopAllocation to every released node that had accepted. Counter `lattice_dispatch_rollback_stop_sent_total`. INV-D9 orphan-cleanup backstops. New FM-D11 documents the race and recovery. |
| D-ADV-ARCH-03 | AllocationManager.completion_buffer thread-safety unspecified | Critical | DEC-DISP-09: AllocationManager is actor-owned via `mpsc::Sender<AllocationManagerCmd>`. All methods routed through the channel; no locks; no data races by construction. |
| D-ADV-ARCH-05 | `reattach_in_progress` flag location ambiguous | High | DEC-DISP-10: flag is Raft-committed on the Node record, updated via extended `Command::RecordHeartbeat`. Every heartbeat commits the current value. Scheduler reads from GlobalState unambiguously. |
| D-ADV-ARCH-08 | Multi-node allocation phase aggregation undefined | High | DEC-DISP-11: conservative aggregation — Staging if any LocalAllocation in Prologue; Running only when ALL past Prologue; Completed only when ALL exit 0; Failed if ANY Failed or any non-zero exit. Matches Slurm + MPI semantics. New scenario added. |

## Open findings (dispatch-analyst-review.md — 2026-04-16)

All findings from the dispatch analyst-review have been addressed in spec (Critical, Medium-09, Medium-11) or resolved during architect phase (Highs, Medium-10/12, Lows). The architect interface spec `specs/architecture/interfaces/allocation-dispatch.md` encodes each resolution; post-architect adversary pass (gate 1) will attack the architect's design choices, not the original analyst findings.

## Resolved during architect phase (dispatch, 2026-04-16)

| # | Title | Severity | Resolution |
|---|---|---|---|
| D-ADV-03 | Cluster-wide image corruption causes cluster-wide deadlock | High | DEC-DISP-01: auto-recovery probe every `degraded_probe_interval` + cluster-wide ratio guard that suspends INV-D11 auto-transition when systemic; operators get alarm rather than cluster-dark. |
| D-ADV-04 | Retry counter location is undefined; leak path exists | High | DEC-DISP-02: both `allocation.dispatch_retry_count` and `node.consecutive_dispatch_failures` are Raft-committed fields, survive crashes. |
| D-ADV-05 | INV-D5 joint invariant permits a reachable contradiction | High | DEC-DISP-03: agent emits early heartbeat with `reattach_in_progress: bool`; silent-sweep skips allocations on reattaching nodes. Bounded by `reattach_grace_period` (5 min default). |
| D-ADV-06 | Two-instance Dispatcher multiplies retry budget | High | DEC-DISP-04: single-leader Dispatcher via Raft leadership. Followers observe but don't act. No coordination needed. |
| D-ADV-07 | `accepted: false` RunAllocation response has no semantics | High | DEC-DISP-05: `refusal_reason` enum with four variants (Busy/UnsupportedCapability/MalformedRequest/AlreadyRunning), each with defined retry semantics. |
| D-ADV-08 | Agent can self-claim arbitrary agent_address | High | DEC-DISP-06: INV-D14 added — registered `agent_address` must appear as a SAN in the agent's workload cert; mTLS middleware extracts, RegisterNode/UpdateNodeAddress validate. |
| D-ADV-10 | Dispatcher vs Reconciler race has undefined winner | Medium | Silent-sweep gates on "no heartbeat within grace window" which is naturally disjoint from in-flight dispatch attempts (heartbeats arrive during dispatch). Residual races resolved by `expected_state_version` in both paths. |
| D-ADV-12 | Dispatch traffic rate has no limit | Medium | DispatcherConfig adds `max_concurrent_attempts` (default 64) and `per_agent_concurrency` (default 8). Assumption A-D9 extended. |
| D-ADV-13 | Bare-Process runtime inherits agent secrets by design | Low | BareProcessRuntime contract: explicit env block-list (`LATTICE_*`, `VAULT_*`, `*_SECRET*`, `*_TOKEN*`, `*_KEY*`), allow-list (PATH, HOME, USER, LANG, TZ, SLURM_*, LATTICE_ALLOC_ID, LATTICE_JOB_NAME, LATTICE_NODELIST, LATTICE_NNODES). |
| D-ADV-14 | Orphan cleanup trusts cgroup presence as "scope has live process" | Low | INV-D9 enforcement clarified in architect spec: read scope PID list first, terminate only if non-empty, then remove scope. Empty-scope case is a silent no-op. |

## Recently resolved (dispatch analyst-review — 2026-04-16)

| # | Title | Severity | Resolution |
|---|---|---|---|
| D-ADV-01 | Completion Report source-authentication is missing | Critical | INV-D12 added: reports from nodes not in `assigned_nodes` are rejected at Raft apply-time with `lattice_completion_report_cross_node_total` counter. |
| D-ADV-02 | Final-state report preservation lacks a mechanism | Critical | INV-D13 added: buffer is keyed by `allocation_id` with latest-phase-wins; monotonicity of phases guarantees terminal reports are never overwritten. A-D19 downgraded from `[CRITICAL] unknown` to validated. |
| D-ADV-09 | Idempotency check at submit-time vs apply-time | Medium | INV-D4 enforcement clause updated: idempotency is evaluated at Raft command-apply step, explicitly not at submit-time. |
| D-ADV-11 | Phase-regression anomaly is a log line, not a metric | Medium | INV-D7 enforcement clause updated: anomaly emits `lattice_completion_report_phase_regression_total` counter in addition to the log line. |

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

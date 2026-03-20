# Fidelity: Node Agent Lifecycle (Chunk 3)
Assessed: 2026-03-20

## Test Inventory

| Module | Tests | THOROUGH | MODERATE | SHALLOW | Key Gaps |
|--------|-------|----------|----------|---------|----------|
| agent.rs | 18 | 18 | 0 | 0 | flush_buffered is stub; quorum reconnect untested |
| allocation_runner.rs | 11 | 11 | 0 | 0 | No concurrent multi-alloc isolation test |
| prologue.rs | 20 | 20 | 0 | 0 | Real uenv mount namespace not tested |
| epilogue.rs | 13 | 13 | 0 | 0 | Cascade failure ordering not tested |
| health.rs | 9 | 9 | 0 | 0 | No memory/disk checks |
| heartbeat.rs + loop | 20 | 12 | 8 | 0 | owner_version never set/tested |
| liveness.rs | 20 | 20 | 0 | 0 | No HTTP redirect/malformed response |
| probe_manager.rs | 8 | 0 | 8 | 0 | No concurrent multi-probe test |
| checkpoint_handler.rs | 6 | 0 | 6 | 0 | No timeout; no partial failure |
| runtime/mock.rs | 20 | 20 | 0 | 0 | State machine enforcement solid |
| runtime/uenv.rs | 4 | 0 | 0 | 4 | Real binary not invoked |
| runtime/sarus.rs | 2 | 0 | 0 | 2 | Real container lifecycle not tested |
| runtime/dmtcp.rs | 3 | 0 | 0 | 3 | Real checkpoint/restore not tested |
| data_stage.rs | 8 | 0 | 8 | 0 | Partial staging + rollback untested |
| cgroup.rs | 6 | 0 | 6 | 0 | Real cgroup enforcement not verified |
| network.rs (VNI) | 16 | 12 | 4 | 0 | No Slingshot topology simulation |
| conformance.rs | 4 | 0 | 4 | 0 | Fingerprint collision handling |
| image_cache.rs | 5 | 5 | 0 | 0 | No persistence across restart |
| integration.rs | 21 | 15 | 6 | 0 | Good e2e coverage |
| **TOTAL** | **~214** | **~155** | **~50** | **~9** | |

## Lifecycle Path Coverage

| Path | Tested | Depth |
|------|--------|-------|
| Prologue -> Running -> Epilogue -> Completed | Yes | THOROUGH |
| Prologue failure -> Failed | Yes | THOROUGH |
| Running -> Checkpoint signal -> Suspend | Yes | THOROUGH |
| Running -> Liveness probe fails -> Failed | Yes | MODERATE |
| Epilogue S3 flush fails -> Continue | Yes | MODERATE |
| Epilogue GPU wipe (sensitive) | Yes | MODERATE |
| Agent heartbeat -> Quorum unreachable -> Buffer | Yes | MODERATE (flush is stub) |
| Agent run loop: commands + probes + heartbeats | Yes | THOROUGH |
| Prologue partial failure -> Cleanup partial state | No | MISSING |
| Checkpoint during epilogue | No | MISSING |
| Drain -> Existing allocs complete, new rejected | Partial | SHALLOW |
| Health degradation -> Auto-drain | No | MISSING |

## Runtime Mock Comparison

| Method | MockRuntime | Real (uenv/sarus) | Divergence |
|--------|-------------|-------------------|-----------|
| prepare() | Records call, creates state | Calls real binaries, mounts SquashFS | Mock skips filesystem ops |
| spawn() | Returns mock PID | fork/exec with real PID | Mock has no real process |
| signal() | Checks stopped flag | kill syscall with PID | Mock skips process liveness check |
| stop() | Always Signal(15) | SIGTERM -> grace -> SIGKILL | Mock skips grace period timing |
| wait() | Returns configured exit | waitpid() | Mock no EINTR/zombie handling |
| cleanup() | Removes state, always ok | Unmount, remove dirs | Mock never fails cleanup |

**Rating**: MockRuntime is FAITHFUL for state tracking but DIVERGENT for real OS interaction. Acceptable since real runtimes require Linux + tools.

## Critical Gaps

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| G12 | flush_buffered() is a stub — quorum reconnect replay never exercised | High | agent.rs:224-228 |
| G13 | Real UenvRuntime/SarusRuntime have <10 tests total | High | runtime/uenv.rs, sarus.rs |
| G14 | CgroupManager stubbed — real resource limits not verified | Medium | cgroup.rs |
| G15 | owner_version never set in heartbeat (ADR-06 gap) | Medium | heartbeat.rs |
| G16 | Prologue partial failure cleanup not tested | Medium | prologue.rs |
| G17 | Epilogue cascade failure ordering not tested | Low | epilogue.rs |
| G18 | Concurrent multi-alloc (>2) not tested | Low | agent.rs |
| G19 | Drain node full workflow not end-to-end tested | Low | agent.rs |

## Confidence: MODERATE-HIGH

Strong unit test coverage in happy path (155 THOROUGH). Core lifecycle well-validated through MockRuntime. Gaps are primarily in real OS integration (uenv mount, cgroups, process lifecycle) and edge cases (partial failures, concurrency). Production hardening needed for real runtimes.

# Testing Strategy

## Design Principle

Scheduler correctness is non-negotiable. The testing strategy covers four levels: unit tests for individual functions, integration tests for component interactions, simulation tests for scheduling behavior, and chaos tests for fault tolerance. Every level must pass before a release.

## Test Levels

```
┌─────────────────────────────────────────────────┐
│ Level 4: Chaos Tests (fault injection)          │
│   Raft leader loss, network partitions,         │
│   node failures, storage unavailability         │
├─────────────────────────────────────────────────┤
│ Level 3: Simulation (RM-Replay)                 │
│   Production workload replay, weight tuning,    │
│   fairness validation, SLO compliance           │
├─────────────────────────────────────────────────┤
│ Level 2: Integration Tests                      │
│   Multi-component scenarios, API contracts,     │
│   end-to-end allocation lifecycle               │
├─────────────────────────────────────────────────┤
│ Level 1: Unit Tests                             │
│   Cost function, topology solver, state machine,│
│   protobuf serialization, error handling        │
└─────────────────────────────────────────────────┘
```

## Level 1: Unit Tests

In-module tests (`#[cfg(test)]`), run via `cargo test`.

### Critical Paths

| Crate | What to Test | Example |
|-------|-------------|---------|
| `lattice-scheduler` | Cost function components (f₁-f₉) | Given inputs, verify score output |
| `lattice-scheduler` | Knapsack solver | Given nodes and allocations, verify placement |
| `lattice-scheduler` | Topology packing | Given groups and node count, verify group selection |
| `lattice-scheduler` | Conformance group selection | Given fingerprints, verify grouping |
| `lattice-quorum` | Raft proposal validation | Hard quota rejection, ownership conflict |
| `lattice-quorum` | State machine transitions | Node state changes, allocation lifecycle |
| `lattice-common` | Type serialization/deserialization | Protobuf round-trip for all types |
| `lattice-common` | Allocation state machine | Valid and invalid state transitions |
| `lattice-api` | Request validation | Reject invalid allocations (cycles in DAG, bad constraints) |
| `lattice-api` | SBATCH directive parsing | Translate Slurm directives to Intent API |
| `lattice-checkpoint` | Cost model evaluation | Given metrics, verify checkpoint decision |
| `lattice-cli` | Argument parsing | Flag combinations, error messages |

### Property-Based Tests

Use `proptest` for property-based testing of the cost function and solver:

- **Cost function monotonicity:** Increasing wait time always increases f₂
- **Fair share bounds:** f₃ always in [0, 1]
- **Solver validity:** Every placement returned by the solver satisfies all constraints
- **Topology packing:** Solver never spans more groups than necessary
- **State machine:** No invalid state transitions accepted

## Level 2: Integration Tests

In `tests/` directories, using real components with mock external dependencies.

### Test Harness

A test harness that spins up:
- In-memory Raft cluster (3 members, using `openraft` test utilities)
- Mock node agents (report capabilities, respond to heartbeats)
- Mock VAST API (storage queries return configurable responses)
- Real scheduler instances
- Real API server (in-process)

### Scenarios

| Scenario | What It Tests |
|----------|--------------|
| **Submit → Schedule → Complete** | Full allocation lifecycle through all components |
| **DAG submission** | Multi-allocation workflow with dependency resolution |
| **Preemption** | Higher-priority allocation preempts lower-priority |
| **Elastic borrowing** | vCluster borrows and returns nodes |
| **Quota rejection** | Hard quota exceeded → proposal rejected |
| **Medical claim** | Node claim, audit logging, wipe on release |
| **Session lifecycle** | Session create → terminal → disconnect → cleanup |
| **Rolling upgrade simulation** | Mixed-version node agents, protocol negotiation |
| **Conformance drift** | Node fingerprint changes → scheduling impact |
| **Reactive scaling** | Metric threshold triggers scale-up/down |

### API Contract Tests

For every API endpoint, test:
- Valid request → expected response
- Invalid request → appropriate error code and message
- Authorization: user sees own allocations only, tenant-admin sees tenant, system-admin sees all
- Rate limiting: exceeded rate → 429 with Retry-After header

### Protobuf Compatibility

Test backward compatibility:
- Deserialize messages from previous version with new code (additive fields)
- Deserialize messages from new version with old code (unknown fields ignored)

## Level 3: Simulation (RM-Replay)

### Purpose

RM-Replay replays production workload traces through the scheduler to validate scheduling behavior without risking production. Essential for:
- Tuning cost function weights before deployment
- Validating fairness across tenants
- Regression testing after scheduler changes

### Workflow

```
1. Capture: Record production workload traces
   - Allocation submissions (arrival time, resources, constraints, tenant)
   - Allocation completions (duration, exit status)
   - Node inventory (capabilities, topology)

2. Configure: Set cost function weights and vCluster policies

3. Replay: Feed traces through lattice-scheduler in simulation mode
   - No real nodes or quorum — mock environment
   - Simulated time (runs in seconds, not hours)
   - Deterministic (same trace + same weights = same result)

4. Evaluate: Measure scheduling outcomes
   - Utilization: fraction of GPU-hours used
   - Wait time: p50, p95, p99 queue wait per priority class
   - Fairness: actual share vs. target share per tenant (Jain's fairness index)
   - Backfill effectiveness: percentage of idle slots filled
   - SLO compliance: percentage of allocations meeting target wait time
   - Preemption rate: preemptions per hour

5. Iterate: Adjust weights, re-run, compare
```

### Regression Suite

Maintain a library of representative workload traces:

| Trace | Description | Key Metric |
|-------|-------------|------------|
| `steady-state.trace` | Normal mixed workload (HPC + ML + services) | Utilization > 85% |
| `burst.trace` | Sudden spike in submissions | No starvation (p99 wait < 4h) |
| `unfair.trace` | One tenant submits heavily | Fair share deviation < 10% |
| `medical-claim.trace` | Medical claims interleaved with HPC | Medical wait = 0 (immediate) |
| `preemption-heavy.trace` | Many priority inversions | Checkpoint success rate > 95% |
| `empty-to-full.trace` | Cluster goes from idle to full | Ramp-up time, scheduling cycle latency |

Each trace has a pass/fail threshold for key metrics. CI runs the regression suite on every scheduler change.

## Level 4: Chaos Tests

Fault injection tests that validate the failure modes documented in [failure-modes.md](failure-modes.md).

### Fault Injection Framework

Use a test harness that can inject faults at configurable times:

| Fault | Injection Method | Validates |
|-------|-----------------|-----------|
| Raft leader kill | Stop leader process | Leader election, in-flight proposal retry |
| Raft member kill | Stop follower process | Continued operation with minority loss |
| Network partition (node↔quorum) | Drop heartbeats | Degraded → Down transition, allocation requeue |
| Network partition (quorum split) | Partition Raft members | Minority stalls, majority continues |
| Node agent crash | Kill agent process | Heartbeat timeout, allocation requeue |
| Storage unavailability | Mock VAST returns errors | Staging pauses, running allocations continue |
| Checkpoint timeout | Application ignores checkpoint hint | Forced preemption after timeout |
| API server crash | Kill API server | Client retry, no state loss |
| Quorum snapshot corruption | Corrupt snapshot file | Recovery from previous valid snapshot |

### Chaos Test Scenarios

| Scenario | Steps | Expected Outcome |
|----------|-------|------------------|
| **Leader election under load** | Submit 50 allocations, kill leader mid-cycle | New leader elected < 5s, no proposals lost, all allocations eventually scheduled |
| **Node failure with requeue** | Start 10 allocations, kill 2 node agents | Allocations requeued, rescheduled on healthy nodes, total delay < 2 min |
| **Split-brain prevention** | Partition 3-member quorum into 1+2 | Minority (1) cannot commit, majority (2) continues, no divergent state |
| **Cascade failure** | Kill 3 node agents simultaneously | Allocations on all 3 nodes requeued, scheduling continues for remaining nodes |
| **Medical node failure** | Kill medical node agent | Extended grace period, operator alert, no auto-requeue |
| **Recovery from full quorum loss** | Kill all quorum members, restore from snapshot | State restored, node agents reconnect, scheduling resumes |

### Execution

Chaos tests run in CI on a dedicated stage (not on every commit):
- Nightly: full chaos suite
- On release branch: full chaos suite must pass

## Performance Benchmarks

### Scheduling Cycle Latency

| Benchmark | Configuration | Target |
|-----------|--------------|--------|
| 100 pending allocations, 1000 nodes | HPC backfill | Cycle < 5s |
| 500 pending allocations, 5000 nodes | HPC backfill | Cycle < 15s |
| 1000 pending allocations, 10000 nodes | HPC backfill | Cycle < 30s |
| Raft commit (single proposal) | 3-member quorum | p99 < 50ms |
| Raft commit (single proposal) | 5-member quorum | p99 < 100ms |

### Load Tests

| Test | Description | Target |
|------|-------------|--------|
| API throughput | Concurrent submission requests | > 1000 req/s |
| Heartbeat load | 10000 node agents reporting | < 1% CPU on quorum |
| Log streaming | 100 concurrent log streams | < 5% CPU on API server |

## CI Pipeline

```
On every commit:
  cargo fmt --check
  cargo clippy --all-targets
  cargo test (Level 1: unit tests)

On every PR:
  Level 1 + Level 2 (integration tests)
  Protobuf backward compatibility check

Nightly:
  Level 1 + Level 2 + Level 3 (RM-Replay regression) + Level 4 (chaos)
  Performance benchmarks (track regressions)

On release:
  All levels must pass
  Performance benchmarks must meet targets
```

## Cross-References

- [failure-modes.md](failure-modes.md) — Failure scenarios validated by chaos tests
- [scheduling-algorithm.md](scheduling-algorithm.md) — Cost function tested by unit tests and RM-Replay
- [upgrades.md](upgrades.md) — Rolling upgrade validated by integration tests
- [conformance.md](conformance.md) — Conformance behavior validated by integration tests

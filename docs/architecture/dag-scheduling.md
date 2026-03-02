# DAG Scheduling

## Design Principle

DAGs are first-class workflow primitives. The scheduler resolves dependencies; users declare intent. Dependency semantics are Slurm-compatible (afterok, afternotok, afterany, aftercorr) to ease migration.

## DAG Submission

A DAG is a set of allocation specs with dependency edges, submitted as a single unit via the Intent API:

```
DagSpec {
    allocations: Map<String, AllocationSpec>,  // ref_id → spec
    dependencies: Vec<DependencyEdge>,
}

DependencyEdge {
    from: String,      // ref_id of predecessor
    to: String,        // ref_id of successor
    condition: DependencyCondition,
}
```

The `DagSpec` is already defined in the protobuf API (`proto/lattice/v1/allocations.proto`).

## Dependency Conditions

Defined in `crates/lattice-common/src/types.rs` (`DependencyCondition` enum):

| Condition | Slurm Equivalent | Semantics |
|-----------|-------------------|-----------|
| `Success` | afterok | Successor runs only if predecessor exits 0 |
| `Failure` | afternotok | Successor runs only if predecessor exits non-zero |
| `Any` | afterany | Successor runs regardless of predecessor's exit status |
| `Corresponding` | aftercorr | Task group: array element N depends on predecessor's element N |
| `Mutex` | singleton | Only one allocation with this mutex name runs at a time |

## DAG Lifecycle

### 1. Submission and Validation

- User submits `DagSpec` via `POST /v1/dags` or `lattice dag submit`
- lattice-api validates the graph:
  - No cycles (topological sort must succeed)
  - All `from`/`to` ref_ids reference allocations in the DAG
  - All allocation specs individually valid
- DAG receives a unique `dag_id`
- Individual allocations receive `allocation_id` values and are tagged with `dag_id`

### 2. Root Node Scheduling

- Allocations with no incoming dependency edges (root nodes) enter their vCluster scheduler queue immediately
- Root nodes are scored and scheduled like any other allocation

### 3. Dependency Resolution

- When an allocation completes (any terminal state), the system evaluates outgoing edges:
  - For each outgoing edge, check if the condition is satisfied
  - If all incoming edges to a successor are satisfied, the successor enters the scheduler queue
- Dependency resolution is eventually consistent (handled by lattice-api or a lightweight DAG controller, not the quorum)

### 4. DAG Completion

- DAG completes when all allocations reach a terminal state (Completed, Failed, or Cancelled)
- DAG state: `Running` while any allocation is pending or running, `Completed` when all are done, `Failed` if any required allocation failed without a catching edge

### 5. DAG Cancellation

- `DELETE /v1/dags/{id}` or `lattice dag cancel {id}`
- Cancels all pending and running allocations in the DAG
- Running allocations receive SIGTERM → grace period → SIGKILL (same as walltime exceeded)

## Failure Propagation

### Default: Success Dependencies

If allocation A fails and B depends on A via `Success`:
- B is cancelled (dependency can never be satisfied)
- B's downstream dependencies are also evaluated (cascading cancellation)

### Error Handling Paths

With `Failure` edges, users can build error-handling workflows:
```
  train ──Success──→ evaluate ──Success──→ deploy
    │                   │
    └──Failure──→ notify_failure
                        │
    └──Failure──→ notify_failure
```

- `notify_failure` runs only if `train` or `evaluate` fails
- `deploy` runs only if both `train` and `evaluate` succeed

### Any Dependencies

With `Any` edges, successors run regardless:
```
  run_experiment ──Any──→ cleanup
```

`cleanup` runs whether `run_experiment` succeeds or fails. Useful for teardown tasks.

### Corresponding Dependencies (Task Groups)

For task groups (array jobs), `Corresponding` creates element-wise dependencies:
```
  preprocess[0..N] ──Corresponding──→ train[0..N]
```

`train[i]` starts only when `preprocess[i]` completes successfully. Other array elements are independent.

## State Tracking

DAG state is eventually consistent, following ADR-004:

- The **quorum** tracks individual allocation states (ownership, terminal states). It does not know about DAG structure.
- The **DAG controller** (runs within lattice-api) evaluates dependency edges when allocation state changes. It reads allocation states from the quorum and determines which successors to release into the scheduler queue.
- This separation keeps the quorum simple and avoids adding DAG-specific logic to the Raft state machine.

## DAG Queries

| Endpoint | Description |
|----------|-------------|
| `GET /v1/dags/{id}` | DAG status: overall state, per-allocation states |
| `GET /v1/dags/{id}/graph` | DAG structure: allocations and edges |
| `GET /v1/dags?tenant={id}` | List DAGs for a tenant |
| `DELETE /v1/dags/{id}` | Cancel DAG |

CLI equivalents: `lattice dag status`, `lattice dag list`, `lattice dag cancel`.

## Cross-References

- [api-design.md](api-design.md) — DagSpec in protobuf definition
- [scheduling-algorithm.md](scheduling-algorithm.md) — DAG members are scored individually by the knapsack solver
- [failure-modes.md](failure-modes.md) — Allocation-level failure recovery interacts with DAG propagation
- types.rs — `Dependency`, `DependencyCondition` enum definitions

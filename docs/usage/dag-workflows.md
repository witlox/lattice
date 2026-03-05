# DAG Workflows

DAGs (Directed Acyclic Graphs) let you define multi-step pipelines where allocations depend on each other.

## YAML Definition

```yaml
# workflow.yaml
name: training-pipeline
allocations:
  - name: preprocess
    entrypoint: "python preprocess.py"
    nodes: 2
    walltime: "2h"

  - name: train
    entrypoint: "torchrun train.py"
    nodes: 64
    walltime: "72h"
    uenv: "prgenv-gnu/24.11:v1"
    depends_on:
      - preprocess: success

  - name: evaluate
    entrypoint: "python eval.py"
    nodes: 1
    walltime: "1h"
    depends_on:
      - train: success

  - name: notify-failure
    entrypoint: "python notify.py --status=failed"
    nodes: 1
    walltime: "10m"
    depends_on:
      - train: failure
```

## Submitting a DAG

```bash
lattice dag submit workflow.yaml
# Submitted DAG d1e2f3g4 with 4 allocations
```

## Dependency Conditions

| Condition | Meaning |
|-----------|---------|
| `success` | Run after dependency completes successfully |
| `failure` | Run after dependency fails |
| `any` | Run after dependency completes (success or failure) |
| `corresponding` | For task groups: task N depends on task N of the parent |

## Monitoring DAGs

```bash
# DAG status overview
lattice dag status d1e2f3g4

# Detailed graph view
lattice dag status d1e2f3g4 --graph

# Output:
# preprocess [Completed] → train [Running] → evaluate [Pending]
#                                          ↘ notify-failure [Pending]
```

## Cancelling a DAG

```bash
# Cancel all allocations in the DAG
lattice dag cancel d1e2f3g4
```

Cancellation cascades — downstream allocations that haven't started are cancelled automatically.

## Failure Propagation

- If a `success` dependency fails, downstream allocations are cancelled
- If a `failure` dependency succeeds, those downstream allocations are skipped
- `any` dependencies always run regardless of upstream outcome

## Limits

- Maximum 1000 allocations per DAG (configurable by admin)
- Cycles are rejected at submission time
- Duplicate allocation names within a DAG are rejected

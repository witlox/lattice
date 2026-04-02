# Operational Validation — Level 3.5 Architecture

## Purpose

ReFrame-inspired workload test suite that submits real workloads to a deployed
lattice cluster and validates scheduling, lifecycle, and recovery behavior.
Fills the gap between Level 2 (in-process integration) and Level 4 (chaos/fault
injection) in the testing strategy.

## Prerequisites

### REST API Extension

The REST `SubmitRequest` must be extended to support the full allocation spec.
Fields to add:

| Field | Type | Default | Maps to |
|---|---|---|---|
| `lifecycle` | `string` | `"bounded"` | `LifecycleSpec.type` |
| `requeue_policy` | `string` | `"never"` | `AllocationSpec.requeue_policy` |
| `max_requeue` | `u32` | `0` | `AllocationSpec.max_requeue` |
| `image` | `string?` | `null` | `EnvironmentSpec.images[0]` (OCI) |
| `priority_class` | `string?` | `"normal"` | `AllocationSpec.priority_class` |
| `sensitive` | `bool?` | `false` | `AllocationSpec.sensitive` |
| `cpus` | `f64?` | `null` | `ResourceSpec.cpus_per_node` |
| `gpus` | `u32?` | `null` | `ResourceSpec.gpus_per_node` |
| `memory_gb` | `f64?` | `null` | `ResourceSpec.memory_gb_per_node` |
| `labels` | `map<string,string>?` | `null` | `AllocationSpec.labels` |

### Python SDK Alignment

Fix `AllocationSpec.to_dict()` to use `tenant` (not `tenant_id`) and align
payload shape with the extended REST schema. Add fields for lifecycle,
requeue_policy, max_requeue, labels.

## Invariants

**INV-OV1: Test isolation** — Each test leaves the cluster in the state it
found it. No leaked allocations, no drained nodes left drained. Cleanup runs
unconditionally (even on failure).

**INV-OV2: Ordering independence** — Tests within a suite may run in any order.
No test depends on side effects from another test.

**INV-OV3: Environment abstraction** — No test contains docker-compose or
GCP-specific logic. TestCluster provides: API endpoint, compute node count,
available images, SSH access (GCP only). Tests declare requirements via tags;
the harness skips unsatisfied tests.

**INV-OV4: Timeout contract** — Every allocation has an explicit timeout. No
test hangs. Total suite <= 60 minutes.

**INV-OV5: Observation via API** — Tests assert on cluster state via REST API.
Stdout extraction is opt-in for performance baselines.

**INV-OV6: Image precondition** — Before workload tests, the harness verifies
all required images are pushed. If push fails, the entire suite is skipped.

**INV-OV7: Tenant strategy** — Default tenant `lattice-ov` for general tests.
Multi-tenant tests (quota, fairness, sensitive) create purpose-specific tenants
and clean them up.

**INV-OV8: Concurrency model** — Tests tagged `@parallel` share the cluster
concurrently. Tests tagged `@sequential` run alone. The harness enforces this.

**INV-OV9: Collect-all execution** — Suite continues on test failure. Final
report aggregates all results. Exit code = number of failures.

## Test Cluster Abstraction

```python
class TestCluster(ABC):
    """Environment-agnostic cluster interface."""
    api_url: str
    token: str
    compute_node_count: int
    registry_url: Optional[str]
    has_gpu: bool

    async def push_image(self, local_tag: str, remote_tag: str) -> bool
    async def restart_agent(self, node_index: int) -> None  # GCP only
    async def kill_agent(self, node_index: int) -> None      # GCP only
    async def health_check(self) -> bool

class DockerCluster(TestCluster):
    """docker-compose E2E stack.
    Requires registry:2 service in docker-compose (ADV-OV-7).
    """

class GcpCluster(TestCluster):
    """Terraform-provisioned GCP cluster."""
```

## Test Image Library

| Image | Base | Contents | Purpose |
|---|---|---|---|
| `lattice-test/sleeper` | `alpine` | `sleep` + configurable duration via env | Basic lifecycle |
| `lattice-test/crasher` | `alpine` | Exits with configurable code/signal after delay | Failure handling, requeue |
| `lattice-test/service-http` | `python:3.12-slim` | HTTP server on :8080 with /healthz | Service workloads, liveness |
| `lattice-test/checkpoint` | `python:3.12-slim` | Writes state, handles SIGUSR1 | Checkpoint/preemption |
| `lattice-test/numpy-bench` | `python:3.12-slim` | NumPy matmul, prints GFLOPS | Scientific compute baseline |
| `lattice-test/pytorch-mnist` | `python:3.12-slim` | PyTorch CPU MNIST (~30s) | ML training baseline |
| `lattice-test/mpi-hello` | `ubuntu + openmpi` | MPI hello-world | Multi-node (future, GCP) |
| `lattice-test/data-consumer` | `alpine` | Reads staged file, validates | Data staging (future) |

Images are built once and pushed to the cluster registry during test setup.
Dockerfiles live in `tests/ov/images/`.

## Suite Structure

```
tests/ov/
├── conftest.py              # Cluster fixtures, image push, cleanup
├── images/
│   ├── sleeper/Dockerfile
│   ├── crasher/Dockerfile
│   ├── service-http/Dockerfile
│   ├── checkpoint/Dockerfile
│   ├── numpy-bench/Dockerfile
│   └── pytorch-mnist/Dockerfile
├── test_lifecycle.py        # Suite 1: allocation lifecycle
├── test_services.py         # Suite 2: service workloads
├── test_quota.py            # Suite 3: quota & fairness
├── test_drain.py            # Suite 4: drain under load
├── test_preemption.py       # Suite 5: preemption
├── test_dag.py              # Suite 6: DAG workflows
├── test_recovery.py         # Suite 7: agent recovery (GCP only)
└── test_performance.py      # Suite 8: performance baselines
```

## Execution

```bash
# Docker-compose (local)
pytest tests/ov/ -m "docker" --cluster=docker

# GCP (deployed cluster)
pytest tests/ov/ -m "gcp" --cluster=gcp --api-url=http://... --token=...

# Specific suite
pytest tests/ov/test_services.py --cluster=docker

# Parallel-safe tests only
pytest tests/ov/ -m "parallel" --cluster=docker -n auto
```

## Tagging & Cleanup (ADV-OV-2)

Each test run generates a unique ID (`ov-run-{uuid4[:8]}`). All allocations
are submitted with `labels: {"ov-run": "<run-id>", "ov-test": "<test-name>"}`.

Per-test cleanup (function-scoped): cancels allocations matching this test's
`ov-test` label only — safe for parallel execution.

Session cleanup (session-scoped finalizer, safety net):
1. Cancel ALL allocations with the run label
2. Undrain any drained nodes
3. Delete purpose-specific tenants
4. Wait for cancellations to complete

## Cross-Context Dependencies

| Dependency | Required by | Skip behavior |
|---|---|---|
| Extended REST API | All suites | Suite fails (hard requirement) |
| Container registry | Image-based tests | Skip image tests, run bare-process tests |
| Docker-compose registry | Docker cluster images | Add `registry:2` to compose stack (ADV-OV-7) |
| SSH/gcloud access | Suite 7 (recovery) | Skip recovery suite |
| Multiple compute nodes | Suite 3, 4, 5 | Skip multi-node scenarios |
| REST extension unit tests | OV suite trust | Must pass in CI before OV runs (ADV-OV-5) |

## Timing Budget (60 min max)

| Suite | Est. | Mode | Env |
|---|---|---|---|
| Image build + push | 5 min | setup | both |
| 1. Lifecycle | 5 min | parallel | both |
| 2. Services | 3 min | parallel | both |
| 3. Quota | 5 min | sequential | both |
| 4. Drain | 3 min | sequential | both |
| 5. Preemption | 3 min | sequential | both |
| 6. DAG | 4 min | parallel | both |
| 7. Recovery | 3 min | sequential | GCP |
| 8. Performance | 4 min | parallel | both |
| Cleanup | 2 min | teardown | both |
| **Total** | **~37 min** | | |

## Fidelity Gaps Addressed

| Gap | Suite | How |
|---|---|---|
| service_workloads LOW | Suite 2 | Real unbounded allocations with requeue |
| G69/G70 quota races | Suite 3 | Concurrent multi-tenant submission |
| G12 reconnect replay | Suite 7 | Agent restart with live workloads |
| G-NEW-10/11 topology | Suite 3/5 | Placement validation under contention |
| G35 DataStager | Future Suite | data-consumer image (not in v1) |
| G61 EventBus | Future | Watch stream validation (not in v1) |

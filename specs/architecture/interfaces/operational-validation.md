# Operational Validation — Interfaces

## Module Map

```
[1] REST API Extension (crates/lattice-api/src/rest.rs)
     ↓ extends SubmitRequest, maps to AllocationSpec proto
[2] Python SDK Fix (sdk/python/lattice_sdk/)
     ↓ uses extended REST API
[3] Test Images (tests/ov/images/)
     ↓ built and pushed by harness
[4] Test Harness (tests/ov/conftest.py)
     ↓ provides cluster abstraction, fixtures
[5] Test Suites (tests/ov/test_*.py)
     ↓ use harness + SDK to validate behavior
```

No cycles. Each module depends only on the one above it.

## Module 1: REST API Extension

### Interface Change: SubmitRequest

```rust
// crates/lattice-api/src/rest.rs
#[derive(Deserialize)]
pub struct SubmitRequest {
    // Existing
    pub tenant: String,
    pub project: Option<String>,
    pub vcluster: Option<String>,
    pub entrypoint: String,
    pub nodes: Option<u32>,
    pub walltime_hours: Option<f64>,

    // New — lifecycle
    #[serde(default = "default_lifecycle")]
    pub lifecycle: String,               // "bounded" | "unbounded" | "reactive"
    #[serde(default)]
    pub requeue_policy: String,          // "never" | "on_node_failure" | "always"
    #[serde(default)]
    pub max_requeue: u32,

    // New — workload
    pub image: Option<String>,           // OCI image ref
    pub priority_class: Option<String>,  // "low" | "normal" | "high" | "urgent"
    #[serde(default)]
    pub sensitive: bool,

    // New — resources
    pub cpus: Option<f64>,
    pub gpus: Option<u32>,
    pub memory_gb: Option<f64>,

    // New — metadata
    pub labels: Option<HashMap<String, String>>,
}

fn default_lifecycle() -> String { "bounded".to_string() }
```

### Mapping: SubmitRequest → AllocationSpec proto

```
tenant          → AllocationSpec.tenant
project         → AllocationSpec.project
vcluster        → AllocationSpec.vcluster
entrypoint      → AllocationSpec.entrypoint
nodes           → ResourceSpec.min_nodes (default 1)
walltime_hours  → LifecycleSpec.walltime (bounded only)
lifecycle       → LifecycleSpec.type (BOUNDED/UNBOUNDED/REACTIVE)
requeue_policy  → AllocationSpec.requeue_policy
max_requeue     → AllocationSpec.max_requeue
image           → EnvironmentSpec.images[0] { spec, image_type: "oci" }
priority_class  → AllocationSpec.priority_class
sensitive       → AllocationSpec.sensitive
cpus            → ResourceSpec.cpus_per_node
gpus            → ResourceSpec.gpus_per_node
memory_gb       → ResourceSpec.memory_gb_per_node
labels          → AllocationSpec.labels
```

### Validation Rules (in submit_allocation handler)

- `lifecycle` must be one of `"bounded"`, `"unbounded"`, `"reactive"` — return 400 otherwise (ADV-OV-1)
- `requeue_policy` must be one of `"never"`, `"on_node_failure"`, `"always"` — return 400 otherwise (ADV-OV-1)
- If `lifecycle == "unbounded"`, `walltime_hours` is ignored
- If `lifecycle == "bounded"` and `walltime_hours` is None, default 1.0
- `max_requeue` capped at 100 (existing ADV F18 validation)
- `labels` keys must be non-empty, values ≤ 256 chars

### Required Unit Tests (ADV-OV-5)

Module 1 must have unit tests covering:
- Each lifecycle value maps to correct LifecycleSpec.type
- Unknown lifecycle value returns 400
- Unknown requeue_policy value returns 400
- `image` field creates EnvironmentSpec with OCI type
- `labels` pass through to AllocationSpec
- Bounded + walltime, unbounded ignores walltime
- All existing submit tests still pass

### Response: unchanged

```rust
pub struct SubmitResponse {
    pub allocation_id: String,
}
```

## Module 2: Python SDK Fix

### AllocationSpec changes

```python
@dataclass
class AllocationSpec:
    entrypoint: str
    tenant: str                              # was tenant_id → rename
    nodes: int = 1
    walltime_hours: Optional[float] = None
    project: Optional[str] = None
    vcluster: Optional[str] = None

    # Lifecycle
    lifecycle: str = "bounded"               # NEW
    requeue_policy: str = "never"            # NEW
    max_requeue: int = 0                     # NEW

    # Workload
    image: Optional[str] = None              # NEW (replaces complex env building)
    priority_class: str = "normal"
    sensitive: bool = False                  # NEW

    # Resources
    cpus: Optional[float] = None             # NEW
    gpus: Optional[int] = None               # NEW
    memory_gb: Optional[float] = None        # NEW

    # Metadata
    labels: Optional[Dict[str, str]] = None  # NEW

    def to_dict(self) -> Dict[str, Any]:
        """Serialize to REST API JSON shape."""
        # Only include non-None/non-default fields
        ...
```

### Breaking changes

- `tenant_id` → `tenant` (keep `tenant_id` as deprecated property alias) (ADV-OV-3)
- Remove `user_id` (set by server from auth token)
- Remove `network_domain`, `uenv`, `view`, `mounts`, `devices` from SDK
  (these go through gRPC; out of scope for REST)
- Update `examples/python-sdk/` to use new field names

### Allocation response

```python
@dataclass
class Allocation:
    id: str
    tenant: str                # was tenant_id
    project: str
    state: str
    assigned_nodes: List[str]
    user: str
```

## Module 3: Test Images

### Dockerfile Contracts

Each image must:
- Accept configuration via environment variables (not args)
- Exit cleanly on SIGTERM within 5s
- Print structured output to stdout for extraction

| Image | Env Vars | Stdout Contract |
|---|---|---|
| sleeper | `SLEEP_SECONDS` (default 10) | `DONE sleep={N}` |
| crasher | `EXIT_CODE` (default 1), `DELAY_SECONDS` (default 5) | `CRASHING code={N}` |
| service-http | `PORT` (default 8080) | `LISTENING port={N}` on start |
| checkpoint | `STATE_FILE` (default /tmp/ckpt) | `CHECKPOINT saved={path}` on SIGUSR1 |
| numpy-bench | `MATRIX_SIZE` (default 2000) | `RESULT gflops={N}` |
| pytorch-mnist | `EPOCHS` (default 2) | `RESULT accuracy={N}` |

### Build & Push Contract

```python
async def build_and_push_images(cluster: TestCluster) -> Dict[str, str]:
    """Build all images, push to cluster registry, return tag map.

    Returns: {"sleeper": "registry:5000/lattice-test/sleeper:latest", ...}
    Raises: ImageBuildError if any build fails (suite should skip).
    """
```

## Module 4: Test Harness (conftest.py)

### Fixtures

```python
# Session-scoped: one cluster per test run
@pytest.fixture(scope="session")
async def cluster(request) -> AsyncIterator[TestCluster]:
    """Provide cluster abstraction based on --cluster flag."""

# Session-scoped: push images once
@pytest.fixture(scope="session")
async def images(cluster) -> Dict[str, str]:
    """Build and push test images. Skip suite if fails."""

# Session-scoped: SDK client
@pytest.fixture(scope="session")
async def client(cluster) -> AsyncIterator[LatticeClient]:
    """Authenticated SDK client for the cluster."""

# Session-scoped: run ID for cleanup
@pytest.fixture(scope="session")
def run_id() -> str:
    """Unique ID for this test run (for allocation labels)."""

# Function-scoped: per-test allocation tracker (ADV-OV-2)
@pytest.fixture(autouse=True)
async def cleanup_allocations(client, run_id, request):
    """Track and cancel this test's allocations on teardown.

    Uses per-test label {"ov-test": request.node.name} to avoid
    cancelling parallel tests' allocations. Session finalizer
    does a sweep of all run_id allocations as safety net.
    """
    yield
    # Cancel allocations tagged with this test's name
    # Undrain any drained nodes
```

### TestCluster Interface

```python
class TestCluster(ABC):
    api_url: str
    token: str
    compute_node_count: int
    registry_url: Optional[str]
    has_ssh: bool                   # GCP only
    has_gpu: bool

    @abstractmethod
    async def push_image(self, local_tag: str, remote_tag: str) -> bool: ...

    @abstractmethod
    async def restart_agent(self, node_index: int) -> None: ...

    @abstractmethod
    async def kill_agent(self, node_index: int) -> None: ...

    @abstractmethod
    async def health_check(self) -> bool: ...

    @abstractmethod
    async def get_agent_logs(self, node_index: int, lines: int) -> str: ...
```

### Skip Logic

```python
def requires_images(test_func):
    """Skip if image push failed."""

def requires_ssh(test_func):
    """Skip if cluster has no SSH access (docker-compose)."""

def requires_multi_node(test_func):
    """Skip if cluster has < 2 compute nodes."""
```

## Module 5: Test Suites

### Common Test Pattern

```python
async def submit_and_wait(
    client: LatticeClient,
    spec: AllocationSpec,
    target_state: str,
    timeout: float,
    run_id: str,
) -> Allocation:
    """Submit allocation with run label, poll until target state or timeout.

    Raises: TimeoutError if state not reached.
    Returns: Final allocation state.
    """
```

### Suite Tags → pytest markers

```python
# conftest.py
def pytest_configure(config):
    config.addinivalue_line("markers", "parallel: safe to run concurrently")
    config.addinivalue_line("markers", "sequential: must run alone")
    config.addinivalue_line("markers", "docker: runs on docker-compose")
    config.addinivalue_line("markers", "gcp: runs on GCP cluster")
```

## Enforcement Map

| Invariant | Enforcement Point |
|---|---|
| INV-OV1 (isolation) | `cleanup_allocations` fixture (per-test label, session sweep fallback) (ADV-OV-2) |
| INV-OV2 (ordering) | No shared mutable state between tests; each test creates own allocations |
| INV-OV3 (abstraction) | TestCluster ABC; tests only call abstract methods |
| INV-OV4 (timeout) | `submit_and_wait` timeout parameter; pytest-timeout per test |
| INV-OV5 (API observation) | Tests use `client.status()` / `client.list_nodes()`; no SSH for assertions |
| INV-OV6 (image precondition) | `images` fixture is session-scoped; `requires_images` skip decorator |
| INV-OV7 (tenant) | Default tenant in conftest; multi-tenant tests create/cleanup own tenants |
| INV-OV8 (concurrency) | pytest-xdist groups: `@parallel` in one group, `@sequential` serialized |
| INV-OV9 (collect-all) | `pytest --continue-on-collection-errors -p no:faulthandler` |

## Error Taxonomy

| Error | Source | Handling |
|---|---|---|
| `ClusterUnhealthy` | health_check fails | Skip all remaining tests |
| `ImageBuildError` | Docker build fails | Skip image-dependent tests |
| `ImagePushError` | Registry unreachable | Skip image-dependent tests |
| `SubmissionError` | REST API returns 4xx/5xx | Test fails |
| `TimeoutError` | Allocation didn't reach state | Test fails, cleanup runs |
| `SSHError` | gcloud ssh fails | Skip recovery tests |
| `CleanupError` | Cancel/undrain fails | Log warning, don't fail test |

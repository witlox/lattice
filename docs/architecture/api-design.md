# API Design

## Two-Tier API Model

### Tier 1: Intent API (Agent-Native)

Agents and advanced users interact with the Intent API. They declare *what* they need; the scheduler resolves *how*.

#### Core Resources

**Allocation** — The universal work unit.
```
POST   /v1/allocations              Create allocation (or DAG of allocations)
GET    /v1/allocations              List allocations (filterable)
GET    /v1/allocations/{id}         Get allocation status
DELETE /v1/allocations/{id}         Cancel allocation
PATCH  /v1/allocations/{id}         Update allocation (e.g., extend walltime, switch telemetry)
POST   /v1/allocations/{id}/tasks   Launch tasks within an existing allocation (srun equivalent)
POST   /v1/allocations/{id}/checkpoint  Request checkpoint
```

**Cancel semantics (INT-1).** `DELETE /v1/allocations/{id}` is a two-step
operation: the quorum commits the transition to Cancelled, and the API
then fan-outs a best-effort `NodeAgentService.StopAllocation` RPC to
every assigned node (reason `"user_cancelled"`). The HTTP response
returns as soon as the Raft commit succeeds; the StopAllocation
fan-out is asynchronous (non-blocking). Agents treat StopAllocation
idempotently — INV-D3 — so duplicate delivery is safe. Delivery
failures are non-fatal and counted via
`lattice_cancel_stop_sent_total`; the orphan-cleanup path (INV-D9)
backstops any agent that missed the stop.

**Observability** — User-facing debugging and monitoring.
```
POST   /v1/allocations/{id}/attach           Attach interactive terminal (WebSocket upgrade)
GET    /v1/allocations/{id}/logs             Historical logs from S3
GET    /v1/allocations/{id}/logs/stream      Live log tail (SSE / gRPC stream)
GET    /v1/allocations/{id}/metrics          Query metrics snapshot from TSDB
GET    /v1/allocations/{id}/metrics/stream   Push-based live metrics stream
GET    /v1/allocations/{id}/diagnostics      Combined network + storage diagnostics
GET    /v1/allocations/{id}/diagnostics/network  Network-specific diagnostics
GET    /v1/allocations/{id}/diagnostics/storage  Storage-specific diagnostics
GET    /v1/compare                           Cross-allocation metric comparison
```

**DAGs** — Workflow graph management.
```
POST   /v1/dags                    Submit a DAG of allocations
GET    /v1/dags                    List DAGs (filterable by tenant, user, state)
GET    /v1/dags/{id}               Get DAG status (overall state + per-allocation states)
GET    /v1/dags/{id}/graph         Get DAG structure (allocations + dependency edges)
DELETE /v1/dags/{id}               Cancel all allocations in a DAG
```

**Session** — Interactive allocation with WebSocket terminal.
```
POST   /v1/sessions                 Create interactive session
GET    /v1/sessions/{id}/terminal   WebSocket terminal endpoint
```

**Nodes** — Read-only view of cluster state.
```
GET    /v1/nodes                    List nodes (filterable by vCluster, tenant, state)
GET    /v1/nodes/{id}               Get node details
```

**Tenants / vClusters** — Administrative.
```
GET    /v1/tenants                  List tenants
GET    /v1/vclusters                List vClusters
GET    /v1/vclusters/{id}/queue     View vCluster queue
```

**Accounting**
```
GET    /v1/accounting               Query usage history
```

#### Allocation Request Schema

```yaml
# Full Intent API allocation request
allocation:
  # Identity
  tenant: "ml-team"
  project: "gpt-training"
  vcluster: "ml-training"           # optional: scheduler can infer from intent
  tags: { experiment: "run-42" }

  # What to run
  intent: "train"                    # optional hint for scheduler
  environment:
    uenv: "prgenv-gnu/24.11:v1"     # uenv name/version
    view: "default"                  # uenv view to activate
    # OR:
    image: "registry.example.com/my-training:latest"  # OCI image via Sarus
  entrypoint: "torchrun --nproc_per_node=4 train.py"

  # Resources
  resources:
    nodes: 64                        # can be exact or range: { min: 32, max: 128 }
    constraints:
      gpu_type: "GH200"
      features: ["nvme_scratch"]
      topology: "tight"              # scheduler hint: pack into fewest groups

  # Lifecycle
  lifecycle:
    type: "bounded"                  # bounded | unbounded | reactive
    walltime: "72h"                  # for bounded
    preemption_class: 2              # 0 = lowest, higher = harder to preempt
    # For reactive:
    # scale_policy: { min: 4, max: 16, metric: "request_latency_p99", target: "100ms" }

  # Data
  data:
    mounts:
      - source: "s3://datasets/imagenet"
        target: "/data/input"
        access: "read-only"
        tier_hint: "hot"             # scheduler pre-stages if needed
    defaults: true                   # auto-mount home, scratch, output dir

  # Networking
  connectivity:
    network_domain: "ml-workspace"   # shared domain for cross-allocation communication
    expose:                          # for services
      - name: "metrics"
        port: 9090

  # Dependencies (for DAG submissions)
  depends_on:
    - ref: "preprocess-job"
      condition: "success"           # success | failure | any | corresponding

  # Checkpointing
  checkpoint:
    strategy: "auto"                 # auto | manual | none
    # auto: scheduler decides based on cost function
    # manual: application manages its own checkpointing
    # none: non-checkpointable, treated as non-preemptible

  # Telemetry
  telemetry:
    mode: "prod"                     # prod | debug | audit
```

#### DAG Submission

Submit multiple allocations as a workflow graph:

```yaml
dag:
  allocations:
    - id: "stage-data"
      entrypoint: "python stage.py"
      resources: { nodes: 1 }
      lifecycle: { type: "bounded", walltime: "2h" }

    - id: "train"
      entrypoint: "torchrun train.py"
      resources: { nodes: 64, constraints: { topology: "tight" } }
      lifecycle: { type: "bounded", walltime: "72h" }
      depends_on: [{ ref: "stage-data", condition: "success" }]

    - id: "evaluate"
      entrypoint: "python eval.py"
      resources: { nodes: 4 }
      depends_on: [{ ref: "train", condition: "any" }]
```

**DAG size limit:** Maximum 1000 allocations per DAG (configurable). Submissions exceeding this limit are rejected at validation time. See [dag-scheduling.md](dag-scheduling.md) for details.

#### Task Groups (Job Arrays)

```yaml
allocation:
  type: "task_group"
  template:
    entrypoint: "python sweep.py --config=${INDEX}"
    resources: { nodes: 1, constraints: { gpu_type: "GH200" } }
    lifecycle: { type: "bounded", walltime: "4h" }
  range: { start: 0, end: 99 }
  concurrency: 20                   # max simultaneous tasks
```

### Tier 2: Compatibility API (Slurm-like)

Translates familiar Slurm commands to Intent API calls. Implemented as CLI wrappers + lattice-api REST endpoints.

#### Command Mapping

| Slurm | Lattice CLI | Intent API |
|---|---|---|
| `sbatch script.sh` | `lattice submit script.sh` | POST /v1/allocations |
| `sbatch --array=0-99%20 script.sh` | `lattice submit --task-group=0-99%20 script.sh` | POST /v1/allocations (task_group) |
| `sbatch --dependency=afterok:123 script.sh` | `lattice submit --depends-on=123:success script.sh` | POST /v1/allocations (depends_on) |
| `squeue` | `lattice status` | GET /v1/allocations |
| `squeue -u $USER` | `lattice status --user=$USER` | GET /v1/allocations?user= |
| `scancel 123` | `lattice cancel 123` | DELETE /v1/allocations/123 |
| `salloc -N2` | `lattice session --nodes=2` | POST /v1/sessions |
| `srun -n4 hostname` | `lattice launch --alloc=123 -n4 hostname` | POST /v1/allocations/123/tasks |
| `sinfo` | `lattice nodes` | GET /v1/nodes |
| `sacct` | `lattice history` | GET /v1/accounting |
| `--constraint="gpu"` | `--constraint="gpu"` | constraints.features |
| `--partition=debug` | `--vcluster=interactive` | vcluster field |
| `--qos=high` | `--priority=high` | preemption_class |
| `--uenv=prgenv-gnu/24.11:v1` | `--uenv=prgenv-gnu/24.11:v1` | environment.uenv |
| `srun --jobid=123 --pty bash` | `lattice attach 123` | Attach RPC (bidir stream) |
| `cat slurm-123.out` | `lattice logs 123` | GET /v1/allocations/123/logs |
| `tail -f slurm-123.out` | `lattice logs 123 --follow` | StreamLogs RPC |
| `sstat -j 123` | `lattice top 123` | QueryMetrics RPC |
| (no equivalent) | `lattice watch 123` | StreamMetrics RPC |
| (no equivalent) | `lattice diag 123` | GetDiagnostics RPC |
| (no equivalent) | `lattice compare 123 456` | CompareMetrics RPC |

#### Script Parsing

The compatibility layer parses `#SBATCH` directives from submission scripts, translating them to Intent API fields. Unknown directives are warned but not fatal (graceful degradation).

```bash
#!/bin/bash
#SBATCH --nodes=64
#SBATCH --time=72:00:00
#SBATCH --gres=gpu:4
#SBATCH --constraint=GH200
#SBATCH --uenv=prgenv-gnu/24.11:v1
#SBATCH --view=default
#SBATCH --account=ml-team
#SBATCH --job-name=training-run

torchrun --nproc_per_node=4 train.py
```

## Wire Format

gRPC (protobuf) is the primary protocol. REST is provided via gRPC-gateway for browser/curl access.

Protobuf definitions in `proto/` directory. See proto/README.md for schema details.

## Proto Coverage

The protobuf definitions in `proto/lattice/v1/allocations.proto` currently cover:

| Service / Area | Proto Status | Notes |
|---|---|---|
| AllocationService (submit, get, list, cancel, update, watch, checkpoint) | Defined | Core allocation lifecycle |
| Observability RPCs (attach, logs, metrics, diagnostics, compare) | Defined | Part of AllocationService |
| DAG RPCs (get, list, cancel) | Defined | Part of AllocationService |
| NodeService (list, get, drain, undrain, disable, enable, health) | Defined | `proto/lattice/v1/nodes.proto` |
| AdminService (tenant CRUD, vCluster CRUD, Raft status, backup, audit, accounting) | Defined | `proto/lattice/v1/admin.proto` |
| Session RPCs (create, get, delete) | Defined | Part of AllocationService |
| Service Discovery (lookup, list) | Defined | Part of AdminService, `admin.proto` |
| LivenessProbeSpec | Defined | Part of AllocationSpec, `allocations.proto` |

All planned services have been implemented as RPCs within the existing three services (AllocationService, NodeService, AdminService). Both gRPC and REST endpoints are available for all operations.

### Service Discovery Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| gRPC | `AdminService.LookupService(name)` | Returns endpoints for a named service (tenant-filtered) |
| gRPC | `AdminService.ListServices()` | Lists all registered service names (tenant-filtered) |
| REST | `GET /api/v1/services` | JSON list of registered service names |
| REST | `GET /api/v1/services/{name}` | JSON endpoints for a named service |

Tenant filtering: requests with `x-lattice-tenant` header only see services belonging to their tenant. Without the header, all services are visible (admin mode).

### Liveness Probe Schema

Allocations can include an optional `liveness_probe` in the submission spec:

```protobuf
message LivenessProbeSpec {
  string probe_type = 1;    // "tcp" or "http"
  uint32 port = 2;          // 1-65535
  string path = 3;          // HTTP path (e.g., "/healthz")
  uint32 period_secs = 4;   // default: 30
  uint32 initial_delay_secs = 5;
  uint32 failure_threshold = 6;  // default: 3
  uint32 timeout_secs = 7;      // default: 5
}
```

When failure_threshold consecutive probes fail, the allocation is marked Failed. The reconciliation loop then requeues it (for Unbounded/Reactive allocations with appropriate requeue policy).

## Client SDKs

| SDK | Protocol | Location |
|-----|----------|----------|
| Python (`lattice-sdk`) | REST (httpx) | `sdk/python/` |
| Rust (`lattice-client`) | gRPC (tonic) | `crates/lattice-client/` |

The Rust SDK re-exports all proto types as `lattice_client::proto` — consumers do not need to depend on `lattice-common` directly.

## Authentication

All API calls require OIDC bearer token. The lattice CLI handles the OIDC flow via hpc-auth (institutional IdP integration). The lattice-api server validates tokens against the configured OIDC provider.

Sensitive tenant tokens include additional claims for audit trail binding.

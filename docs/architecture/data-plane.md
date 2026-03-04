# Data Plane & Storage Architecture

## Tiered Storage Model

```
┌─ Hot Tier (VAST-like) ─────────────────────────────────┐
│  Protocol: NFS + S3 (native multiprotocol)             │
│  Use: active datasets, home dirs, checkpoints, scratch │
│  Performance: NVMe-speed, low-latency                  │
│  Scheduler integration: QoS per export, pre-staging    │
│  Sensitive: encrypted pool, access-logged                │
└────────────────────┬───────────────────────────────────┘
                     │ policy-driven data mover
┌────────────────────┴───────────────────────────────────┐
│  Warm Tier (capacity storage)                          │
│  Protocol: S3-compatible                               │
│  Use: completed outputs, older datasets, cold models   │
│  Cost: significantly lower than hot                    │
└────────────────────┬───────────────────────────────────┘
                     │ archive policy
┌────────────────────┴───────────────────────────────────┐
│  Cold Tier (tape/object archive)                       │
│  Protocol: S3-compatible (Glacier-style retrieval)     │
│  Use: regulatory retention, long-term archival         │
│  Sensitive: 7+ year retention, immutable                 │
└────────────────────────────────────────────────────────┘
```

## Protocol Standardization

Only two protocols for user-facing access:
- **NFS**: POSIX workloads, home directories, uenv images, legacy codes that expect a filesystem
- **S3**: Object access for checkpoints, datasets, model artifacts, any cloud-native tooling

No Lustre/GPFS client required. VAST delivers parallel-file-system performance via NFS.

## Job Data Requirements

### Explicit Declaration

Users who know their data needs can declare them:

```yaml
data:
  mounts:
    - source: "s3://training-data/imagenet"
      target: "/data/input"
      tier_hint: "hot"
      access: "read-only"
    - source: "nfs://home/{user}"
      target: "/home/{user}"
      access: "read-write"
  output: "s3://{tenant}/{project}/{allocation_id}/"
  scratch_per_node: "500GB"
```

### Sane Defaults (for users who don't specify)

Every allocation automatically gets:
- **Home directory**: mounted via NFS from hot tier (`/home/{user}`)
- **Node-local scratch**: NVMe-backed ephemeral storage (`/scratch/local/`) if NVMe is available; tmpfs or network scratch otherwise
- **Output directory**: `s3://{tenant}/{project}/{allocation_id}/` auto-created
- **Checkpoint directory**: `s3://{tenant}/{project}/{allocation_id}/checkpoints/` (if checkpoint != none)

### Data Staging (Scheduler-Integrated)

The scheduler integrates with the storage API for intelligent data movement:

1. **Pre-staging during queue wait**: When a job is queued and its data is on warm/cold tier, the data mover begins warming it to hot tier. Queue wait time becomes useful instead of idle.

2. **QoS allocation at job start**: The scheduler calls the VAST API to set bandwidth guarantees for the job's NFS export. Prevents I/O-intensive jobs from starving latency-sensitive services.

3. **Checkpoint coordination**: The checkpoint broker pre-allocates storage bandwidth windows to avoid I/O storms when many jobs checkpoint simultaneously.

### VAST API Integration Points

| Operation | VAST API | When |
|---|---|---|
| Create export with QoS | POST /exports + QoS policy | Job starts |
| Query data locality | GET /catalog?path=... | Scheduling (data_readiness score) |
| Create snapshot | POST /snapshots | Job start (reproducibility) or checkpoint |
| Pre-stage from warm | POST /dataspace/prefetch | Job queued, data not on hot tier |
| Set bandwidth floor | PATCH /exports/{id}/qos | Job starts |
| Audit log query | GET /audit/logs?path=... | Compliance reporting |

### Sensitive Storage Policy

```yaml
vcluster: sensitive-secure
  storage_policy:
    encryption: aes-256-at-rest
    pool: dedicated               # separate VAST view/tenant
    wipe_on_release: true         # scrub after allocation ends
    access_logging: full          # every read/write logged
    data_sovereignty: "ch"        # data stays in Swiss jurisdiction
    retention:
      data: "as_specified_by_user"
      audit_logs: "7_years"
      tier_restriction: "hot_only"  # no unencrypted copies on warm/cold
```

### Log Storage

Allocation logs are persisted to S3 alongside output data. See [observability.md](observability.md) for the log storage layout:
```
s3://{tenant}/{project}/{alloc_id}/logs/
    ├── stdout/{node_id}/{chunk_000..N}.log.zst
    ├── stderr/{node_id}/{chunk_000..N}.log.zst
    └── metadata.json
```

Sensitive allocation logs are stored in the encrypted sensitive S3 pool with access logging enabled.

## Node-Local Storage (Optional)

Nodes **may** have NVMe SSDs managed by the node agent. Local storage is not a hard requirement — nodes without NVMe operate with reduced performance but full functionality.

**When NVMe is present:**
- **Scratch**: ephemeral, wiped between allocations. For temp files, staging.
- **Image cache**: persistent across allocations. Caches uenv squashfs images and OCI layers.
  - LRU eviction policy
  - Cache hit avoids network pull from registry
  - Popular images stay warm automatically

**When NVMe is absent:**
- **Scratch**: falls back to tmpfs (RAM-backed) or a network-mounted scratch directory. Capacity is limited by available RAM or network storage quota.
- **Image cache**: no persistent local cache. Images are pulled from the registry on every allocation start (or served from a shared NFS cache if configured). Higher startup latency.
- Allocations requesting the `nvme_scratch` feature constraint will not be scheduled on these nodes.

The node agent detects local storage at startup and reports its availability as part of node capabilities (`features: ["nvme_scratch"]`).

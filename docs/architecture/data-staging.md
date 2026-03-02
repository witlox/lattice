# Data Staging & Cache Lifecycle

## Design Principle

Data staging is invisible to users. The scheduler pre-stages data during queue wait time, manages node-local caches with bounded eviction, and coordinates storage bandwidth to prevent I/O storms. Users declare data requirements; the system handles placement.

This document extends [data-plane.md](data-plane.md) with operational details for staging, caching, and eviction.

## Pre-Staging Pipeline

### Trigger

When an allocation enters the `Pending` state and declares data mounts with `tier_hint: hot`:

1. Scheduler queries VAST API for data locality (`GET /catalog?path=...`)
2. If data is on warm/cold tier: scheduler issues pre-stage request (`POST /dataspace/prefetch`)
3. Allocation transitions to `Staging` state (visible to user via `lattice status`)
4. When staging completes: allocation is eligible for scheduling

### Staging During Queue Wait

Pre-staging runs concurrently with queue waiting. If the allocation reaches the front of the scheduling queue before staging completes:

| Scenario | Action |
|----------|--------|
| Staging complete | Schedule immediately |
| Staging >80% complete | Schedule, accept brief I/O stall at start |
| Staging <80% complete | Hold in queue, f₅ (data_readiness) penalizes scheduling |
| Staging failed | Retry up to 3 times, then alert user and keep in queue |

### Priority

Pre-stage requests are prioritized by:
1. Estimated scheduling time (jobs closer to front of queue stage first)
2. Data size (smaller datasets stage faster, unblock more jobs)
3. Tenant fair share (tenants below their share get staging priority)

### Bandwidth Coordination

The scheduler tracks aggregate staging bandwidth to avoid saturating the VAST system:

```
max_concurrent_staging_bandwidth = 0.3 × total_VAST_write_bandwidth
```

When the staging bandwidth limit is reached, additional staging requests are queued. This prevents staging from impacting running allocations' I/O performance.

## Node-Local Image Cache

Nodes with NVMe SSDs use a dedicated partition for image caching (uenv SquashFS and OCI layers). **Local storage is optional** — nodes without NVMe pull images directly from the registry on every allocation start, or use a shared NFS-based cache if configured. The scheduler accounts for this via the `nvme_scratch` feature: jobs that benefit from local caching can request it as a constraint.

### Cache Layout

```
/var/cache/lattice/
├── uenv/                     # SquashFS images
│   ├── prgenv-gnu_24.11_v1.squashfs
│   ├── pytorch_2.4_cuda12.squashfs
│   └── ...
├── oci/                      # OCI container layers
│   ├── sha256:<hash>/
│   └── ...
└── metadata.json             # Cache index: image → size, last_used, pin
```

### Cache Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `cache_partition_size` | 80% of NVMe (if present) | Reserved for image cache; ignored on nodes without NVMe |
| `cache_high_watermark` | 90% | Eviction starts when usage exceeds this |
| `cache_low_watermark` | 70% | Eviction stops when usage drops below this |
| `min_free_space` | 50 GB | Absolute minimum free space (overrides watermarks) |

### Eviction Policy

**LRU with pinning:**

1. When cache usage exceeds `cache_high_watermark`:
   - Evict least-recently-used images until usage drops below `cache_low_watermark`
   - Never evict images currently mounted by running allocations (pinned)
   - Never evict images marked as `sticky` by admin (base OS images, common frameworks)
2. Eviction order: LRU by last mount time, largest images first among equally-old entries
3. If eviction cannot free enough space (all images pinned or sticky): alert raised, staging for new allocations pauses on this node

### Cache-Full During Staging

If the node-local cache is full when a new allocation needs to pull an image:

1. Check if eviction can free space → run eviction
2. If eviction insufficient (all pinned): allocation's prologue waits with backoff
3. After 3 retries (5 minutes total): node marked as cache-full, scheduler avoids this node for allocations requiring uncached images
4. Scheduler selects alternative nodes with cache space (or where the image is already cached)

### Cache Warming

Administrators can pre-warm caches for anticipated workloads:

```bash
# Warm a uenv image on all nodes in a group
lattice cache warm --image=prgenv-gnu/24.11:v1 --group=3

# Warm on specific nodes
lattice cache warm --image=pytorch/2.4:cuda12 --nodes=x1000c0s0b0n0,x1000c0s0b0n1
```

### Post-Reboot Cache Consistency

After a node reboot (nodes with NVMe only):

1. Node agent reads `metadata.json` from the cache partition
2. Validates each cached image (hash check against registry manifest)
3. Images that fail validation are evicted
4. Images that pass remain in cache (NVMe is persistent across reboots)
5. Cache index rebuilt in ~seconds (metadata only, no full re-scan)

On nodes without NVMe, there is no persistent cache to recover — images are pulled fresh after reboot.

## Allocation Data Lifecycle

### Start (Prologue)

```
1. Node agent receives allocation assignment
2. Pull uenv image:
   a. Check node-local cache → hit: mount directly
   b. Cache miss: pull from registry → write to cache → mount
3. Mount data volumes:
   a. NFS mounts (home, shared data): mount with VAST QoS policy
   b. S3 mounts: FUSE or native S3 client
4. Create scratch directory: /scratch/local/{alloc_id}/ (NVMe) or /scratch/tmp/{alloc_id}/ (tmpfs/network)
5. Create output directory (S3): s3://{tenant}/{project}/{alloc_id}/
6. If checkpoint != none: create checkpoint directory
```

### During Execution

- NFS QoS maintained by VAST (bandwidth floor set at prologue)
- Scratch is node-local NVMe (if available) or tmpfs/network scratch
- Output is written to S3 (async, application-driven)
- Checkpoint broker coordinates checkpoint writes to avoid bandwidth storms

### End (Epilogue)

```
1. Processes terminated (completed, failed, or killed)
2. Flush pending log chunks to S3
3. Unmount uenv image (stays in cache for future use)
4. Unmount NFS volumes
5. Clean scratch: rm -rf /scratch/local/{alloc_id}/
6. Release VAST QoS policy
7. Medical: trigger secure wipe sequence (cross-ref: node-lifecycle.md)
```

### Data Retention

| Data Type | Location | Retention |
|-----------|----------|-----------|
| uenv images | Node-local cache | Until evicted (LRU) |
| Logs | S3 | Configurable (default: 30 days) |
| Checkpoints | S3 | Configurable (default: 7 days after completion) |
| Output | S3 | User-managed (not auto-deleted) |
| Scratch | NVMe or tmpfs | Deleted at allocation end |
| Debug traces | S3 | Short (default: 7 days) |
| Medical audit logs | Cold tier (S3) | 7 years |

## Storage Tier Migration

Data automatically migrates between tiers based on access patterns:

```
Hot (VAST NFS+S3) → Warm (capacity S3) → Cold (archive S3)
     ↑ pre-stage        ↑ restore            ↑ retrieve
```

| Trigger | Direction | Mechanism |
|---------|-----------|-----------|
| Allocation queued with `tier_hint: hot` | Warm → Hot | Scheduler-initiated pre-stage |
| Data untouched for 30 days | Hot → Warm | VAST policy-driven (automatic) |
| Data untouched for 90 days | Warm → Cold | Storage policy (automatic) |
| User request or allocation references cold data | Cold → Warm/Hot | Explicit retrieval (may take hours) |

**Medical exception:** Medical data on hot tier stays on hot tier (no automatic migration). `tier_restriction: hot_only` prevents copies on shared warm/cold tiers.

## Cross-References

- [data-plane.md](data-plane.md) — Storage architecture, VAST API integration, protocol standardization
- [scheduling-algorithm.md](scheduling-algorithm.md) — f₅ data_readiness in cost function
- [node-lifecycle.md](node-lifecycle.md) — Medical node wipe sequence
- [failure-modes.md](failure-modes.md) — VAST unavailability handling
- [sensitive-workloads.md](sensitive-workloads.md) — Medical storage policy

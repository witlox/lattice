# Design: Native uenv Integration for Lattice

> Design-only. No implementation.

## Status Quo

### How uenv works in Slurm (SPANK plugin)

The [eth-cscs/uenv](https://github.com/eth-cscs/uenv) Slurm integration is a **SPANK plugin** (`libslurm-uenv-mount.so`) that hooks into Slurm's job launch pipeline at two points:

1. **Local context** (`init_post_opt` on the submit host):
   - Parses `--uenv=prgenv-gnu/24.11:v1` and `--view=default` from `srun`/`sbatch` args
   - Resolves image references against a **local SQLite repository** (name/version/tag → squashfs path + SHA256)
   - Reads `env.json` metadata from the image to extract view definitions (env var patches: prepend/append/set/unset for PATH-like lists, scalar values)
   - Serializes mount list into `UENV_MOUNT_LIST` environment variable (pipe-delimited `sqfs_path:mount_point` pairs)
   - Applies view environment patches to the job environment via `spank_setenv()`
   - Collects telemetry payload for Elasticsearch

2. **Remote context** (`init_post_opt` on the compute node):
   - Reads `UENV_MOUNT_LIST` from the propagated job environment
   - Temporarily adopts the job's GID (to handle NFS `root_squash`)
   - Validates squashfs files exist and have correct magic bytes (`hsqs`)
   - Calls `unshare(CLONE_NEWNS)` → `mount(MS_SLAVE | MS_REC)` to create isolated mount namespace
   - Calls `do_mount()` per image using libmount (squashfs type, `nosuid,nodev,ro`, loop device)
   - Job processes inherit the mount namespace — uenv is visible, host is unaffected

### What's good about this

- Near-zero overhead (mount namespace, no daemon)
- Views are a clean abstraction: named sets of env var patches stored in `env.json` inside the image
- Multiple uenvs can be composed (each mounted at a different path)
- `tools_uenv` pattern: overlay a debugging/profiling image alongside the main environment
- Repository is just SQLite — simple, local, fast

### What's awkward about it

| Problem | Detail |
|---------|--------|
| **SPANK is a Slurm concept** | Lattice has no SPANK equivalent. The plugin hooks `init_post_opt` which is deeply Slurm-specific. |
| **Env var smuggling** | Mount list is serialized into `UENV_MOUNT_LIST` as a string and propagated via the job environment — fragile, no schema, parsing on both ends. |
| **Two-phase split** | Logic is split between submit host (resolve + metadata read) and compute node (mount). This makes sense for Slurm's architecture but creates a temporal gap where the resolution could go stale. |
| **No scheduler awareness** | Slurm's scheduler knows nothing about uenv. It can't factor image locality into placement or pre-stage images. |
| **View activation is env-only** | Views only patch environment variables. No module loads, no activation scripts, no lifecycle hooks. |
| **No image versioning contract** | The repository is local SQLite per site. No cross-site registry protocol, no content-addressed pulls. |

### How Lattice does it today

Lattice already has a uenv runtime (`lattice-node-agent/src/runtime/uenv.rs`) that:
- Takes an image ref string and mounts it via `squashfs-mount`
- Creates mount namespace via `unshare --mount` + `nsenter`
- Tracks per-allocation state (mount_point, pid, workdir)
- Has an LRU image cache on node-local NVMe

But the integration is shallow:
- `EnvironmentSpec.uenv` is an opaque string — no structured image reference
- `EnvironmentSpec.view` is a string — not validated, not resolved
- Views are not applied (env var patching is not implemented)
- No metadata extraction from images
- No multi-uenv composition
- No scheduler-side image awareness

## Design: Native Lattice Integration

### Principle

Replace the SPANK plugin's two-phase hack with a **first-class scheduler-aware uenv pipeline** where image resolution, view activation, and mount setup are explicit stages in Lattice's allocation lifecycle.

### 1. Structured Image Reference

Replace opaque string with a structured type:

```protobuf
message UenvRef {
  // Human-readable: "prgenv-gnu/24.11:v1" or "prgenv-gnu/24.11@sha256:abc..."
  string spec = 1;

  // Resolved by API server before queuing:
  string registry = 10;        // e.g., "jfrog.cscs.ch/uenv"
  string name = 11;            // "prgenv-gnu"
  string version = 12;         // "24.11"
  string tag = 13;             // "v1"
  string sha256 = 14;          // content hash (immutable after resolve)
  uint64 size_bytes = 15;      // for cache/staging decisions
}

message EnvironmentSpec {
  repeated UenvRef uenvs = 1;  // multiple images, each with its mount point
  repeated string views = 2;    // "default", "spack", "uenv_name:view_name"
  bool no_default_view = 3;
  string image = 10;           // OCI alternative (mutually exclusive with uenvs)
  bool sign_required = 20;
}
```

**Resolution happens once** at the API server on submit, not on the compute node. The `sha256` is pinned — no TOCTOU race between submit and launch.

### 2. Registry Protocol

Replace per-node SQLite with a **centralized OCI-compatible registry** (the uenv project already uses ORAS for pushing images):

```
Submit → API resolves name:tag → sha256 via registry
       → Stores resolved UenvRef in allocation spec
       → Scheduler sees size_bytes for cache-aware placement
       → Node agent pulls by sha256 (content-addressed, cacheable)
```

For air-gapped / sensitive sites: registry can be site-local with signature verification at pull time.

### 3. Metadata Extraction as a Discrete Step

The SPANK plugin reads `env.json` from the mounted squashfs on the submit host. In Lattice, separate this:

```
┌─────────────┐    ┌──────────────┐    ┌────────────┐
│  API Server  │───▶│ Metadata Svc │───▶│  Registry  │
│  (on submit) │    │  (cache)     │    │  (ORAS/S3) │
└─────────────┘    └──────────────┘    └────────────┘
```

- **Metadata service** extracts `env.json` from the squashfs image (or caches it alongside the registry)
- Returns structured `UenvMeta`: name, description, mount point, views (with typed env patches)
- API server validates views exist before accepting the allocation
- Env patches are stored in the allocation spec — no runtime metadata parsing on compute nodes

```protobuf
message UenvMeta {
  string name = 1;
  string description = 2;
  string mount = 3;
  repeated ViewDef views = 4;
  string default_view = 5;
}

message ViewDef {
  string name = 1;
  string description = 2;
  repeated EnvPatch patches = 3;
}

message EnvPatch {
  string variable = 1;
  EnvOp op = 2;          // PREPEND, APPEND, SET, UNSET
  string value = 3;
  string separator = 4;  // default ":"
}
```

### 4. Scheduler-Aware Image Placement

The scheduler currently ignores uenv entirely. With structured refs:

**Cost function extension:**
```
f₅ = data_readiness now includes:
  - Is the squashfs image already in node-local NVMe cache? (cache_hit_bonus)
  - Is it on the same VAST pool / NFS server? (proximity_bonus)
  - What's the pull cost? (size_bytes / available_bandwidth)
```

**Pre-staging during queue wait:**
The data mover already pre-stages input data. Extend it to pre-stage uenv images:
```
Allocation enters queue
  → Scheduler identifies target node group (tentative placement)
  → Data mover begins pulling image to those nodes' NVMe cache
  → By the time allocation is scheduled, image is warm
```

This is invisible to the user — same UX as today, but faster cold starts.

### 5. View Activation in the Prologue

Replace the SPANK plugin's `spank_setenv()` with an explicit prologue step:

```
Prologue pipeline (existing):
  1. Image cache check     ← already exists
  2. Cgroup creation       ← already exists
  3. Runtime prepare       ← mount squashfs (already exists)
  4. ★ View activation     ← NEW: apply env patches from resolved metadata
  5. Memory policy         ← already exists
  6. Data staging          ← already exists
```

View activation:
- Reads pre-resolved `ViewDef` from the allocation spec (no filesystem access needed)
- Applies `EnvPatch` operations in order: prepend/append modify PATH-like variables, set/unset for scalars
- Resulting environment is passed to `spawn()` — clean, no env var smuggling

### 6. Multi-uenv Composition

The SPANK plugin supports mounting multiple squashfs images at different paths. Lattice should make this first-class:

```protobuf
message UenvRef {
  // ... fields from above ...
  string mount_point = 20;  // explicit mount target, or derived from image metadata
}
```

Composition rules (validated at submit time by API server):
- Mount points must not overlap
- Each uenv's views are namespaced: `uenv_name:view_name`
- Env patches are applied in the order views are listed
- Conflicts (same variable patched by multiple views) produce a warning, last-write-wins

Tools uenv pattern becomes natural:
```yaml
uenvs:
  - spec: "prgenv-gnu/24.11:v1"      # main environment
  - spec: "debug-tools/2024.1:v1"     # profiling overlay
    mount_point: "/user-tools"
views: ["default", "debug-tools:perf"]
```

### 7. CLI Integration

```bash
# Submit with uenv (same UX as today)
lattice submit --uenv prgenv-gnu/24.11:v1 --view default -- ./my_app

# Multiple uenvs
lattice submit \
  --uenv prgenv-gnu/24.11:v1 \
  --uenv debug-tools/2024.1:v1:/user-tools \
  --view default --view debug-tools:perf \
  -- ./my_app

# Interactive session with uenv (replaces `uenv start`)
lattice session --uenv prgenv-gnu/24.11:v1 --view default

# Query available views
lattice uenv views prgenv-gnu/24.11:v1
# → default: "Default GNU programming environment"
# → spack:   "Spack-managed packages"

# Inspect image metadata
lattice uenv inspect prgenv-gnu/24.11:v1

# List cached images on a node
lattice uenv cache list --node nid001234
```

### 8. Sensitive Workload Extensions

For sensitive allocations, the pipeline adds:
- **Signature verification** at pull time (Ed25519, same key infrastructure as audit log)
- **Vulnerability scan gate**: registry metadata includes scan status; API server rejects unscanned images for sensitive allocations
- **Image pinning**: sensitive allocations must use `@sha256:...` refs (no floating tags)
- **Signed tools_uenv only**: profiling overlays must also be signed

### 9. What This Replaces

| SPANK plugin concern | Lattice equivalent |
|---|---|
| `--uenv` CLI arg parsing | `lattice submit --uenv` → API server resolution |
| Local SQLite repository | Centralized registry (ORAS/S3) |
| `UENV_MOUNT_LIST` env propagation | Resolved `UenvRef` in allocation spec (protobuf) |
| `spank_setenv()` for views | Prologue view activation step |
| `unshare(CLONE_NEWNS)` on compute | `UenvRuntime.prepare()` (already exists) |
| `do_mount()` via libmount | `squashfs-mount` binary (already exists) |
| Elasticsearch telemetry post | Lattice telemetry pipeline (eBPF + TSDB) |

### 10. What We Don't Build

- **No SPANK equivalent**: Lattice controls the full lifecycle, no plugin architecture needed
- **No local SQLite per node**: Registry is the source of truth
- **No env var smuggling**: Structured protobuf all the way through
- **No GID switching hack**: Lattice node agent already runs with appropriate privileges
- **No libmount direct calls**: Continue delegating to `squashfs-mount` binary

## Migration Path from Slurm

Users migrating from Slurm with uenv:

1. **Same image format**: SquashFS images are unchanged, same `env.json` metadata
2. **Same view names**: Views work identically
3. **Compat layer**: `sbatch --uenv=X --view=Y` translates to `lattice submit --uenv X --view Y` (already planned in lattice-cli compat)
4. **Registry migration**: One-time push from SQLite repo to ORAS registry (`uenv push` already exists)

## Open Questions

1. **View composition conflicts**: Should conflicting env patches across views be an error or warning? (Recommend: warning + last-write-wins, matching uenv behavior)
2. **Metadata cache invalidation**: When an image tag is re-pushed (mutable tag), how quickly must the metadata cache refresh? (Recommend: sha256-keyed cache never invalidates; tag→sha256 mapping has short TTL)
3. **squashfs-mount vs. libmount**: Should we eventually call `mount()` directly from Rust instead of shelling out to `squashfs-mount`? (Recommend: keep binary for now — it handles loop device setup and is maintained upstream)
4. **View activation hooks**: Should views support running activation scripts beyond env vars (e.g., `source /path/to/activate.sh`)? (Recommend: no — env patches are deterministic, scripts are not. Keep it pure.)

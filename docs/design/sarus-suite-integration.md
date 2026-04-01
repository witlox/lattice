# Design: Sarus Suite (cluster-tooling) Integration for Lattice

> Design-only. No implementation.

## Status Quo

### What changed: old Sarus → Sarus Suite

The [old Sarus](https://github.com/eth-cscs/sarus) was a monolithic C++ container engine with OCI hooks. It's been replaced by [sarus-suite/cluster-tooling](https://github.com/sarus-suite/cluster-tooling) — a Rust workspace with four crates that delegates container execution to **Podman** and focuses on HPC-specific glue.

| Aspect | Old Sarus (C++) | Sarus Suite (Rust) |
|--------|----------------|-------------------|
| Runtime | Custom OCI runtime | Delegates to **Podman** (`--module hpc`) |
| Scheduler integration | Standalone CLI (`sarus run`) | **SPANK plugin** (`skybox`) loaded by slurmstepd |
| Image management | Custom SquashFS handling | Podman + **Parallax** (shared SquashFS image store) |
| User interface | `sarus run --image X` | `srun --edf=file.toml` (EDF = Environment Definition File) |
| Environment definition | CLI flags | **EDF files** (TOML) with inheritance chain |
| Namespace joining | OCI hooks | Direct `setns()` into Podman container namespaces |
| GPU support | Custom NVIDIA hooks | Podman CDI (`nvidia.com/gpu=all`) |
| Language | C++ | Rust |

### Sarus Suite Components

| Crate | Type | Purpose |
|-------|------|---------|
| **skybox** | `cdylib` (shared lib) | Slurm SPANK plugin — the scheduler integration point |
| **raster** | Library | EDF schema, parsing, rendering, validation |
| **sarus-suite-podman-driver** | Library | Constructs `podman run` commands from EDFs |
| **sarusctl** | Binary | CLI for admin/testing — validate, pull, migrate, run |

External dependency: **Parallax** — migrates OCI images into a shared read-only SquashFS image store via `squashfuse_ll`.

### How Sarus Suite works in Slurm

The skybox SPANK plugin hooks the job launch:

1. **`init_post_opt`** (submit host): Parse `--edf=file.toml`, load and render EDF (resolve `base_environment` chain)
2. **`user_init`** (slurmstepd): Expand config, set up working directories
3. **`task_init`** (slurmstepd, per task): Pull image (first task only, synced), start Podman container in detached mode, **join container's user+mount namespaces via `setns()`**, import container env vars from `/proc/<pid>/environ`, set workdir
4. **`task_exit`**: Coordinate container stop across tasks
5. **`exit`**: Clean up local and shared filesystem artifacts

Key insight: the job process itself **enters the container's namespace** rather than running as a child of Podman. This is what enables native MPI, RDMA, and scheduler integration — the process is still managed by Slurm, it just has the container's filesystem view.

### EDF Format (the key abstraction)

```toml
image = "nvcr.io/nvidia/pytorch:24.01-py3"
base_environment = ["base-gpu", "base-mpi"]     # Inheritance chain
mounts = ["/scratch:/scratch:ro", "/home/user:/home/user"]
devices = ["/dev/fuse", "nvidia.com/gpu=all"]    # CDI for GPUs
workdir = "/workspace"
writable = false
entrypoint = false                                # Don't run image entrypoint

[env]
NCCL_DEBUG = "INFO"
MY_CUSTOM_VAR = "value"

[annotations]
com.hooks.ssh.enabled = "true"                   # SSH into container
```

EDFs support:
- **Inheritance**: `base_environment` chains (up to 10 levels), system-provided base EDFs in `/etc/edf/`
- **Layered config**: System TOML files (`/etc/sarus-suite/*.conf`) with numeric priority ordering
- **Annotations**: `com.sarus.*` prefix for overriding system config; passed to Podman for hooks

### How Lattice does containers today

The `SarusRuntime` in `lattice-node-agent/src/runtime/sarus.rs`:
- Calls the old `sarus` CLI binary (`sarus pull`, `sarus run`, `sarus stop`)
- Tracks container ID and PID per allocation
- Has a `SarusConfig` with `sarus_bin` path and `extra_flags`
- No EDF awareness, no Podman delegation, no shared image store

The `EnvironmentSpec` in allocations:
- `image: Option<String>` — opaque OCI reference string
- No structured container spec, no mount declarations, no device passthrough, no env patches, no inheritance

## Design: Native Sarus Suite Integration

### Principle

Replace the old `sarus` CLI shelling with a **first-class Podman-based container pipeline** that uses the sarus-suite libraries directly (raster for EDF parsing, podman-driver for command construction) and adopts the `setns()` pattern for namespace joining. This eliminates the SPANK dependency while preserving the HPC-optimized execution model.

### 1. Structured Container Spec (EDF-compatible)

Extend `EnvironmentSpec` to carry a full container specification that maps 1:1 to the EDF format:

```protobuf
message ContainerSpec {
  // OCI image reference
  string image = 1;

  // Resolved image info (pinned at submit time, like UenvRef)
  string registry = 10;
  string digest = 11;           // sha256 content hash
  uint64 size_bytes = 12;

  // EDF-equivalent fields
  repeated string base_environments = 20;  // Inheritance chain
  repeated Mount mounts = 21;
  repeated string devices = 22;            // CDI device specs
  string workdir = 23;
  bool writable = 24;
  bool use_entrypoint = 25;
  map<string, string> env = 26;
  map<string, string> annotations = 27;
}

message Mount {
  string source = 1;
  string target = 2;
  string options = 3;     // "ro", "rw", etc.
}

message EnvironmentSpec {
  repeated UenvRef uenvs = 1;          // SquashFS images (uenv path)
  repeated string views = 2;
  ContainerSpec container = 10;        // OCI container (sarus-suite path)
  bool sign_required = 20;
}
```

`uenvs` and `container` are mutually exclusive (enforced at API validation). This matches the existing design principle: uenv for HPC-native software stacks, containers for third-party/isolated workloads.

### 2. EDF Rendering in the API Server

At submit time, the API server:

1. Receives the allocation with a `ContainerSpec` or raw EDF TOML string
2. Uses the **raster** library to parse and render the EDF (resolve `base_environment` chain)
3. Resolves the image reference against the registry → pins `digest` and `size_bytes`
4. Validates: image exists, mounts are allowed, devices are permitted for the tenant
5. Stores the fully-resolved `ContainerSpec` in the allocation — no runtime resolution needed

For CLI submissions:
```bash
# Direct fields (simple)
lattice submit --image nvcr.io/nvidia/pytorch:24.01-py3 \
  --mount /scratch:/scratch:ro --device nvidia.com/gpu=all -- python train.py

# EDF file (full control)
lattice submit --edf my-environment.toml -- python train.py

# EDF with uenv composition (container + uenv overlay)
# Not supported — mutually exclusive by design
```

### 3. Podman-Based Container Lifecycle

Replace `SarusRuntime` with `PodmanRuntime` that mirrors the skybox execution model:

```
Allocation lifecycle (container path):

  Prologue:
    1. Image cache check (Parallax shared store)
    2. Cgroup creation (existing)
    3. ★ Podman pull + migrate (if not cached)
    4. ★ Podman start (detached mode, self-stopping init)
    5. ★ Namespace join: setns(container_pid, CLONE_NEWNS | CLONE_NEWUSER)
    6. ★ Env import: read /proc/<container_pid>/environ
    7. Memory policy (existing)
    8. Data staging (existing)

  Spawn:
    - Process runs inside the container's namespace view
    - Not a child of Podman — managed directly by lattice-node-agent
    - Inherits container's filesystem, devices, env vars

  Epilogue:
    1. Process exit captured
    2. ★ Podman stop (graceful container shutdown)
    3. ★ Parallax cleanup (if configured)
    4. Cgroup destroy (existing)
    5. Data cleanup (existing)
    6. Sensitive wipe (existing, if applicable)
```

### 4. PodmanRuntime Implementation

```rust
/// Sarus Suite Podman-based container runtime.
///
/// Uses sarus-suite-podman-driver to construct Podman commands and
/// the setns() pattern for namespace joining.
pub struct PodmanRuntime {
    /// Per-allocation container state.
    containers: Arc<RwLock<HashMap<AllocId, PodmanState>>>,
    /// Podman configuration.
    config: PodmanConfig,
}

struct PodmanState {
    /// Container ID from `podman start`.
    container_id: String,
    /// PID of the container's init process.
    container_pid: u32,
    /// Original namespaces FDs (for cleanup).
    saved_ns: Option<SavedNamespaces>,
    /// Environment captured from container.
    container_env: HashMap<String, String>,
    /// Whether container has been stopped.
    stopped: bool,
}

pub struct PodmanConfig {
    /// Path to podman binary.
    pub podman_bin: String,                    // default: "podman"
    /// Podman module (HPC optimizations).
    pub podman_module: String,                 // default: "hpc"
    /// Temporary storage for image pulls.
    pub podman_tmp_path: String,              // default: "/dev/shm"
    /// Path to Parallax shared image store.
    pub parallax_imagestore: Option<String>,   // e.g., "/scratch/parallax"
    /// Path to parallax binary.
    pub parallax_bin: Option<String>,
    /// Path to parallax mount program (squashfuse_ll).
    pub parallax_mount_program: Option<String>,
}
```

The runtime implements the existing `Runtime` trait:
- `prepare()`: Pull image → migrate to Parallax store → start Podman detached → read container PID
- `spawn()`: `setns()` into container namespaces → exec entrypoint (process inherits container view)
- `signal()`: Signal the spawned process (inside container NS)
- `stop()`: Stop Podman container → cleanup
- `cleanup()`: Remove Podman container, clean local artifacts

### 5. The setns() Pattern

This is the critical difference from "running inside Podman". Instead of being a Podman-managed process, the workload process:

1. Lattice agent starts Podman with a self-stopping init: `podman run -d sh -c "kill -STOP $$ ; exit 0"`
2. Agent reads the container PID from `podman inspect`
3. Agent opens `/proc/<container_pid>/ns/user` and `/proc/<container_pid>/ns/mnt`
4. Agent calls `setns()` on both FDs → current thread enters the container's namespace
5. Agent `exec()`s the user's entrypoint — it runs with container's filesystem view but is a direct child of the agent (not Podman)
6. Process has native access to Slingshot RDMA, MPI, GPUs — no container isolation overhead

This is identical to what skybox does in slurmstepd. Lattice replaces slurmstepd.

### 6. Parallax Image Store Integration

Parallax provides a shared read-only image store using SquashFS:

```
Registry (OCI) ──pull──► Local graphroot (/dev/shm)
                              │
                    parallax --migrate
                              │
                              ▼
                    Shared image store (NFS/VAST)
                    /scratch/parallax/
                    ├── layers/          (SquashFS per layer)
                    ├── manifests/       (OCI manifests)
                    └── metadata/
```

**Scheduler integration** (parallels the uenv design):
- `f₅ = data_readiness` includes: is the container image in the Parallax store? Is it on the local VAST pool?
- Pre-staging during queue wait: data mover triggers `parallax --migrate` to the target node group's shared store
- `size_bytes` in `ContainerSpec` enables the scheduler to estimate pull cost

**Node-local caching**: Parallax uses `squashfuse_ll` (FUSE) for on-demand layer access. Frequently-used layers stay in page cache. No explicit NVMe cache needed (unlike uenv where the full SquashFS is local).

### 7. EDF Base Environment Inheritance

The EDF `base_environment` chain is a powerful feature. Lattice should support it:

```
System EDFs (/etc/lattice/edf/):
  base-gpu.toml     → devices = ["nvidia.com/gpu=all"], env.CUDA_HOME
  base-mpi.toml     → mounts = ["/opt/cray/pe"], env.FI_PROVIDER
  base-nccl.toml    → base_environment = ["base-gpu"], env.NCCL_*

User EDF (in allocation):
  image = "nvcr.io/nvidia/pytorch:24.01-py3"
  base_environment = ["base-gpu", "base-mpi"]
  workdir = "/workspace"
```

The API server resolves the chain at submit time using the **raster** library:
1. Load system EDFs from configured search paths
2. Walk `base_environment` chain (max 10 levels, cycle detection)
3. Merge: user fields override base fields, mounts/devices/env are additive
4. Store the fully-rendered EDF in the allocation — no chain resolution at runtime

### 8. GPU Passthrough via CDI

Sarus Suite uses Podman's CDI (Container Device Interface) for GPU access:

```toml
devices = ["nvidia.com/gpu=all"]       # All GPUs
devices = ["nvidia.com/gpu=0,1"]       # Specific GPUs
devices = ["/dev/fuse"]                # Explicit device paths
```

Lattice integration:
- `ContainerSpec.devices` carries CDI specs
- API server validates: tenant is allowed to request GPUs (quota check)
- Scheduler uses `gpu_count` for placement, CDI spec for runtime
- Node agent passes devices to `podman run --device`

For **sensitive workloads**: CDI devices are validated against the allowed device list in the sensitive workload policy. No `nvidia.com/gpu=all` for sensitive — must specify exact GPU indices matching the claimed nodes.

### 9. Relationship to uenv Design

The uenv design doc (`docs/design/uenv-native-integration.md`) and this sarus-suite design are complementary:

| Concern | uenv | sarus-suite |
|---------|------|-------------|
| Image format | SquashFS | OCI (via Podman + Parallax) |
| Env setup | View activation (env patches from env.json) | EDF rendering (TOML with inheritance) |
| Namespace | Mount namespace only (lightweight) | User + mount namespace (full container) |
| Isolation | None (shared host, mount ns only) | Container isolation (Podman) |
| Use case | HPC software stacks (compilers, MPI, libraries) | Third-party images, reproducible environments |
| Scheduler aware | `f₅` includes image cache locality | `f₅` includes Parallax store locality |
| Pre-staging | Data mover pulls SquashFS to NVMe | Data mover triggers `parallax --migrate` |

**Composition rule**: An allocation uses either `uenvs` OR `container`, never both. This avoids layering mount namespaces (uenv creates one, Podman creates another — they'd conflict).

Exception: a future "tools overlay" pattern could mount a debugging uenv alongside a container, but this requires careful namespace ordering and is deferred.

### 10. Configuration

```yaml
# lattice-agent config: container runtime section
container:
  runtime: podman                      # "podman" (new) or "sarus" (legacy compat)
  podman_bin: /usr/bin/podman
  podman_module: hpc
  podman_tmp_path: /dev/shm
  parallax_imagestore: /scratch/parallax
  parallax_bin: /usr/bin/parallax
  parallax_mount_program: /usr/bin/squashfuse_ll
  edf_system_search_path:
    - /etc/lattice/edf
    - /etc/sarus-suite/edf
  allowed_registries:                  # Restrict image sources
    - nvcr.io
    - ghcr.io
    - jfrog.cscs.ch
  pull_timeout_secs: 300
```

### 11. CLI Integration

```bash
# Simple container submission
lattice submit --image nvcr.io/nvidia/pytorch:24.01-py3 -- python train.py

# With mounts and devices
lattice submit \
  --image nvcr.io/nvidia/pytorch:24.01-py3 \
  --mount /scratch:/scratch:ro \
  --device nvidia.com/gpu=all \
  -- python train.py

# EDF-based (full control)
lattice submit --edf my-environment.toml -- python train.py

# Admin: manage shared image store
lattice container images                    # List Parallax store
lattice container pull nvcr.io/nvidia/pytorch:24.01-py3
lattice container inspect nvcr.io/nvidia/pytorch:24.01-py3

# Slurm compatibility (compat layer)
sbatch --container-image=nvcr.io/nvidia/pytorch:24.01-py3 script.sh
# → translates to lattice submit --image ...
```

### 12. Sensitive Workload Extensions

Identical to the uenv design:
- **Image signature verification** at pull time
- **Vulnerability scan gate**: reject unscanned images for sensitive allocations
- **Digest pinning**: sensitive allocations must use `@sha256:...` (no mutable tags)
- **Registry restriction**: sensitive tenants can only pull from approved registries
- **Device restriction**: explicit GPU indices, no `gpu=all`

Additional for containers:
- **Read-only rootfs enforced**: `writable = false` for sensitive allocations
- **No custom entrypoint**: sensitive allocations must not override image entrypoint (prevents payload injection)
- **Network isolation**: container gets no network namespace — Slingshot VNI is configured by Lattice, not Podman

### 13. What This Replaces

| Old Sarus concern | Lattice sarus-suite equivalent |
|---|---|
| `sarus pull <image>` | `podman pull` + `parallax --migrate` (via PodmanRuntime) |
| `sarus run [opts] <image> <cmd>` | `podman start` (detached) + `setns()` + `exec()` |
| `sarus stop <container>` | `podman stop` (via PodmanRuntime) |
| Custom NVIDIA OCI hooks | Podman CDI (`nvidia.com/gpu=all`) |
| Custom MPI hooks | Native MPI via namespace joining (no hook needed) |
| JSON config file | TOML config (`/etc/lattice/container.toml`) |
| Per-node image storage | Parallax shared image store on VAST/NFS |
| `sarus images` | `lattice container images` (wraps `sarusctl images`) |

### 14. What We Don't Build

- **No SPANK plugin**: Lattice replaces slurmstepd entirely — skybox is not needed
- **No custom container runtime**: Delegate to Podman, which is actively maintained
- **No image layer deduplication**: Parallax handles this with its SquashFS store
- **No container networking**: Workloads use Slingshot VNIs configured by Lattice, not container networking
- **No pod/multi-container support**: One container per allocation (HPC model)
- **No Docker compatibility layer**: Direct OCI/Podman only

### 15. Dependencies

| Dependency | Required | Notes |
|-----------|----------|-------|
| Podman | Yes | `>= 5.0` with `--module hpc` support |
| Parallax | Recommended | For shared image store; without it, each node pulls independently |
| squashfuse_ll | With Parallax | FUSE mount for SquashFS layers |
| raster (sarus-suite) | Build-time | EDF parsing library (Rust crate, vendorable) |
| CDI (container device interface) | For GPU | NVIDIA CDI driver installed on compute nodes |

## Migration Path

### From old Sarus
1. **Same OCI images**: No image rebuilds needed
2. **Config translation**: `sarus.json` → TOML config (one-time)
3. **CLI compat**: `sarus run --image X` → `lattice submit --image X` (compat layer)

### From Slurm with sarus-suite
1. **Same EDFs**: `.toml` files work unchanged
2. **Same Parallax store**: Point Lattice at the existing `parallax_imagestore` path
3. **Same base_environments**: System EDFs in `/etc/sarus-suite/edf/` or `/etc/lattice/edf/`
4. **CLI**: `srun --edf=X` → `lattice submit --edf X`

## Open Questions

1. **raster as a build dependency**: Should Lattice vendor the raster crate or depend on the sarus-suite workspace? (Recommend: crates.io dependency if published, vendored otherwise)
2. **Podman rootless vs rootful**: The node agent runs as root. Should it run Podman rootless (better security) or rootful (simpler device access)? (Recommend: rootless with CDI for GPUs, matching sarus-suite's approach)
3. **setns() on macOS**: The `setns()` pattern is Linux-only. Testing on macOS requires a stub implementation. (Recommend: `cfg(target_os = "linux")` gate, same as existing runtime code)
4. **Container + uenv composition**: Should we allow mounting a tools uenv alongside a container? (Recommend: defer — namespace ordering is complex, and containers already support custom mounts)
5. **Parallax dependency**: Should Lattice work without Parallax (direct Podman pull per node)? (Recommend: yes, Parallax is recommended but optional — without it, each node pulls independently, slower but functional)

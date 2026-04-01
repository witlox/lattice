# Interface: Software Delivery

Structural contracts for uenv and container (sarus-suite) integration.
Traces to: INV-SD1–SD10, BDD `software_delivery.feature`, designs in `docs/design/`.

## Module Boundaries

```
┌──────────────┐     ┌───────────────┐     ┌──────────────────┐
│  lattice-api │────►│ lattice-common│◄────│ lattice-scheduler│
│  (resolve,   │     │ (types, trait │     │ (f5 scoring,     │
│   validate)  │     │  definitions) │     │  image staging)  │
└──────┬───────┘     └───────────────┘     └────────┬─────────┘
       │                                            │
       │         ┌──────────────────────┐           │
       └────────►│ lattice-node-agent   │◄──────────┘
                 │ (pull, mount/setns,  │
                 │  view activation,    │
                 │  container lifecycle)│
                 └──────────────────────┘
```

## Trait: ImageResolver (lattice-common)

Resolves human-readable image references to content-addressed ImageRefs.
Called by the API server at submit time (or scheduler for deferred resolution).

```rust
#[async_trait]
pub trait ImageResolver: Send + Sync {
    /// Resolve a spec string to a content-addressed ImageRef.
    /// Returns None if the image does not exist in the registry.
    async fn resolve(&self, spec: &str, image_type: ImageType)
        -> Result<Option<ImageRef>, ImageResolveError>;

    /// Extract metadata (views, mount point) from a resolved image.
    async fn metadata(&self, image: &ImageRef)
        -> Result<ImageMetadata, ImageResolveError>;
}
```

**Implementations:**
- `OrasImageResolver` — resolves uenv images from ORAS/S3 registry
- `OciImageResolver` — resolves OCI images from container registries
- `StubImageResolver` — test stub, returns configurable results

**Enforcement:**
- INV-SD1: `resolve()` returns sha256; callers store it immutably
- INV-SD2: `metadata()` returns views; callers validate view names
- INV-SD4: scheduler calls `resolve()` each cycle for deferred images

## Trait: ImageStager (lattice-node-agent)

Pulls images to local or shared storage. Extends `DataStageExecutor`.

```rust
#[async_trait]
pub trait ImageStager: Send + Sync {
    /// Check if an image is available locally (cache or shared store).
    async fn is_cached(&self, image: &ImageRef) -> bool;

    /// Pull an image to the shared store (single pull, INV-SD6).
    async fn stage(&self, image: &ImageRef) -> Result<StagedImage, ImageStageError>;

    /// Report cache readiness as 0.0–1.0 for f5 scoring.
    fn readiness(&self, image: &ImageRef) -> f64;
}

pub struct StagedImage {
    pub local_path: String,       // Path to mounted/extracted image
    pub from_cache: bool,         // True if already in cache
}
```

**Implementations:**
- `UenvImageStager` — pulls SquashFS to VAST/NFS or node NVMe cache
- `ParallaxImageStager` — delegates to `parallax --migrate` for OCI images
- `DirectPullStager` — fallback: `podman pull` directly (no shared store)
- `NoopImageStager` — test stub

**Enforcement:**
- INV-SD6: `stage()` targets shared store, not per-node
- INV-SD9: integrated into `DataStager.plan_staging()`

## Types: ImageRef, EnvPatch, ImageMetadata (lattice-common)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ImageType {
    Uenv,
    Oci,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRef {
    pub spec: String,              // "prgenv-gnu/24.11:v1"
    pub image_type: ImageType,
    pub registry: String,          // "jfrog.cscs.ch/uenv" or "nvcr.io"
    pub name: String,              // "prgenv-gnu"
    pub version: String,           // "24.11"
    pub original_tag: String,      // "v1" (preserved for audit, INV-SD1)
    pub sha256: String,            // content hash (empty if deferred)
    pub size_bytes: u64,
    pub mount_point: String,       // "/user-environment" (uenv) or "/" (container)
    pub resolve_on_schedule: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EnvOp {
    Prepend,
    Append,
    Set,
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvPatch {
    pub variable: String,
    pub op: EnvOp,
    pub value: String,
    pub separator: String,          // default ":"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewDef {
    pub name: String,
    pub description: String,
    pub patches: Vec<EnvPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub name: String,
    pub description: String,
    pub mount_point: String,        // Default mount from image metadata
    pub views: Vec<ViewDef>,
    pub default_view: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub base_environments: Vec<String>,   // EDF inheritance chain
    pub mounts: Vec<Mount>,
    pub devices: Vec<String>,             // CDI: "nvidia.com/gpu=all"
    pub workdir: String,
    pub writable: bool,
    pub env: Vec<(String, String)>,
    pub annotations: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub options: String,                  // "ro", "rw"
}
```

## Revised Environment Type (lattice-common)

Replaces the current flat `Environment` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub images: Vec<ImageRef>,
    pub env_patches: Vec<EnvPatch>,       // Resolved from views/EDF
    pub devices: Vec<String>,             // CDI device specs
    pub mounts: Vec<Mount>,               // Additional bind mounts
    pub container: Option<ContainerSpec>, // Full container spec (if OCI)
    pub writable: bool,                   // Container rootfs writability
    pub sign_required: bool,
    pub scan_required: bool,
    pub approved_bases_only: bool,
}
```

**Migration from current:** The existing `uenv: Option<String>`, `view: Option<String>`,
`image: Option<String>`, `tools_uenv: Option<String>` fields are **removed** and replaced
by `images` (Vec<ImageRef>) and `env_patches` (Vec<EnvPatch>). Clean break — no backward
compatibility layer. All clients (CLI, Python SDK, lattice-client) updated in the same
release. No external consumers exist yet.

## API Server: Resolution Pipeline (lattice-api)

```
Submit allocation
  │
  ├─ Parse EnvironmentSpec from proto
  │
  ├─ For each image:
  │   ├─ resolve_on_schedule? → store unresolved ImageRef, skip validation
  │   └─ resolve via ImageResolver
  │       ├─ Not found → reject submission (INV-SD2)
  │       └─ Found → pin sha256, populate size_bytes
  │           └─ Fetch metadata → validate views (INV-SD2)
  │
  ├─ Validate mount point non-overlap (INV-SD3)
  │   Collect ALL mount targets: ImageRef.mount_point + Environment.mounts[].target
  │   Sort by length, check each against all shorter for prefix containment
  │   /opt overlaps /opt/env (prefix shadowing)
  │
  ├─ Validate EDF inheritance (INV-SD8)
  │   ├─ Load system EDFs from search paths
  │   ├─ Walk chain with depth counter + visited set
  │   └─ Cycle or depth > 10 → reject
  │
  ├─ Resolve views → populate env_patches (INV-SD7)
  │
  ├─ Sensitive checks (INV-SD5):
  │   ├─ sign_required must be true
  │   ├─ sha256 must be non-empty (no deferred for sensitive)
  │   ├─ writable must be false
  │   └─ devices must use specific indices (no gpu=all)
  │
  └─ Store resolved Environment in allocation
```

## Scheduler: Image-Aware Placement (lattice-scheduler)

**New SchedulerCommandSink method** (INV-SD4):
```rust
/// Pin a resolved image on an allocation (deferred resolution).
/// Proposes Command::ResolveImage to the Raft quorum.
async fn resolve_image(
    &self,
    alloc_id: AllocId,
    image_index: usize,
    resolved: ImageRef,
) -> Result<(), LatticeError>;
```

**New SchedulerStateReader method:**
```rust
/// Read allocations with unresolved images (resolve_on_schedule = true, sha256 empty).
async fn unresolved_allocations(&self) -> Result<Vec<Allocation>, LatticeError>;
```

**Image readiness cache:** `ImageStager.readiness()` is synchronous and reads from
an in-memory cache of known-staged images. The cache is updated asynchronously by
the staging pipeline (on pull completion) and a background refresh task. The scheduler
never blocks on network I/O for readiness checks.

```
Each scheduling cycle:
  │
  ├─ Deferred resolution check (INV-SD4):
  │   For each Pending allocation with unresolved images:
  │     ├─ Try resolve via ImageResolver (async)
  │     ├─ Resolved → sink.resolve_image(alloc_id, idx, resolved)
  │     ├─ Timeout exceeded (submitted_at + resolve_timeout) → transition to Failed
  │     └─ Still unresolved → skip (defer to next cycle)
  │
  ├─ Image staging (INV-SD9):
  │   DataStager.plan_staging() now includes:
  │     - Data mount staging requests (existing)
  │     - Image staging requests (new: one per uncached resolved image)
  │
  └─ f5 scoring (INV-SD6):
      For each allocation × candidate node:
        readiness = min(data_readiness, image_readiness)
        image_readiness = ImageStager.readiness(image)  // sync, reads cache
          1.0 = cached on node or shared store
          0.5 = in registry, not cached (pull needed)
          0.0 = unresolved
```

## Node Agent: Prologue Extension (lattice-node-agent)

```
Existing prologue pipeline:
  1. Image cache check        ← extended: check ImageRef in cache
  2. Cgroup creation           ← unchanged
  3. Runtime prepare           ← extended: pull if needed, mount/setns
  4. ★ View activation         ← NEW: apply env_patches
  5. Memory policy             ← unchanged
  6. Data staging              ← unchanged (images staged separately)

View activation (step 4):
  For each EnvPatch in env_patches (declaration order, INV-SD7):
    Prepend → var = value + separator + existing
    Append  → var = existing + separator + value
    Set     → var = value
    Unset   → remove var
```

## Node Agent: PodmanRuntime (lattice-node-agent)

New runtime implementing the `Runtime` trait:

```rust
pub struct PodmanRuntime {
    containers: Arc<RwLock<HashMap<AllocId, PodmanState>>>,
    config: PodmanConfig,
}

pub struct PodmanConfig {
    pub podman_bin: String,
    pub podman_module: String,                  // "hpc"
    pub podman_tmp_path: String,                // "/dev/shm"
    pub parallax_imagestore: Option<String>,
    pub parallax_bin: Option<String>,
    pub parallax_mount_program: Option<String>,
    pub edf_search_paths: Vec<String>,
}
```

Lifecycle (INV-SD10):
```
prepare():
  1. Check Parallax shared store for image
  2. If missing: podman pull → parallax --migrate
  3. podman run -d --module hpc --label managed-by=lattice --label alloc-id=<id>
       [devices] [mounts] sh -c "kill -STOP $$ ; exit 0"
  4. Read container PID from podman inspect
  5. Store PodmanState { container_id, container_pid }
  6. Persist container_id in agent state file (for crash recovery)

spawn():
  IMPORTANT: The agent MUST NOT call setns() in its own async context.
  Namespace joining is delegated to nsenter(1):

  1. Build environment from env_patches
  2. Command::new("nsenter")
       --target <container_pid>
       --mount --user
       -- <entrypoint> [args...]
     with env vars set on the Command
  3. Spawned process is a child of the agent (via tokio::process::Command)
  4. Agent retains PID for signal delivery and exit status collection

  Rationale: setns() changes the calling thread's namespace. In a tokio
  async runtime with work-stealing, this would corrupt other tasks on the
  same thread. The nsenter binary forks, calls setns() in the child, then
  exec()s — safe for long-lived agents. This matches UenvRuntime's pattern.

stop():
  1. Signal spawned process (SIGTERM → grace → SIGKILL)
  2. podman stop <container_id>

cleanup():
  1. podman rm <container_id>
  2. Clean local artifacts

crash recovery (on agent restart):
  1. Load persisted state → find container_ids
  2. For each container_id: podman inspect → check if container exists
  3. If workload PID alive → reattach (resume heartbeating status)
  4. If workload PID dead but container exists → podman stop + podman rm
  5. Orphan scan: podman ps -a --filter label=managed-by=lattice
     → containers not in persisted state → stop + remove
```

## Proto Changes (proto/)

```protobuf
message ImageRef {
  string spec = 1;
  string image_type = 2;          // "uenv" or "oci"
  string registry = 3;
  string name = 4;
  string version = 5;
  string original_tag = 6;
  string sha256 = 7;
  uint64 size_bytes = 8;
  string mount_point = 9;
  bool resolve_on_schedule = 10;
}

message EnvPatch {
  string variable = 1;
  string op = 2;                  // "prepend", "append", "set", "unset"
  string value = 3;
  string separator = 4;
}

message ContainerSpec {
  repeated string base_environments = 1;
  repeated MountSpec mounts = 2;
  repeated string devices = 3;
  string workdir = 4;
  bool writable = 5;
  map<string, string> env = 6;
  map<string, string> annotations = 7;
}

message MountSpec {
  string source = 1;
  string target = 2;
  string options = 3;
}

// Updated EnvironmentSpec in allocations.proto
message EnvironmentSpec {
  repeated ImageRef images = 1;
  repeated EnvPatch env_patches = 2;
  repeated string devices = 3;
  repeated MountSpec mounts = 4;
  ContainerSpec container = 5;
  bool writable = 6;
  bool sign_required = 10;
  bool scan_required = 11;
  bool approved_bases_only = 12;
}
```

## Configuration (lattice-common)

```rust
pub struct SoftwareDeliveryConfig {
    /// uenv registry URL (ORAS/S3)
    pub uenv_registry: Option<String>,
    /// OCI registry URL (for container images)
    pub oci_registry: Option<String>,
    /// Parallax shared image store path
    pub parallax_imagestore: Option<String>,
    /// Parallax binary path
    pub parallax_bin: Option<String>,
    /// Podman binary path
    pub podman_bin: String,              // default: "podman"
    /// Podman module
    pub podman_module: String,           // default: "hpc"
    /// EDF system search paths
    pub edf_search_paths: Vec<String>,
    /// Deferred resolution timeout (seconds)
    pub resolve_timeout_secs: u64,       // default: 3600
    /// Node-local image cache size (bytes)
    pub image_cache_bytes: u64,          // default: 50GB
    /// Allowed registries (empty = all allowed)
    pub allowed_registries: Vec<String>,
}
```

## Error Taxonomy

```rust
pub enum ImageResolveError {
    NotFound { spec: String },
    RegistryUnavailable { url: String, reason: String },
    InvalidSpec { spec: String, reason: String },
    ViewNotFound { image: String, view: String },
    InheritanceCycle { chain: Vec<String> },
    InheritanceDepthExceeded { depth: usize, max: usize },
    SignatureInvalid { image: String },
    ScanRequired { image: String },
}

pub enum ImageStageError {
    PullFailed { image: String, reason: String },
    MigrateFailed { image: String, reason: String },
    StorageUnavailable { path: String },
    Timeout { image: String, elapsed_secs: u64 },
}
```

## Enforcement Map Additions

| Invariant | Module | Mechanism | Verified By |
|-----------|--------|-----------|-------------|
| INV-SD1 | lattice-api | `ImageResolver.resolve()` pins sha256 at submit | software_delivery.feature |
| INV-SD2 | lattice-api | `ImageResolver.metadata()` + view name check | software_delivery.feature |
| INV-SD3 | lattice-api | mount_point prefix check in `allocation_from_proto()` | software_delivery.feature |
| INV-SD4 | lattice-scheduler | timeout check in `run_once()` for unresolved images | software_delivery.feature |
| INV-SD5 | lattice-api | sensitive constraint validation at submit | software_delivery.feature |
| INV-SD6 | lattice-scheduler + node-agent | `DataStager` single request + `ImageStager.is_cached()` | software_delivery.feature |
| INV-SD7 | lattice-node-agent | prologue iterates `env_patches` in order | software_delivery.feature |
| INV-SD8 | lattice-api | depth counter + visited set in EDF resolution | software_delivery.feature |
| INV-SD9 | lattice-scheduler | `DataStager.should_prefetch()` checks images | software_delivery.feature |
| INV-SD10 | lattice-node-agent | `PodmanRuntime.spawn()` uses `setns()` + direct exec | software_delivery.feature |

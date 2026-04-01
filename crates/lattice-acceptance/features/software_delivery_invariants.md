# Software Delivery Invariants

Derived from analyst review of `docs/design/uenv-native-integration.md` and
`docs/design/sarus-suite-integration.md`. To be merged into `specs/invariants.md`
after architect review.

## New Domain Entities

### ImageRef
A content-addressed reference to a software image (uenv SquashFS or OCI container).

- `spec`: human-readable reference (`prgenv-gnu/24.11:v1`, `nvcr.io/nvidia/pytorch:24.01-py3`)
- `sha256`: content hash, pinned at resolution time (immutable after resolve)
- `size_bytes`: image size for cache-aware scheduling
- `registry`: source backend (ORAS/S3 for uenv, OCI registry for containers)
- `original_tag`: the mutable tag the user submitted (preserved for auditability)
- `mount_point`: where the image is mounted (uenv only)
- `image_type`: `Uenv` or `Oci`
- `resolve_on_schedule`: if true, defer resolution to scheduling time (CI pipeline pattern)

Scheduler treats ImageRef uniformly — `f₅` data readiness scores cache presence regardless
of image type.

### EnvPatch
A single environment variable modification, used by both uenv views and EDF env sections.

- `variable`: env var name (e.g., `PATH`)
- `operation`: `Prepend | Append | Set | Unset`
- `value`: value to apply
- `separator`: for list operations (default `:`)

Applied in declaration order. Last-write-wins for conflicts across views.

### ViewDef
A named set of EnvPatches extracted from a uenv's `env.json`.

- `name`: view name (e.g., `default`, `spack`)
- `description`: human-readable
- `patches`: ordered list of EnvPatch

### ContainerSpec
Full container specification, equivalent to a rendered EDF.

- `image`: OCI reference (resolved to ImageRef)
- `base_environments`: EDF inheritance chain (resolved at submit)
- `mounts`: bind mounts
- `devices`: CDI device specs
- `workdir`: container working directory
- `writable`: rootfs writability
- `env`: environment variables
- `annotations`: Podman annotations

### Revised Environment
The unified `Environment` on an `Allocation`:

- `images`: Vec<ImageRef> — one or more uenv and/or OCI images
- `env_patches`: Vec<EnvPatch> — resolved from views (uenv) or EDF (container)
- `devices`: Vec<String> — CDI device specs (typically only for containers)
- `mounts`: Vec<Mount> — additional bind mounts
- `writable`: bool — container rootfs writability (ignored for uenv-only)
- `sign_required`: bool — require image signatures (sensitive)
- `scan_required`: bool — require vulnerability scan (sensitive)
- `approved_bases_only`: bool — restrict to approved base images (sensitive)

## Invariants

### INV-SD1: Content-Addressed Image Pinning

**Statement:** After resolution, an allocation's ImageRef.sha256 is immutable. Re-pushing
a mutable tag to a different hash does not change the pinned image. The allocation runs
exactly the image that was resolved at submit time (or scheduling time for deferred).

**Violation consequence:** Different nodes in a multi-node job could run different image
versions. Non-reproducible results.

### INV-SD2: View Validation at Resolution Time

**Statement:** View names are validated against the image's metadata at resolution time.
Requesting a nonexistent view is a submit-time error (or scheduling-time error for
deferred resolution).

**Violation consequence:** Allocation reaches prologue, view activation fails, allocation
fails after consuming scheduler resources.

### INV-SD3: Mount Point Non-Overlap

**Statement:** When an allocation specifies multiple images, their mount points must not
overlap (no prefix containment). Validated at submit time.

**Violation consequence:** Mount shadowing — one image's files hidden by another's mount.
Unpredictable behavior.

### INV-SD4: Deferred Resolution Timeout

**Statement:** Allocations with `resolve_on_schedule: true` must resolve within a
configurable timeout (default: 1 hour). After timeout, allocation transitions to Failed
with reason "image resolution timeout".

**Violation consequence:** Unresolvable allocations occupy queue indefinitely, consuming
quota and confusing users.

### INV-SD5: Sensitive Image Integrity

**Statement:** Sensitive allocations require:
- `sign_required: true` (image signature verified at pull)
- Digest reference (`@sha256:...`), not mutable tags
- `writable: false` for containers
- Specific GPU indices (not `gpu=all`) in device specs

**Violation consequence:** Unsigned or tampered images on sensitive nodes. Data exfiltration
via writable rootfs. Unauditable GPU access.

### INV-SD6: Single Pull for Shared Store

**Statement:** For multi-node allocations, the image is pulled once to the shared store
(VAST/NFS for uenv, Parallax for OCI). Individual nodes read from the shared store.
There is never a per-node pull to a registry for images available in the shared store.

**Violation consequence:** 1000 nodes simultaneously hitting the registry. Registry
rate-limiting causes cascading timeouts. Minutes of wasted bandwidth.

### INV-SD7: Prologue Env Patch Ordering

**Statement:** Environment patches are applied in the order views are listed in the
allocation spec. For multi-image compositions, patches from the first image's views
are applied before the second image's views. Last-write-wins for conflicts.

**Violation consequence:** Non-deterministic PATH ordering. Different env state depending
on internal iteration order.

### INV-SD8: EDF Inheritance Bounded

**Statement:** EDF `base_environment` chains have a maximum depth of 10 levels.
Cycles are detected and rejected at submit time.

**Violation consequence:** Infinite resolution loop. API server hang or stack overflow.

### INV-SD9: Image Staging Reuses Data Mover

**Statement:** Image pre-staging uses the existing `DataStager` and `DataStageExecutor`
pipeline. Images are treated as staging requests alongside data mounts, prioritized
by allocation preemption class.

**Violation consequence:** Duplicate staging infrastructure. Inconsistent priority handling
between data and images.

### INV-SD10: Namespace Joining Preserves Agent Parentage

**Statement:** When using the Podman setns() pattern, the spawned workload process is
a direct child of the lattice-node-agent (not Podman). The agent retains signal delivery,
exit status collection, and cgroup membership control.

**Violation consequence:** Agent loses process management. Cannot signal, checkpoint, or
collect exit status. Zombie processes after Podman stop.

## Assumptions

### A-SD1: Shared Filesystem Available
**Assumption:** A shared filesystem (VAST NFS or S3-compatible) is available to all
compute nodes for the image store. Without it, each node pulls independently (degraded
but functional).
**If wrong:** Multi-node jobs have per-node pull overhead. Acceptable for small clusters,
unacceptable at scale (>100 nodes).
**Critical:** At scale, yes. For dev/small deployments, no.

### A-SD2: Podman >= 5.0 with HPC Module
**Assumption:** Podman >= 5.0 with `--module hpc` is available on compute nodes for
container workloads. Without it, container submissions fail.
**If wrong:** Fall back to old Sarus binary or reject container submissions. uenv path
unaffected.
**Critical:** Only for container workloads. uenv is the default and doesn't need Podman.

### A-SD3: squashfs-mount Binary Available
**Assumption:** The `squashfs-mount` binary is present on compute nodes for uenv mounts.
This is an existing assumption (ADR-003) that continues unchanged.
**If wrong:** uenv submissions fail at prologue. Containers still work.
**Critical:** For uenv workloads, yes.

### A-SD4: Registry Accessible from API Server
**Assumption:** The API server can reach the image registry (ORAS, OCI, S3) for
resolution at submit time. If registry is down, submissions with `resolve_on_schedule:
false` (default) are rejected.
**If wrong:** Users can't submit jobs during registry outage. Workaround: use
`resolve_on_schedule: true` if the image is known to exist.
**Critical:** During outage, yes. Mitigated by deferred resolution option.

### A-SD5: env.json in uenv Images
**Assumption:** uenv SquashFS images contain `env.json` at a well-known path describing
available views and their env patches. Images without `env.json` have no views (no env
patching, just mount).
**If wrong:** View activation fails silently (no patches applied). Users must set env
vars manually.
**Critical:** No — views are optional. Mount-only uenv is still useful.

### A-SD6: Parallax Optional
**Assumption:** Parallax (shared OCI image store) is recommended but not required.
Without it, each node pulls from the registry directly via Podman. This is slower but
functional.
**If wrong:** N/A — both paths work.
**Critical:** No.

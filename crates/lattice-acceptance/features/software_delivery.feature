Feature: Software Delivery — uenv and container lifecycle
  Lattice delivers software to allocations via two mechanisms:
  - uenv: SquashFS images mounted via mount namespace (lightweight, HPC-native)
  - containers: OCI images via Podman + namespace joining (isolated, third-party)

  Both can be used in the same allocation. Images are resolved at submit time
  (content-addressed by sha256), pre-staged during queue wait, and mounted
  during the allocation prologue.

  # ─── Image Resolution ─────────────────────────────────────

  Scenario: uenv image resolved at submit time
    Given a uenv registry with image "prgenv-gnu/24.11:v1" at sha256 "abc123"
    When I submit an allocation with uenv "prgenv-gnu/24.11:v1"
    Then the allocation spec should contain a resolved ImageRef
    And the ImageRef sha256 should be "abc123"
    And the ImageRef original_tag should be "v1"

  Scenario: OCI image resolved at submit time
    Given an OCI registry with image "nvcr.io/nvidia/pytorch:24.01-py3" at digest "sha256:def456"
    When I submit an allocation with container image "nvcr.io/nvidia/pytorch:24.01-py3"
    Then the allocation spec should contain a resolved ImageRef
    And the ImageRef sha256 should be "def456"

  Scenario: submit with nonexistent image fails fast
    Given an empty registry
    When I submit an allocation with uenv "nonexistent/image:v1"
    Then the submission should be rejected with "image not found"

  Scenario: deferred resolution for CI pipelines
    Given an empty registry
    When I submit an allocation with uenv "ci-build/latest:v1" and resolve_on_schedule true
    Then the allocation should be accepted in Pending state
    And the ImageRef should have sha256 empty

  Scenario: deferred resolution resolves when image appears
    Given an allocation pending with unresolved image "ci-build/latest:v1"
    When the image "ci-build/latest:v1" is pushed to the registry
    And the scheduler runs a cycle
    Then the ImageRef sha256 should be populated
    And the allocation should be eligible for scheduling

  Scenario: deferred resolution times out
    Given an allocation pending with unresolved image "ci-build/latest:v1"
    And the image resolution timeout is 1 hour
    When 1 hour passes without the image appearing
    Then the allocation should transition to Failed
    And the failure reason should contain "image resolution timeout"

  Scenario: mutable tag re-push does not change pinned sha256
    Given an allocation with resolved uenv "prgenv-gnu/24.11:v1" at sha256 "abc123"
    When the tag "v1" is re-pushed to sha256 "xyz789"
    Then the allocation's ImageRef sha256 should still be "abc123"

  # ─── View Activation ──────────────────────────────────────

  Scenario: uenv view activation applies env patches
    Given a uenv "prgenv-gnu/24.11:v1" with view "default" containing:
      | variable | operation | value                    |
      | PATH     | prepend   | /user-environment/bin    |
      | CUDA_HOME| set       | /user-environment/cuda   |
    When the prologue activates view "default"
    Then the process environment should have PATH starting with "/user-environment/bin"
    And CUDA_HOME should be "/user-environment/cuda"

  Scenario: multiple views applied in order
    Given a uenv "prgenv-gnu/24.11:v1" with views:
      | view    | variable    | operation | value              |
      | default | PATH        | prepend   | /env/bin           |
      | spack   | PATH        | prepend   | /spack/bin         |
    When the prologue activates views "default" then "spack"
    Then PATH should start with "/spack/bin:/env/bin"

  Scenario: nonexistent view rejected at submit time
    Given a uenv "prgenv-gnu/24.11:v1" with only view "default"
    When I submit an allocation with uenv "prgenv-gnu/24.11:v1" and view "nonexistent"
    Then the submission should be rejected with "view not found"

  Scenario: EDF env section applied to container
    Given a container with EDF env section:
      | variable   | value     |
      | NCCL_DEBUG | INFO      |
      | MY_VAR     | custom    |
    When the prologue prepares the container environment
    Then the process environment should contain NCCL_DEBUG="INFO"
    And the process environment should contain MY_VAR="custom"

  # ─── Multi-Image Composition ──────────────────────────────

  Scenario: multiple uenv images mounted at different paths
    Given uenv images:
      | spec                      | mount_point        |
      | prgenv-gnu/24.11:v1       | /user-environment  |
      | debug-tools/2024.1:v1     | /user-tools        |
    When I submit an allocation with both uenv images
    Then the allocation should have 2 ImageRefs
    And the mount points should not overlap

  Scenario: overlapping mount points rejected
    Given uenv images:
      | spec                  | mount_point        |
      | image-a/1.0:v1        | /opt/env           |
      | image-b/1.0:v1        | /opt/env           |
    When I submit an allocation with both uenv images
    Then the submission should be rejected with "overlapping mount points"

  Scenario: uenv and container used together
    Given a uenv "prgenv-gnu/24.11:v1" and container "pytorch:24.01-py3"
    When I submit an allocation with both
    Then the allocation should be accepted
    And the uenv should be mounted in the container's namespace

  Scenario: namespaced view references for multi-uenv
    Given uenv images "prgenv-gnu" and "debug-tools"
    When I submit with views "prgenv-gnu:default" and "debug-tools:perf"
    Then the prologue should apply "default" from "prgenv-gnu" first
    And then "perf" from "debug-tools"

  # ─── EDF Base Environment Inheritance ─────────────────────

  Scenario: EDF base_environment chain resolved
    Given system EDFs:
      | name     | contents                                |
      | base-gpu | devices: ["nvidia.com/gpu=all"]         |
      | base-mpi | mounts: ["/opt/cray/pe:/opt/cray/pe"]   |
    And a user EDF with base_environment ["base-gpu", "base-mpi"]
    When the EDF is rendered
    Then the resolved spec should include devices "nvidia.com/gpu=all"
    And the resolved spec should include mount "/opt/cray/pe"

  Scenario: EDF inheritance cycle detected
    Given system EDFs where "a" inherits from "b" and "b" inherits from "a"
    When I submit an allocation with EDF inheriting from "a"
    Then the submission should be rejected with "inheritance cycle"

  Scenario: EDF max depth exceeded
    Given a chain of 11 base_environment levels
    When I submit an allocation using the deepest EDF
    Then the submission should be rejected with "inheritance depth exceeded"

  # ─── Image Pre-staging ────────────────────────────────────

  Scenario: uenv image pre-staged during queue wait
    Given an allocation pending with uenv "prgenv-gnu/24.11:v1" (size 2GB)
    And the scheduler has tentatively selected node group 0
    When the data stager runs
    Then a staging request should be created for the uenv image
    And the staging priority should match the allocation priority

  Scenario: container image pre-staged via Parallax
    Given an allocation pending with container "pytorch:24.01-py3" (size 5GB)
    When the data stager runs
    Then a staging request should be created for the OCI image
    And the staging target should be the Parallax shared store

  Scenario: cached image skips staging
    Given an allocation pending with uenv "prgenv-gnu/24.11:v1"
    And the image is already in the node's NVMe cache
    Then the data stager should not create a staging request for this image
    And f5_data_readiness should score 1.0

  Scenario: image staging failure fails allocation
    Given an allocation in Staging state with uenv "broken-image:v1"
    When the image pull fails with "registry unavailable"
    Then the allocation should transition to Failed
    And the failure reason should contain "image pull failed"

  # ─── Container Lifecycle (Podman + setns) ─────────────────

  Scenario: Podman container started in detached mode
    Given an allocation with container "pytorch:24.01-py3"
    When the prologue prepares the container
    Then Podman should start with a self-stopping init process
    And the container PID should be recorded

  Scenario: namespace joining via setns
    Given a running Podman container with PID 42000
    When the agent joins the container namespace
    Then the spawned process should see the container filesystem
    And the process should still be a child of the lattice agent

  Scenario: container cleanup on allocation complete
    Given a running allocation with Podman container "abc123"
    When the allocation completes
    Then the epilogue should stop the Podman container
    And the container should be removed

  # ─── GPU Passthrough (CDI) ────────────────────────────────

  Scenario: CDI device spec passed to Podman
    Given an allocation with container requesting device "nvidia.com/gpu=all"
    When the prologue starts the container
    Then Podman should receive "--device nvidia.com/gpu=all"

  Scenario: sensitive allocation rejects gpu=all
    Given a sensitive allocation with container requesting device "nvidia.com/gpu=all"
    When I submit the allocation
    Then the submission should be rejected with "sensitive allocations must specify exact GPU indices"

  # ─── Sensitive Workload Extensions ────────────────────────

  Scenario: sensitive allocation requires signed image
    Given a sensitive tenant
    When I submit an allocation with unsigned uenv "untrusted:v1"
    Then the submission should be rejected with "image signature required"

  Scenario: sensitive allocation requires digest pinning
    Given a sensitive tenant
    When I submit an allocation with container "pytorch:latest" (mutable tag)
    Then the submission should be rejected with "sensitive allocations must use digest reference"

  Scenario: sensitive container enforces read-only rootfs
    Given a sensitive allocation with container spec writable=true
    When I submit the allocation
    Then the submission should be rejected with "sensitive containers must be read-only"

  # ─── Scheduler Data Readiness ─────────────────────────────

  Scenario: f5 scores image cache hit higher
    Given two nodes: node-A has cached "prgenv-gnu/24.11:v1", node-B does not
    And a pending allocation requesting "prgenv-gnu/24.11:v1"
    When the scheduler scores both nodes
    Then node-A should score higher on f5 than node-B

  Scenario: image size affects pull cost in f5
    Given a pending allocation with a 10GB container image
    And a pending allocation with a 100MB uenv image
    When both are scored on an empty-cache node
    Then the uenv allocation should score higher on f5

  # ─── CLI ──────────────────────────────────────────────────

  Scenario: submit with --uenv flag
    When I run "lattice submit --uenv prgenv-gnu/24.11:v1 --view default -- ./my_app"
    Then the allocation should have a uenv ImageRef for "prgenv-gnu/24.11:v1"
    And the views should contain "default"

  Scenario: submit with --image flag
    When I run "lattice submit --image nvcr.io/nvidia/pytorch:24.01-py3 -- python train.py"
    Then the allocation should have a container ImageRef

  Scenario: submit with --edf flag
    When I run "lattice submit --edf my-env.toml -- python train.py"
    Then the EDF should be parsed and rendered
    And the resolved ContainerSpec should be stored in the allocation

  Scenario: uenv cache list
    When I run "lattice uenv cache list --node nid001234"
    Then I should see the cached images on that node

  Scenario: uenv views query
    When I run "lattice uenv views prgenv-gnu/24.11:v1"
    Then I should see the available views with descriptions

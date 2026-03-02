# CLAUDE.md — Coding Context for Lattice

This file provides context for Claude (or any LLM) working on the Lattice codebase.
Read this first, then consult docs/architecture/ and docs/decisions/ for details.

## What is Lattice?

A distributed workload scheduler that sits between Slurm (HPC batch) and Kubernetes (cloud services). It schedules both finite jobs (training runs, simulations) and infinite jobs (inference services, monitoring) on a shared HPC infrastructure with:
- Slingshot/Ultra Ethernet interconnect
- GPU-accelerated nodes (NVIDIA GH200, AMD MI300X, etc.)
- VAST-like tiered storage (NFS + S3)
- OpenCHAMI infrastructure management (Redfish BMC)
- uenv software delivery (SquashFS mount namespaces)
- Medical/regulated workload isolation requirements

## Architecture Summary

### Control Plane
- **Raft Quorum** (3-5 replicas): Strong consistency for node ownership + medical audit
- **vCluster Schedulers**: Per-workload-type schedulers (HPC backfill, service bin-pack, medical reservation, interactive FIFO)
- Each vCluster scheduler proposes allocations → quorum validates and commits

### Scheduling Algorithm
Multi-dimensional knapsack with composite cost function:
```
Score(j) = Σ wᵢ · fᵢ(j)
  f₁ = priority_class         (preemption tier)
  f₂ = wait_time_factor       (anti-starvation, ages with queue time)
  f₃ = fair_share_deficit      (tenant equity)
  f₄ = topology_fitness        (Slingshot dragonfly group packing)
  f₅ = data_readiness          (is input data on hot tier?)
  f₆ = backlog_pressure        (queue depth)
  f₇ = energy_cost             (time-varying electricity price)
  f₈ = checkpoint_efficiency   (preemption cost)
  f₉ = conformance_fitness     (node config homogeneity)
```
Weights are tunable per vCluster. Use RM-Replay simulator to test weight changes before production.

### Consistency Model
- **Strong (Raft-committed)**: (1) Node ownership (2) Medical audit state
- **Eventually consistent**: Job queues, telemetry, quota accounting, session state

### Key Abstractions
- **Allocation**: The universal work unit (replaces Slurm job + K8s pod)
  - lifecycle: bounded (batch) | unbounded (service) | reactive (autoscale)
  - Has resources, constraints, dependencies, network domain, uenv
- **vCluster**: A view/projection of resources with its own scheduler policy
- **Task Group**: Equivalent of Slurm job arrays
- **DAG**: Directed acyclic graph of allocations with dependency edges (Slurm-compatible: afterok, afternotok, afterany, aftercorr)
- **Network Domain**: Allocations sharing a domain get L3 reachability (Slingshot VNI)
- **Tenant**: Organizational boundary (quotas, isolation, audit)

### Two-Tier API
1. **Intent API** (agent-native): Agents declare what they need, scheduler resolves how
2. **Compatibility API** (Slurm-like): sbatch/squeue/scancel mapped to Intent API

### Software Delivery
- **Default: uenv** — SquashFS images mounted via mount namespace (near-zero overhead)
- **When needed: Sarus** — OCI containers for isolation, third-party images
- **Registry**: JFrog/Nexus → S3 backing, optional node-local NVMe cache

### Storage Integration
- Hot tier: VAST (NFS + S3), scheduler can set QoS, pre-stage data, snapshot
- Warm/Cold: tiered, S3-compatible
- Medical: encrypted pool, access-logged, wipe-on-release
- Data mover: pre-stages during queue wait time (invisible to user)

### Telemetry
- Collection: eBPF (always-on, <0.3% overhead)
- Aggregation: switchable resolution (prod: 30s bicubic, debug: 1s raw, audit: access logs)
- Storage: time-series store, three views (holistic, tenant, vCluster)
- Feeds back into cost function (GPU util, network congestion, I/O patterns)

### Checkpointing
Scheduler-coordinated with cost function:
- checkpoint when Value(recompute_saved + preemptability + backlog_relief) > Cost(write_time + compute_waste + storage_cost)
- Backlog pressure increases checkpoint aggressiveness
- Applications implement checkpoint API (signal, shmem flag, or gRPC callback)
- Fallback: DMTCP transparent checkpointing or non-preemptible flag

### Federation (Optional — pluggable via feature flag)
- Sovra for cryptographic trust (federated sovereign key management)
- Loose coupling: federation broker suggests, local scheduler decides
- Data gravity drives placement
- Medical data sovereignty enforced (data stays at designated site)
- System fully functional without federation

### Medical Workload Model
- User (not cluster) claims nodes → quorum records user identity as owner
- Dedicated nodes, no sharing, hardened OS image via OpenCHAMI
- Encrypted storage pool, all access logged, wipe on release
- Signed uenv images only, vulnerability-scanned
- 7-year audit retention
- Risk-averse: no clever optimizations on medical resources

## Code Organization

### Rust Workspace (crates/)
All performance-critical components. Shared protobuf types generated into lattice-common.

- `lattice-common`: Shared types (Allocation, Node, Tenant, vCluster), config, errors, protobuf bindings
- `lattice-quorum`: Raft consensus implementation, global state machine, node ownership, medical audit log
- `lattice-scheduler`: vCluster scheduler trait + implementations (HPC backfill, service bin-pack, medical, interactive), knapsack solver, cost function, topology model
- `lattice-node-agent`: Per-node daemon, Sarus/uenv lifecycle, eBPF telemetry loading, health reporting, checkpoint signal forwarding
- `lattice-api`: gRPC server (tonic) + REST gateway, Intent API + Compatibility API endpoints
- `lattice-cli`: `lattice` CLI binary, subcommands for submit/status/cancel/session/telemetry, Slurm compat aliases
- `lattice-checkpoint`: Checkpoint broker, cost function evaluator, application protocol (signal/shmem/gRPC)

### Proto (proto/)
Protobuf definitions are the API contract. Generate Rust (tonic/prost), Python (grpcio), and Go (if needed) bindings.

### Python SDK (sdk/python/)
User-facing SDK for agents and notebooks. Wraps gRPC client with Pythonic interface.

### Infrastructure (infra/)
- OpenCHAMI integration configs and deployment
- eBPF programs for telemetry collection
- Telemetry pipeline configuration

### Tools (tools/)
- RM-Replay: Scheduler simulator for testing cost function weights
- Compat layer: Slurm command translation (may merge into lattice-cli)

## Design Decisions

All major decisions are recorded as ADRs in docs/decisions/. Key ones:
- ADR-001: Raft for quorum consensus
- ADR-002: Knapsack scheduling with composite cost function
- ADR-003: uenv-first software delivery
- ADR-004: Two strong consistency domains only
- ADR-005: Federation as opt-in via Sovra
- ADR-006: Rust for scheduler core
- ADR-007: Full-node scheduling with intra-node packing
- ADR-008: Asynchronous accounting via Waldur
- ADR-009: Two-tier quota enforcement

### Architecture Docs (docs/architecture/)
Detailed design documents: system-architecture, api-design, scheduling-algorithm, telemetry, observability, sensitive-workloads, checkpoint-broker, conformance, data-plane, federation, failure-modes, security, upgrades, gpu-topology, quota-enforcement, dag-scheduling, autoscaling, accounting, node-lifecycle, preemption, data-staging, deployment, sessions, cli-design, slurm-migration, testing-strategy, network-domains, tuning-guide, troubleshooting.

## Coding Conventions

- Rust: stable toolchain, 2021 edition, `cargo fmt` + `cargo clippy`
- Error handling: `thiserror` for library crates, `anyhow` for binaries
- Async: `tokio` runtime
- gRPC: `tonic` + `prost`
- Testing: unit tests in-module, integration tests in tests/
- Protobuf: `buf` for linting and generation
- Python: `ruff` for linting, `pytest` for testing

## Key External Dependencies

| Dependency | Purpose | Notes |
|---|---|---|
| OpenCHAMI | Infrastructure management | Go, separate deployment, API integration |
| Sovra | Federation trust | Go, optional, pluggable |
| FirecREST | User API gateway | Sits in front of lattice-api |
| uenv/squashfs-mount | Software delivery | C, existing binary, integrated by node agent |
| Sarus | OCI container runtime | C++, existing, used by node agent |
| libfabric | Network abstraction | C, Slingshot/UE interface |
| VAST API | Storage integration | REST API calls from scheduler |
| Waldur | External accounting & billing | REST API, optional, pluggable |

## What NOT to Do

- Don't add Kubernetes dependencies — this is not a K8s extension
- Don't implement a full container orchestrator — Sarus/uenv handle execution
- Don't build a storage system — integrate with VAST/Lustre via their APIs
- Don't reimplement Raft from scratch — use a proven library (openraft)
- Don't make federation mandatory — it's always feature-gated
- Don't optimize for microservice scale — this schedules tens-to-hundreds of large allocations, not millions of pods

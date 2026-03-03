# Lattice

A distributed workload scheduler for large-scale scientific computing, AI/ML training, inference services, and regulated workloads.

Lattice schedules both finite jobs (batch training, simulations) and infinite jobs (inference services, monitoring) on shared HPC infrastructure with topology-aware placement, federated multi-site operation, and a unified API for human users and autonomous agents.

## Architecture at a Glance

```
User Plane         FirecREST API Gateway (OIDC/SAML)
Software Plane     uenv (SquashFS) + Sarus (OCI) + Registry
Scheduling Plane   Raft Quorum + vCluster Schedulers (knapsack)
Data Plane         VAST (NFS/S3) tiered storage + data mover
Network Fabric     Slingshot / Ultra Ethernet (libfabric)
Node Plane         Node Agent + mount namespaces + eBPF telemetry
Infrastructure     OpenCHAMI (Redfish BMC, boot, inventory)
```

Start with [System Architecture](architecture/system-architecture.md) for the full picture, or jump to [API Design](architecture/api-design.md) to see how users interact with the system.

## Source Code

The project is organized as a Rust workspace with 9 crates:

| Crate | Purpose |
|---|---|
| `lattice-common` | Shared types, config, protobuf bindings |
| `lattice-quorum` | Raft consensus, global state machine, audit log |
| `lattice-scheduler` | vCluster schedulers, knapsack solver, cost function |
| `lattice-api` | gRPC + REST server, OIDC, RBAC, mTLS |
| `lattice-checkpoint` | Checkpoint broker, cost evaluator |
| `lattice-node-agent` | Per-node daemon, GPU discovery, eBPF telemetry |
| `lattice-cli` | CLI binary (submit, status, cancel, session, telemetry) |
| `lattice-test-harness` | Shared mocks, fixtures, builders |
| `lattice-acceptance` | BDD scenarios and property tests |

Plus a [Python SDK](https://github.com/witlox/lattice/tree/main/sdk/python), an [RM-Replay simulator](https://github.com/witlox/lattice/tree/main/tools/rm-replay), and deployment configs in `infra/`.

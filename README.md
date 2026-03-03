# Lattice

**A scheduler for HPC and AI workloads.**

Lattice is a distributed workload scheduler designed for large-scale scientific computing, AI/ML training, inference services, and sensitive/regulated workloads. It supports both finite (batch) and infinite (service) jobs on full-node allocations, with topology-aware placement, federated multi-site operation, and a unified API for both human users and autonomous agents.

## Key Design Principles

- **Full-node scheduling with intra-node packing** — the scheduler reasons about whole nodes; the node agent handles container/process packing via Sarus and uenv
- **Intent-based API with Slurm compatibility** — agents submit intents, users can use familiar `sbatch`-like commands
- **Distributed control plane** — Raft quorum for global state, per-vCluster schedulers for workload-specific policies
- **uenv-native software delivery** — SquashFS-based user environments as the default, OCI containers when isolation is needed
- **Medical/regulated workload support** — user-level node claims, dedicated nodes, encrypted storage, full audit trail
- **Federation as opt-in** — multi-site operation via Sovra trust layer, system fully functional without it

## Architecture Overview

```
User Plane:       FirecREST API Gateway (OIDC/SAML)
Software Plane:   uenv (squashfs) + Sarus (OCI) + Registry
Scheduling Plane: Raft Quorum + vCluster Schedulers (knapsack)
Data Plane:       VAST (NFS/S3) tiered storage + data mover
Network Fabric:   Slingshot / Ultra Ethernet (libfabric)
Node Plane:       Node Agent + squashfs-mount + eBPF telemetry
Infrastructure:   OpenCHAMI (Redfish BMC, boot, inventory)
```

See [docs/architecture/](docs/architecture/) for detailed design documents.

## Repository Structure

```
lattice/
├── CLAUDE.md             # Claude coding context 
├── crates/               # Rust workspace
│   ├── lattice-common/   # Shared types, config, error handling
│   ├── lattice-quorum/   # Raft consensus + global state
│   ├── lattice-scheduler/# vCluster scheduler (knapsack solver)
│   ├── lattice-node-agent/# Per-node daemon
│   ├── lattice-api/      # gRPC + REST API server
│   ├── lattice-cli/      # CLI (lattice command + compat layer)
│   └── lattice-checkpoint/# Checkpoint broker
├── proto/                # Protobuf definitions (the API contract)
├── sdk/
│   └── python/           # Python SDK + agent framework
├── infra/
│   ├── openchami/        # OpenCHAMI integration configs
│   └── telemetry/        # eBPF programs, telemetry pipeline
├── tools/
│   ├── rm-replay/        # Scheduler simulator (Python)
│   └── compat-layer/     # Slurm compatibility translation
├── tests/
│   ├── integration/      # Cross-crate integration tests
│   └── e2e/              # End-to-end cluster tests
└── docs/                 # All design documentation
    ├── architecture/     # System design documents
    ├── decisions/        # Architecture Decision Records (ADRs)
    ├── references/       # External references and research
    └── runbooks/         # Operational procedures
```

## Technology Stack

| Component | Language | Rationale |
|---|---|---|
| Scheduler core (quorum, schedulers, node agent, API, CLI) | Rust | Memory safety, no GC pauses, correctness-critical |
| Infrastructure services (OpenCHAMI, federation broker) | Go | Ecosystem alignment with OpenCHAMI/Sovra |
| User-facing SDK, agents, simulator | Python | ML ecosystem, scientific workflow community |
| eBPF telemetry programs | C/BPF | Kernel interface requirement |
| squashfs-mount, Sarus | C/C++ | Existing components, integrated as-is |

## Building

```bash
# Rust workspace
cargo build --workspace

# Python SDK
cd sdk/python && pip install -e .

# Protobuf generation
cd proto && buf generate
```

## Status

🚧 **Pre-alpha** — Architecture design phase. See [docs/decisions/](docs/decisions/) for current ADRs.

## License

Apache-2.0 (TBD)

## References

- [CSCS Alps vCluster architecture](https://arxiv.org/abs/2507.02404)
- [OpenCHAMI](https://openchami.org)
- [FirecREST](https://github.com/eth-cscs/firecrest)
- [uenv](https://github.com/eth-cscs/uenv)
- [Sarus](https://github.com/eth-cscs/sarus)
- [Sovra](https://github.com/witlox/sovra)
- [Ultra Ethernet Consortium](https://ultraethernet.org)

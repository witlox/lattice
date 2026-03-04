# External References

## Core Infrastructure Projects

### OpenCHAMI
- **What:** Open-source HPC system management platform (provisioning, boot, inventory)
- **Repo:** https://github.com/OpenCHAMI
- **Docs:** https://openchami.org
- **Components we integrate with:** SMD (State Management Daemon), BSS (Boot Script Service), Magellan (Redfish discovery), OPAAL (auth), Cloud-init
- **Founded by:** LANL, NERSC, CSCS, HPE, University of Bristol
- **Language:** Go
- **Our integration:** Infrastructure plane — Lattice queries SMD for node inventory, triggers BSS for boot image selection (e.g., sensitive hardened image), uses Magellan for hardware discovery

### FirecREST
- **What:** RESTful API gateway for HPC systems
- **Repo:** https://github.com/eth-cscs/firecrest
- **Docs:** https://firecrest.readthedocs.io
- **Our integration:** User plane — sits in front of lattice-api, handles OIDC authentication, provides WebSocket terminal for interactive sessions, file management

### uenv
- **What:** User environment tool for mounting SquashFS software stacks
- **Repo:** https://github.com/eth-cscs/uenv
- **Related:** https://github.com/eth-cscs/squashfs-mount (setuid mount binary), https://github.com/eth-cscs/slurm-uenv-mount (Slurm SPANK plugin)
- **Docs:** https://docs.cscs.ch/software/uenv/using/
- **Key properties:** SquashFS images, mount namespace isolation (per-process-tree), setuid binary (not FUSE), Spack-built stacks via Stackinator, multiple mount points (/user-environment, /user-tools)
- **Our integration:** Software plane — node agent uses squashfs-mount to deliver uenv to allocations. We replace the Slurm SPANK plugin with native node agent integration.

### Sarus
- **What:** OCI-compliant container runtime for HPC
- **Repo:** https://github.com/eth-cscs/sarus
- **Key properties:** Near-native performance, direct GPU/interconnect access via OCI hooks, no network namespace overhead for MPI
- **Our integration:** Software plane — used when full container isolation is needed (multi-tenant node sharing, third-party images, sensitive workloads with enhanced isolation)

### Sovra
- **What:** Federated sovereign key management for critical infrastructure
- **Repo:** https://github.com/witlox/sovra
- **Docs:** https://witlox.github.io/sovra/
- **Key properties:** Peer-to-peer control planes, customer-controlled root keys, OPA-based policy, air-gap capable, cross-domain sharing
- **Language:** Go
- **Our integration:** Federation trust layer (optional, feature-gated). Provides cross-site authentication, sensitive data encryption key management, audit log signing.

## Networking

### Slingshot (HPE CXI)
- **What:** HPE's HPC interconnect, dragonfly topology
- **Key properties:** Hardware traffic classes, VNIs for isolation, high-radix switches, RDMA
- **Scheduler relevance:** Topology-aware placement (minimize inter-group hops), VNI-based network domains, separate traffic classes for compute/management/telemetry

### Ultra Ethernet Consortium (UEC)
- **What:** Open Ethernet-based networking stack for AI/HPC
- **Spec:** https://ultraethernet.org (1.0 released June 2025)
- **Key properties:** UET transport (native RDMA over Ethernet), packet spraying (adaptive multi-path), CSIG (in-band congestion signaling), built-in encryption, libfabric 2.0 API
- **Relationship to Slingshot:** ~75% of UET derives from Slingshot transport. Migration path is evolutionary, not revolutionary.
- **Scheduler relevance:** CSIG feeds into telemetry (congestion-aware scheduling), encryption simplifies sensitive compliance, libfabric abstraction enables fabric-agnostic scheduler

### libfabric
- **What:** Fabric abstraction library (provider-based: CXI for Slingshot, EFA for AWS, verbs for InfiniBand, UET for Ultra Ethernet)
- **Our integration:** Network fabric abstraction. The scheduler and node agent interact with the network via libfabric, making the scheduler fabric-agnostic.

## Storage

### VAST Data Platform
- **What:** All-flash unified storage (NFS + S3 + block), DASE architecture
- **Key properties:** Multiprotocol (NFS + S3 native), RESTful API for everything, QoS per export, auto-indexing catalog, snapshots, DataSpace (global namespace with prefetch)
- **Scheduler integration:** QoS setting at job start, data locality queries via Catalog API, pre-staging via DataSpace prefetch, snapshots for reproducibility, audit logs for sensitive compliance

### IBM Storage Scale (GPFS)
- **What:** Parallel file system with extensive management features
- **Key properties:** Placement policies, AFM (async data management), filesets with quotas, watch/callback API, transparent cloud tiering
- **Scheduler integration:** Alternative to VAST. Fileset-per-job for isolation, placement policies for workload-specific tuning, AFM for remote data staging.

## Research Papers

### CSCS Alps Architecture
- Martinasso, Klein, Schulthess. "Alps, a versatile research infrastructure." CUG 2025. [arXiv:2507.02404](https://arxiv.org/abs/2507.02404)
- Alam, Gila, Klein, Martinasso, Schulthess. "Versatile software-defined HPC and cloud clusters on Alps supercomputer for diverse workflows." IJHPCA 2023.
- Martinasso et al. "Resource Elasticity for Scientific Platforms on HPC Infrastructure." Springer 2025.

### Scheduler Simulation
- Martinasso, Gila, Bianco, Alam, McMurtrie, Schulthess. "RM-Replay: A High-Fidelity Tuning, Optimization and Exploration Tool for Resource Management." SC18. [Repo](https://github.com/eth-cscs/slurm-replay)

### Multi-Objective Scheduling
- Simon, Nguyen, Halem. "Multiple Objective Scheduling of HPC Workloads Through Dynamic Prioritization." Uses bounded fractional knapsack with dynamic priority scoring.
- Goponenko. "Objective-Driven Strategies for HPC Job Scheduling." UCF 2024. Comprehensive metrics for scheduling quality, I/O-aware backfill.

### Energy-Aware Federation
- "Power-Aware Scheduling for Multi-Center HPC Electricity Cost Optimization." arXiv:2503.11011. GNN-based power prediction + multi-site scheduling, up to 18% energy cost reduction.

### uenv Deployment
- Coles et al. "Deploying Alternative User Environments on Alps." CUG 2023. Details squashfs-mount, Slurm SPANK plugin, Spack stack building.

### ML on HPC
- CSCS. "Evolving HPC services to enable ML workloads on HPE Cray EX." CUG 2025. [arXiv:2507.01880](https://arxiv.org/html/2507.01880). Container Engine, Environment Definition Files, gaps for ML users.

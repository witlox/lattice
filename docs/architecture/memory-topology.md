# Memory Topology

## Design Principle

Vendor-neutral abstraction over CPU-memory-GPU memory topology. The scheduler reasons about "memory domains" and "interconnect bandwidth," not vendor-specific terms like NUMA node IDs or NVLink-C2C. Node agents discover and report memory topology; the scheduler uses it for placement decisions and memory policy configuration.

This complements [gpu-topology.md](gpu-topology.md), which models GPU interconnects. Memory topology models the CPU-memory-GPU memory hierarchy: NUMA domains, unified memory architectures, and CXL-attached memory tiers.

## Memory Domain Types

| Type | Hardware Example | Characteristics | Discovery |
|------|-----------------|-----------------|-----------|
| Discrete NUMA | Multi-socket Intel Xeon, AMD EPYC | Separate DRAM per socket, asymmetric access latencies | `/sys/devices/system/node/` |
| Unified CPU-GPU | NVIDIA Grace Hopper GH200 | NVLink-C2C coherent, single address space across CPU and GPU | NVML + `/sys/devices/system/node/` |
| APU / Unified Die | AMD MI300A | CPU + GPU on same package, shared HBM3 pool | ROCm-SMI + `hwloc` |
| CXL-Attached | CXL Type 3 memory expanders | Pooled or device-attached memory, higher latency than local DRAM | `/sys/bus/cxl/` |
| Single-Socket | Single-socket servers | Trivial: one NUMA node, uniform access | `/sys/devices/system/node/` |

## Abstraction Model

```
MemoryTopology {
    domains: Vec<MemoryDomain>,
    interconnects: Vec<MemoryInterconnect>,
    total_capacity_bytes: u64,
}

MemoryDomain {
    id: u32,
    domain_type: MemoryDomainType,    // Dram | Hbm | CxlAttached | Unified
    capacity_bytes: u64,
    numa_node: Option<u32>,           // Linux NUMA node ID, if applicable
    attached_cpus: Vec<u32>,          // CPU IDs with local access
    attached_gpus: Vec<u32>,          // GPU indices with local/coherent access
}

MemoryInterconnect {
    domain_a: u32,
    domain_b: u32,
    link_type: MemoryLinkType,        // NumaLink | CxlSwitch | CoherentFabric
    bandwidth_gbps: f64,
    latency_ns: u64,
}

enum MemoryDomainType { Dram, Hbm, CxlAttached, Unified }
enum MemoryLinkType { NumaLink, CxlSwitch, CoherentFabric }
```

The node agent populates this structure at startup alongside `GpuTopology` and reports it with node capabilities and health data.

## Interconnect Bandwidth and Latency

| Link Type | Typical Bandwidth | Typical Latency | Notes |
|-----------|-------------------|-----------------|-------|
| Local DRAM access | 50-100 GB/s per channel | ~80 ns | Same-socket, same NUMA node |
| Remote NUMA (UPI/xGMI) | 20-40 GB/s | ~150-300 ns | Cross-socket, 1.5-3x local latency |
| NVLink-C2C (GH200) | 900 GB/s | ~100 ns | CPU-GPU coherent fabric |
| Infinity Fabric (MI300A) | 896 GB/s aggregate | ~100 ns | On-package CPU-GPU interconnect |
| CXL 2.0 (Type 3) | 32-64 GB/s | ~200-400 ns | Memory expander, higher latency |
| PCIe Gen5 (discrete GPU) | 64 GB/s | ~1-2 us | Non-coherent, requires explicit transfer |

Actual bandwidth and latency are discovered at runtime, not hardcoded.

## Superchip Architectures

### NVIDIA Grace Hopper (GH200)

Grace CPU + Hopper GPU connected via NVLink-C2C (900 GB/s bidirectional). The CPU and GPU share a single coherent address space — no explicit `cudaMemcpy` required for data movement.

```
┌──────────────────────────────────────────────────┐
│                  GH200 Superchip                  │
│                                                   │
│  ┌─────────────┐   NVLink-C2C    ┌────────────┐  │
│  │  Grace CPU  │◄──900 GB/s───►│  Hopper GPU │  │
│  │  72 cores   │   coherent      │  80 GB HBM3 │  │
│  │  512 GB LPDDR5X              │             │  │
│  └─────────────┘                 └────────────┘  │
│                                                   │
│  Single coherent address space (CPU + GPU)        │
│  → Maps to one Unified MemoryDomain               │
└──────────────────────────────────────────────────┘
```

**Mapping to abstraction:**
- One `MemoryDomain { type: Unified }` spanning CPU LPDDR5X + GPU HBM3
- `attached_cpus`: all Grace cores; `attached_gpus`: [Hopper GPU index]
- One `MemoryInterconnect { type: CoherentFabric, bandwidth: 900 }` between CPU and GPU sub-domains

### AMD Instinct MI300A

APU with CDNA 3 GPU + Zen 4 CPU on the same package, sharing HBM3 memory pool. No discrete CPU DRAM — all memory is HBM3 accessible by both CPU and GPU.

```
┌──────────────────────────────────────────────────┐
│                  MI300A Package                    │
│                                                   │
│  ┌─────────────┐  Infinity    ┌────────────────┐ │
│  │  Zen 4 CPU  │◄──Fabric──►│  CDNA 3 GPU    │ │
│  │  24 cores   │  896 GB/s    │  6 XCDs        │ │
│  └──────┬──────┘              └───────┬────────┘ │
│         │                             │           │
│         └──────┐          ┌───────────┘           │
│                ▼          ▼                        │
│         ┌─────────────────────┐                   │
│         │  Shared HBM3 Pool   │                   │
│         │  128 GB              │                   │
│         └─────────────────────┘                   │
│                                                   │
│  → Maps to one Unified MemoryDomain               │
└──────────────────────────────────────────────────┘
```

**Mapping to abstraction:**
- One `MemoryDomain { type: Unified }` for the shared HBM3 pool
- `attached_cpus`: all Zen 4 cores; `attached_gpus`: [MI300A GPU index]
- Internal Infinity Fabric interconnect is not separately modeled (on-package, always present)

## Discovery

The node agent discovers memory topology at startup using platform-specific sources:

| Source | What It Provides | Platform |
|--------|-----------------|----------|
| `/sys/devices/system/node/` | NUMA node count, CPU-to-node mapping, memory per node | Linux (all) |
| `numactl --hardware` | NUMA distances (latency matrix between nodes) | Linux (all) |
| `hwloc` | Portable topology discovery, cache hierarchy, PCI locality | Linux (all) |
| NVML | GPU-to-NUMA affinity, NVLink-C2C detection (GH200) | NVIDIA GPUs |
| ROCm-SMI | GPU-to-NUMA affinity, MI300A detection | AMD GPUs |
| `/sys/bus/cxl/` | CXL device enumeration, memory regions, interleave config | CXL-capable systems |

### Superchip Detection

GH200 and MI300A superchips are identified by GPU model string during GPU discovery (cross-ref: [gpu-topology.md](gpu-topology.md)). When detected:

1. The node agent queries the coherent memory size via vendor API (NVML for GH200, ROCm-SMI for MI300A)
2. NUMA nodes associated with both CPU and GPU are merged into a single `Unified` domain
3. The coherent interconnect bandwidth is reported as a `CoherentFabric` link

### Discovery Fallback

If vendor APIs are unavailable (e.g., driver not loaded), the node agent falls back to `hwloc` for topology and reports `Dram` domains only. GPU memory domains are still reported via the GPU topology path but without coherent interconnect metadata.

## Scheduling Impact

### Extending f₄ (topology_fitness)

Memory topology extends the intra-node component of f₄ alongside GPU topology:

```
intra_node_fitness = β · gpu_link_fitness + (1-β) · memory_locality_fitness

memory_locality_fitness(j, selected_nodes) =
    average over selected nodes of:
        fraction of allocation's CPUs and GPUs in the same memory domain

β = 0.7 for GPU-heavy workloads (GPU interconnect dominates)
β = 0.3 for CPU-heavy workloads with GPU offload (memory locality dominates)
β = 0.5 default
```

### Constraint Hints

Allocations can specify memory topology preferences:

| Constraint | Effect |
|-----------|--------|
| `prefer_same_numa` | Soft: prefer placing all CPUs in a single NUMA domain |
| `require_unified_memory` | Hard: only schedule on nodes with `Unified` memory domains (GH200, MI300A) |
| `prefer_local_memory` | Soft: prefer NUMA-local memory allocation policy |
| `allow_cxl_memory` | Opt-in: allow scheduling on CXL-expanded memory capacity |

Hard constraints filter nodes before the knapsack solver runs. Soft constraints contribute to `memory_locality_fitness`.

### Intra-Node CPU-GPU Co-location

On discrete NUMA systems (e.g., dual-socket with 4 GPUs per socket), the node agent co-locates an allocation's CPU cores and GPUs within the same NUMA domain when possible:

```
For an allocation requesting k CPUs and g GPUs on a multi-NUMA node:
1. Identify NUMA domains that have both free CPUs and GPUs with local affinity
2. Prefer the domain where GPU-to-NIC affinity is best (for inter-node traffic)
3. Assign CPUs and GPUs from the same domain via cgroup/cpuset
4. If the allocation spans domains: prefer domains connected by highest-bandwidth link
```

## Memory Mapping Policies

The node agent configures memory allocation policy at allocation start via `numactl` (or equivalent). This is transparent to the user unless they specify a preference.

| Policy | `numactl` Flag | When Used |
|--------|----------------|-----------|
| Local | `--localalloc` | Default: allocate on the NUMA node where the thread runs |
| Interleave | `--interleave=all` | Large shared datasets that all threads access equally |
| Preferred | `--preferred=<node>` | Pin to a specific NUMA node (for known data locality) |
| Bind | `--membind=<nodes>` | Strict: only allocate from specified nodes (sensitive isolation) |

On unified memory architectures (GH200, MI300A), NUMA policy has reduced impact since CPU and GPU share the same memory pool. The node agent skips `numactl` configuration for allocations on unified nodes unless the user explicitly requests a policy.

### Allocation-Level Override

Users can specify memory policy in the allocation request:

```yaml
resources:
  cpus: 24
  gpus: 1
  memory_gb: 128
constraints:
  memory_policy: interleave    # optional: local | interleave | preferred | bind
  require_unified_memory: true  # optional: only unified architectures
```

## CXL Memory Tiers

CXL Type 3 memory expanders add a new capacity tier: higher latency than local DRAM but lower cost per GB. The scheduler treats CXL memory as a separate resource dimension.

### Capacity Model

```
Node memory capacity:
  local_dram_bytes:  512 GB  (fast, NUMA-local)
  cxl_memory_bytes:  2 TB    (slower, CXL-attached)
  total_bytes:       2.5 TB

Allocation can request:
  memory_gb: 256              # scheduler satisfies from local DRAM
  memory_gb: 1024             # scheduler must use CXL tier (exceeds local DRAM)
  memory_gb: 1024
  allow_cxl_memory: true      # explicit opt-in for CXL tier
```

### Scheduling Rules

1. By default, allocations are placed using local DRAM capacity only
2. If `allow_cxl_memory: true`, CXL capacity is included in available memory
3. Allocations requesting more memory than local DRAM are only placed on CXL-capable nodes when the constraint is set
4. CXL memory appears as a separate `CxlAttached` domain in `MemoryTopology`

## Cross-References

- [gpu-topology.md](gpu-topology.md) — GPU interconnect topology, NIC affinity, intra-node GPU selection
- [telemetry.md](telemetry.md) — NUMA locality metrics collection (eBPF), memory utilization
- [scheduling-algorithm.md](scheduling-algorithm.md) — f₄ topology_fitness, knapsack solver, constraint handling
- [node-lifecycle.md](node-lifecycle.md) — Node agent startup, health reporting, capability discovery
- [conformance.md](conformance.md) — Hardware configuration fingerprint (includes memory architecture)

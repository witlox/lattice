# GPU Topology

## Design Principle

Vendor-neutral abstraction over GPU interconnect topologies. The scheduler reasons about "GPU domains" and "link bandwidth," not vendor-specific terms. Node agents discover and report topology; the scheduler uses it for placement decisions.

## Vendor Support

| Vendor | GPU Family | Interconnect | Topology Discovery | Metrics Collection |
|--------|-----------|-------------|-------------------|-------------------|
| NVIDIA | H100, GH200, B200 | NVLink, NVSwitch | NVML (`nvmlDeviceGetTopologyCommonAncestor`) | NVML / DCGM |
| AMD | MI300X, MI300A | Infinity Fabric, xGMI | ROCm-SMI (`rsmi_topo_get_link_type`) | ROCm-SMI / `rocm_smi_lib` |

Additional vendors can be supported by implementing the topology discovery trait in the node agent.

## Abstraction Model

```
GpuTopology {
    gpus: Vec<GpuDevice>,
    links: Vec<GpuLink>,
    nic_affinity: Map<GpuIndex, NicId>,  // which NIC is closest to which GPU
}

GpuDevice {
    index: u32,
    vendor: GpuVendor,          // Nvidia | Amd
    model: String,              // "H100", "MI300X"
    memory_bytes: u64,
    compute_capability: String, // CUDA CC or GCN/CDNA arch
}

GpuLink {
    gpu_a: u32,
    gpu_b: u32,
    link_type: GpuLinkType,     // NvLink | NvSwitch | InfinityFabric | Xgmi | Pcie
    bandwidth_gbps: f64,
}
```

The node agent populates this structure at startup using vendor-specific APIs and reports it alongside node capabilities and health data.

## Link Types and Bandwidth

| Link Type | Typical Bandwidth | Latency | Notes |
|-----------|-------------------|---------|-------|
| NVLink (H100) | 450 GB/s per link | ~1 μs | Direct GPU-to-GPU |
| NVSwitch (H100) | 900 GB/s all-to-all | ~1 μs | Full-bisection via switch |
| Infinity Fabric (MI300X) | 896 GB/s aggregate | ~1 μs | XGMI links between dies |
| PCIe Gen5 | 64 GB/s | ~2-5 μs | Fallback, cross-socket |
| PCIe Gen4 | 32 GB/s | ~2-5 μs | Older systems |

Actual bandwidth is discovered at runtime via vendor APIs, not hardcoded.

## Intra-Node Scheduling Impact

ADR-007 defines "full-node scheduling with intra-node packing." GPU topology informs the intra-node packing:

### Multi-GPU Jobs Within a Node

For allocations requesting fewer GPUs than the node has, the node agent packs on GPUs with direct high-bandwidth links:

1. Prefer GPUs connected via NVLink/NVSwitch/InfinityFabric (direct high-bandwidth)
2. Avoid splitting across PCIe domains when high-bandwidth links are available
3. For NCCL/RCCL workloads, contiguous GPU groups minimize communication overhead

### Multi-Node Jobs

For allocations spanning multiple nodes:

1. Prefer nodes where GPU-to-NIC affinity matches — GPUs closest to the NIC used for inter-node communication (Slingshot/Ultra Ethernet)
2. NIC affinity reduces PCIe hops for inter-node traffic, improving MPI/NCCL allreduce performance
3. Combined with f₄ (topology_fitness): inter-node placement minimizes dragonfly group span, intra-node placement maximizes link bandwidth

### Selection Algorithm

```
For a k-GPU allocation on a node with n GPUs:
1. Build a graph of GPUs weighted by link bandwidth
2. Find the k-GPU subgraph with maximum minimum link bandwidth
3. If multiple subgraphs tie: prefer the one with best NIC affinity
4. Assign allocation to selected GPUs via cgroup/device isolation
```

## MIG / GPU Partitioning

### NVIDIA Multi-Instance GPU (MIG)

H100 can partition into up to 7 MIG instances, each with isolated memory, cache, and compute:

| MIG Profile | GPU Memory | SMs | Use Case |
|-------------|-----------|-----|----------|
| 1g.10gb | 10 GB | 1/7 | Interactive, notebooks |
| 2g.20gb | 20 GB | 2/7 | Small inference |
| 3g.40gb | 40 GB | 3/7 | Medium training |
| 4g.40gb | 40 GB | 4/7 | Medium training |
| 7g.80gb | 80 GB | 7/7 | Full GPU (no partitioning) |

MIG is relevant for interactive/small-job vClusters where intra-node packing is used. Each MIG instance is a separate schedulable GPU resource.

### AMD

No equivalent partitioning as of MI300 generation. MI300X allocations always get full GPU dies.

### Scheduler Integration

- MIG instances are reported as individual `GpuDevice` entries with reduced `memory_bytes` and a `partitioned: true` flag
- The scheduler treats MIG instances like smaller GPUs — no special MIG logic in the knapsack solver
- MIG configuration is managed by the node agent, not the scheduler (reconfiguration requires idle GPU)

## Integration with Cost Function

GPU topology extends f₄ (topology_fitness) to include intra-node topology quality:

```
f₄(j) = α · inter_node_fitness(j) + (1-α) · intra_node_fitness(j)

inter_node_fitness = 1.0 - (groups_needed / max_groups_available)  // existing
intra_node_fitness = min_link_bandwidth(selected_gpus) / max_link_bandwidth(node)

α = 1.0 for single-node jobs (intra-node only matters)
α = 0.7 for multi-node jobs (inter-node dominates but intra-node still relevant)
```

The node agent reports `GpuTopology` alongside capabilities and health on every heartbeat (topology is static, but health/utilization changes).

## Conformance Interaction

GPU driver version and firmware version are part of the conformance fingerprint (cross-ref: [conformance.md](conformance.md)). For multi-node GPU jobs, mismatched drivers cause NCCL/RCCL hangs. The conformance fitness factor (f₉) ensures nodes in a multi-GPU allocation share the same driver stack.

## Cross-References

- [scheduling-algorithm.md](scheduling-algorithm.md) — f₄ topology_fitness, f₉ conformance_fitness
- [conformance.md](conformance.md) — GPU driver version in conformance fingerprint
- [telemetry.md](telemetry.md) — GPU metrics collection (NVML/DCGM, ROCm-SMI)

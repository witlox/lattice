# Monitoring & Observability

## Allocation Status

```bash
# Your allocations
lattice status

# Specific allocation
lattice status a1b2c3d4

# Filter by state
lattice status --state=running
lattice status --state=pending

# All tenant allocations (requires permissions)
lattice status --all

# Watch mode (refreshes every 5 seconds)
lattice status --watch
lattice watch a1b2c3d4
```

## Logs

```bash
# View logs (from S3 persistent store)
lattice logs a1b2c3d4

# Live tail (streaming)
lattice logs a1b2c3d4 --follow

# Last N lines
lattice logs a1b2c3d4 --tail=100
```

## Metrics

Query metrics for a running allocation:

```bash
# Snapshot of current metrics
lattice metrics a1b2c3d4

# Output:
# METRIC            VALUE    UNIT
# gpu_utilization   87.3     %
# gpu_memory_used   71.2     GB
# cpu_utilization   45.1     %
# memory_used       384.0    GB
# network_rx        12.4     GB/s
# network_tx        8.7      GB/s
```

Live metrics stream:

```bash
lattice metrics a1b2c3d4 --stream
```

## Diagnostics

Combined view of network and storage health for an allocation:

```bash
lattice diagnostics a1b2c3d4

# Network diagnostics only
lattice diagnostics a1b2c3d4 --network

# Storage diagnostics only
lattice diagnostics a1b2c3d4 --storage
```

## Cross-Allocation Comparison

Compare metrics between two allocations (useful for A/B experiments):

```bash
lattice compare a1b2c3d4 e5f6g7h8 --metric=gpu_utilization
```

## Cluster Overview

```bash
# List all nodes
lattice nodes

# Filter by state
lattice nodes --state=ready
lattice nodes --state=draining

# Specific node details
lattice nodes nid001234
```

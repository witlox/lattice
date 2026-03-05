# Getting Started

## Overview

Lattice is a distributed workload scheduler for HPC and AI infrastructure. It schedules both batch jobs (training runs, simulations) and long-running services (inference endpoints, monitoring) on shared GPU-accelerated clusters.

If you're coming from Slurm, most concepts map directly — see the [Slurm migration guide](slurm-migration.md) for a quick comparison.

## Prerequisites

- A running Lattice cluster (ask your admin for the API endpoint)
- The `lattice` CLI installed on your workstation or login node
- Your tenant credentials (OIDC token or mTLS certificate)

## Installing the CLI

```bash
# From a release binary
curl -sSfL https://github.com/witlox/lattice/releases/latest/download/lattice-$(uname -s)-$(uname -m) -o lattice
chmod +x lattice
sudo mv lattice /usr/local/bin/

# Or build from source
cargo install --path crates/lattice-cli
```

## Configuration

Create `~/.config/lattice/config.yaml`:

```yaml
endpoint: "lattice-api.example.com:50051"
tenant: "my-team"
# Optional: default vCluster
vcluster: "gpu-batch"
```

Or use environment variables:

```bash
export LATTICE_ENDPOINT="lattice-api.example.com:50051"
export LATTICE_TENANT="my-team"
```

## Your First Job

### Submit a batch script

```bash
lattice submit train.sh
# Submitted allocation a1b2c3d4
```

### Check status

```bash
lattice status
# ID        NAME           STATE    NODES  WALLTIME   ELAPSED    VCLUSTER
# a1b2c3d4  train.sh       Running  4      24:00:00   00:12:34   gpu-batch
```

### View logs

```bash
lattice logs a1b2c3d4
# [2026-03-05T10:00:12Z] Epoch 1/100, loss=2.341
# [2026-03-05T10:01:45Z] Epoch 2/100, loss=1.892
```

### Cancel a job

```bash
lattice cancel a1b2c3d4
```

## Next Steps

- [Submitting Workloads](submitting-workloads.md) — detailed submission options
- [Interactive Sessions](interactive-sessions.md) — attach a terminal to running jobs
- [DAG Workflows](dag-workflows.md) — multi-step pipelines with dependencies
- [Python SDK](python-sdk.md) — programmatic access from notebooks and agents

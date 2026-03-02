# CLI Design

## Design Principle

The CLI is the primary user interface. It should feel natural to Slurm users while exposing Lattice's richer capabilities. Commands follow a consistent `lattice <verb> [resource] [flags]` pattern. Output is human-readable by default, machine-parseable with `--output=json`.

## Command Structure

```
lattice <command> [subcommand] [arguments] [flags]
```

### Global Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output format: `table` (default), `json`, `yaml`, `wide` |
| `--quiet` | `-q` | Suppress non-essential output |
| `--verbose` | `-v` | Verbose output (debug info) |
| `--tenant` | `-t` | Override tenant (for multi-tenant users) |
| `--vcluster` | | Override vCluster selection |
| `--config` | | Config file path (default: `~/.config/lattice/config.yaml`) |
| `--no-color` | | Disable colored output |

## Core Commands

### Submit (`lattice submit`)

Submit an allocation or batch script.

```bash
# Submit a script (Slurm-compatible directives parsed)
lattice submit script.sh

# Submit with inline arguments
lattice submit --nodes=64 --walltime=72h --uenv=prgenv-gnu/24.11:v1 -- torchrun train.py

# Submit a task group (job array)
lattice submit --task-group=0-99%20 script.sh

# Submit with dependencies
lattice submit --depends-on=12345:success script.sh

# Submit a DAG from YAML
lattice dag submit workflow.yaml

# Submit to a specific vCluster
lattice submit --vcluster=ml-training script.sh
```

**Output:** Allocation ID on success.
```
Submitted allocation 12345
```

### Status (`lattice status`)

Query allocation status.

```bash
# List own allocations
lattice status

# Specific allocation
lattice status 12345

# Filter by state
lattice status --state=running

# All allocations (tenant admin)
lattice status --all

# Watch mode (refresh every 5s)
lattice status --watch
```

**Default output (table):**
```
ID      NAME           STATE    NODES  WALLTIME   ELAPSED   VCLUSTER
12345   training-run   Running  64     72:00:00   14:23:01  ml-training
12346   eval-job       Pending  4      02:00:00   —         hpc-batch
12347   sweep          Running  1×20   04:00:00   01:12:33  hpc-batch
```

**Wide output (`-o wide`):** Adds columns: tenant, project, uenv, GPU type, dragonfly groups.

### Cancel (`lattice cancel`)

Cancel allocations.

```bash
# Cancel single
lattice cancel 12345

# Cancel multiple
lattice cancel 12345 12346 12347

# Cancel all own pending allocations
lattice cancel --state=pending --all-mine

# Cancel a DAG
lattice dag cancel dag-789
```

### Session (`lattice session`)

Create an interactive session. See [sessions.md](sessions.md) for details.

```bash
# Basic session
lattice session --walltime=4h

# With resources
lattice session --nodes=2 --constraint=gpu_type:GH200 --walltime=8h

# With uenv
lattice session --uenv=prgenv-gnu/24.11:v1 --walltime=4h
```

### Attach (`lattice attach`)

Attach a terminal to a running allocation. See [observability.md](observability.md).

```bash
lattice attach 12345
lattice attach 12345 --node=x1000c0s0b0n3
lattice attach 12345 --command="nvidia-smi -l 1"
```

### Launch (`lattice launch`)

Run a task within an existing allocation (srun equivalent).

```bash
# Run on all nodes
lattice launch --alloc=12345 hostname

# Run on specific number of tasks
lattice launch --alloc=12345 -n 4 ./my_program

# Run interactively with PTY
lattice launch --alloc=12345 --pty bash
```

### Logs (`lattice logs`)

View allocation logs. See [observability.md](observability.md).

```bash
lattice logs 12345
lattice logs 12345 --follow
lattice logs 12345 --stderr --node=x1000c0s0b0n3
lattice logs 12345 --tail=100
```

### Top / Watch / Diag / Compare

Monitoring commands. See [observability.md](observability.md).

```bash
lattice top 12345                              # Metrics snapshot
lattice top 12345 --per-gpu                    # Per-GPU breakdown
lattice watch 12345                            # Live streaming metrics
lattice watch 12345 --alerts-only              # Alerts only
lattice diag 12345                             # Network + storage diagnostics
lattice compare 12345 12346 --metric=gpu_util  # Cross-allocation comparison
```

### Telemetry (`lattice telemetry`)

Switch telemetry mode.

```bash
lattice telemetry --alloc=12345 --mode=debug --duration=30m
```

### Nodes (`lattice nodes`)

View cluster nodes (read-only).

```bash
# List all nodes
lattice nodes

# Filter by state
lattice nodes --state=ready

# Filter by vCluster
lattice nodes --vcluster=hpc-batch

# Specific node details
lattice nodes x1000c0s0b0n0
```

**Output:**
```
NODE                STATE   GPUS  VCLUSTER      TENANT    GROUP  CONFORMANCE
x1000c0s0b0n0       Ready   4×GH200  hpc-batch    physics   3      a1b2c3
x1000c0s0b0n1       Ready   4×GH200  hpc-batch    physics   3      a1b2c3
x1000c0s1b0n0       Draining 4×GH200  ml-training  ml-team   7      a1b2c3
```

### History (`lattice history`)

Query completed allocations (accounting data).

```bash
lattice history
lattice history --since=2026-03-01 --until=2026-03-02
lattice history --output=json
```

### DAG Commands (`lattice dag`)

```bash
lattice dag submit workflow.yaml     # Submit a DAG
lattice dag status dag-789           # DAG status with per-allocation states
lattice dag list                     # List DAGs
lattice dag cancel dag-789           # Cancel a DAG
```

### Cache Commands (`lattice cache`)

```bash
lattice cache warm --image=prgenv-gnu/24.11:v1 --group=3
lattice cache status --node=x1000c0s0b0n0
```

## Admin Commands (`lattice admin`)

Administrative commands require system-admin role.

```bash
# Node management
lattice node drain x1000c0s0b0n0
lattice node drain x1000c0s0b0n0 --urgent
lattice node undrain x1000c0s0b0n0
lattice node disable x1000c0s0b0n0

# Tenant management
lattice admin tenant create --name=physics --max-nodes=200
lattice admin tenant set-quota --name=physics --max-nodes=250

# vCluster management
lattice admin vcluster create --name=hpc-batch --scheduler=hpc-backfill --tenant=physics
lattice admin vcluster set-weights --name=hpc-batch --priority=0.20 ...

# Configuration
lattice admin config get accounting.enabled
lattice admin config set accounting.enabled=true

# Raft status
lattice admin raft status
```

## Output Formats

| Format | Flag | Use Case |
|--------|------|----------|
| `table` | Default | Human-readable, aligned columns |
| `wide` | `-o wide` | Extended columns |
| `json` | `-o json` | Machine-parseable, scripting |
| `yaml` | `-o yaml` | Machine-parseable, config integration |

All formats support piping and redirection. JSON output uses newline-delimited JSON for streaming commands (logs --follow, watch).

## Error Messages

Errors are human-readable with actionable guidance:

```
Error: allocation rejected — tenant "physics" exceeds max_nodes quota
  Current: 195 nodes in use
  Requested: 10 additional nodes
  Limit: 200 nodes

  Hint: Cancel running allocations or request a quota increase from your tenant admin.
```

```
Error: no nodes available matching constraints
  GPU type: GH200
  Nodes requested: 64
  Available: 42 (22 in use by your allocations, 136 by other tenants)

  Hint: Reduce node count, use --topology=any, or wait for resources.
```

## Shell Completion

Shell completion is generated for bash, zsh, and fish:

```bash
# Generate completion
lattice completion bash > /etc/bash_completion.d/lattice
lattice completion zsh > ~/.zfunc/_lattice
lattice completion fish > ~/.config/fish/completions/lattice.fish
```

Completions cover: subcommands, flag names, allocation IDs (from recent `lattice status`), node IDs, vCluster names, uenv names.

## Configuration File

```yaml
# ~/.config/lattice/config.yaml
api_url: "https://firecrest.example.com/lattice/v1"
default_tenant: "physics"
default_vcluster: "hpc-batch"
default_uenv: "prgenv-gnu/24.11:v1"
output_format: "table"
color: true
```

Environment variables override config file: `LATTICE_API_URL`, `LATTICE_TENANT`, `LATTICE_VCLUSTER`.

## Slurm Compatibility Aliases

For sites migrating from Slurm, optional shell aliases:

```bash
# Source from lattice-provided script
source $(lattice compat-aliases)

# Provides:
# sbatch → lattice submit
# squeue → lattice status
# scancel → lattice cancel
# salloc → lattice session
# srun → lattice launch
# sinfo → lattice nodes
# sacct → lattice history
```

These aliases translate Slurm flags to Lattice flags where possible. See [slurm-migration.md](slurm-migration.md) for details.

## Cross-References

- [api-design.md](api-design.md) — API endpoints that CLI commands map to
- [sessions.md](sessions.md) — Interactive session lifecycle
- [observability.md](observability.md) — Monitoring commands (top, watch, diag, compare)
- [slurm-migration.md](slurm-migration.md) — Slurm command translation details

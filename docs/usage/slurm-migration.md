# Slurm Migration

## Command Mapping

| Slurm | Lattice | Notes |
|-------|---------|-------|
| `sbatch script.sh` | `lattice submit script.sh` | `#SBATCH` directives are parsed |
| `squeue` | `lattice status` | |
| `squeue -u $USER` | `lattice status` | Default shows own jobs |
| `scancel 12345` | `lattice cancel 12345` | |
| `salloc` | `lattice session` | Interactive allocation |
| `srun --pty bash` | `lattice attach <id>` | Attach terminal |
| `sinfo` | `lattice nodes` | Cluster node overview |
| `sacct` | `lattice status --all` | Historical view |

## Directive Mapping

| `#SBATCH` Directive | Lattice Equivalent | Notes |
|---------------------|-------------------|-------|
| `--nodes=N` | `--nodes=N` | Exact match |
| `--ntasks=N` | — | Mapped to node count: `ceil(N / tasks_per_node)` |
| `--ntasks-per-node=N` | — | Passed as task config |
| `--time=HH:MM:SS` | `--walltime=HH:MM:SS` | Also accepts `24h`, `30m` shorthand |
| `--partition=X` | `--vcluster=X` | Configurable partition→vCluster mapping |
| `--account=X` | `--tenant=X` | Account→tenant mapping |
| `--job-name=X` | `--name=X` | |
| `--output=file` | — | Logs always go to persistent store; download path configurable |
| `--error=file` | — | Same as `--output` |
| `--constraint=X` | `--constraint=X` | Feature matching |
| `--gres=gpu:N` | `--constraint="gpu_count=N"` | |
| `--qos=X` | `--preemption-class=N` | Configurable QOS→class mapping |
| `--array=0-99%20` | `--task-group=0-99%20` | |
| `--dependency=afterok:ID` | `--depends-on=ID:success` | |
| `--exclusive` | Default | Lattice always allocates full nodes |

## Environment Variables

When Slurm compatibility is enabled (`compat.set_slurm_env: true`), Lattice sets familiar environment variables inside allocations:

| Variable | Value |
|----------|-------|
| `SLURM_JOB_ID` | Allocation ID |
| `SLURM_JOB_NAME` | Allocation name |
| `SLURM_NNODES` | Number of allocated nodes |
| `SLURM_NODELIST` | Comma-separated node list |
| `SLURM_NTASKS` | Task count |
| `SLURM_SUBMIT_DIR` | Working directory at submission |

Lattice also sets its own `LATTICE_*` equivalents.

## What's Different

### Full-Node Scheduling
Lattice always allocates full nodes (no sub-node sharing). This simplifies resource management and improves performance isolation. If you're used to `--ntasks=1` on a shared node, you'll get the whole node.

### No Partitions — vClusters
Slurm partitions map to Lattice vClusters, but vClusters are more flexible: each has its own scheduling policy (backfill, bin-pack, FIFO, reservation) and weight tuning.

### Topology-Aware Placement
Lattice automatically packs multi-node jobs within the same Slingshot dragonfly group for optimal network performance. No manual `--switches` needed.

### Data Staging
Lattice can pre-stage data during queue wait time. Add `--data-mount="s3://bucket/data:/data"` and the scheduler factors data locality into placement decisions.

### Checkpointing
Unlike Slurm's `--requeue`, Lattice coordinates checkpointing before preemption. Declare `--checkpoint=signal` and your job receives `SIGUSR1` before being suspended.

## Migration Steps

1. **Start with existing scripts** — `#SBATCH` directives work out of the box
2. **Replace `sbatch`/`squeue`/`scancel`** with `lattice submit`/`status`/`cancel`
3. **Gradually adopt native features** — data staging, checkpointing, DAGs, uenv
4. **Tune scheduling weights** — use the RM-Replay simulator for A/B comparison

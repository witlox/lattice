# Slurm Migration

## Design Principle

Migration from Slurm should be gradual and low-risk. Existing Slurm scripts should work with minimal changes via the compatibility layer. Users can adopt Lattice-native features incrementally. The goal is not perfect Slurm emulation — it's a smooth on-ramp.

## Migration Phases

### Phase 1: Dual-Stack (Recommended Start)

Run Lattice alongside Slurm on a subset of nodes. Users can submit to either system. This provides:
- Side-by-side comparison of scheduling behavior
- Gradual user migration with rollback to Slurm
- Time to validate RM-Replay weight tuning

### Phase 2: Compat-Mode Cutover

Move all nodes to Lattice. Users continue using `sbatch`/`squeue` via compatibility aliases. Slurm daemons are decommissioned.

### Phase 3: Native Adoption

Users migrate scripts to native `lattice` CLI, adopting features not available in Slurm (reactive scaling, metric-driven autoscaling, DAG workflows, data staging hints).

## Script Compatibility

### Supported `#SBATCH` Directives

| Slurm Directive | Lattice Mapping | Notes |
|----------------|-----------------|-------|
| `--nodes=N` | `resources.nodes: N` | Exact match |
| `--ntasks=N` | Mapped to node count | `nodes = ceil(N / tasks_per_node)` |
| `--ntasks-per-node=N` | Passed as task config | Used by launcher |
| `--time=HH:MM:SS` | `lifecycle.walltime` | Exact match |
| `--partition=X` | `vcluster: X` | Partition name → vCluster name mapping |
| `--account=X` | `tenant: X` | Account → tenant mapping |
| `--job-name=X` | `tags.name: X` | Stored as tag |
| `--output=file` | Log path hint | Logs always go to S3; `--output` sets download path |
| `--error=file` | Log path hint | Same as `--output` |
| `--constraint=X` | `constraints.features` | Feature matching |
| `--gres=gpu:N` | `constraints.gpu_count` | Mapped to GPU constraint |
| `--exclusive` | Default behavior | Lattice schedules full nodes by default (ADR-007) |
| `--array=0-99%20` | `task_group` | Task group with concurrency limit |
| `--dependency=afterok:123` | `depends_on: [{ref: "123", condition: "success"}]` | DAG edge |
| `--qos=X` | `preemption_class` | QoS → priority mapping (configurable per site) |
| `--mail-user`, `--mail-type` | Not supported | Warn, skip |
| `--mem=X` | Not supported | Full-node scheduling; memory is not a constraint |
| `--cpus-per-task=N` | Not supported | Full-node scheduling |
| `--uenv=X` | `environment.uenv: X` | Lattice extension, not in Slurm |
| `--view=X` | `environment.view: X` | Lattice extension |

### Unsupported Directives

Directives that have no Lattice equivalent are handled gracefully:

```
Warning: #SBATCH --mem=64G ignored (Lattice uses full-node scheduling, memory is not constrainable)
Warning: #SBATCH --mail-user=user@example.com ignored (use `lattice watch` for event notifications)
Submitted allocation 12345
```

The submission succeeds — unsupported directives produce warnings, not errors. This is critical for migration: existing scripts should not fail because of irrelevant Slurm options.

### Conflicting Directives

| Conflict | Resolution |
|----------|------------|
| `--nodes=64` + `--ntasks=128` with `--ntasks-per-node=4` | `--nodes` takes precedence; `ntasks-per-node` used by launcher |
| `--exclusive` + `--mem=64G` | `--exclusive` is default; `--mem` ignored with warning |
| `--partition` not found | Error: `vCluster "X" not found. Available: hpc-batch, ml-training, interactive` |

## Slurm Features Not Supported

These Slurm features have no Lattice equivalent and are not planned:

| Feature | Reason | Alternative |
|---------|--------|-------------|
| Job steps (`srun` within `sbatch`) | Lattice uses tasks within allocations | `lattice launch --alloc=<id>` |
| Hetjob (heterogeneous job) | Not yet designed | Submit separate allocations with DAG dependencies |
| Burst buffer (`#DW`) | DataWarp-specific | Use `data.mounts` with `tier_hint: hot` |
| GRES beyond GPU | Not needed (full-node scheduling) | Use `constraints.features` for non-GPU resources |
| Accounting (`sacctmgr`) | Waldur handles accounting | `lattice history` or Waldur portal |
| Reservations (`scontrol create reservation`) | Use sensitive claims for dedicated nodes | `lattice admin reserve` (future) |
| Licenses/resources (`--licenses=`) | Not applicable | Use `constraints.features` |
| Multi-cluster (`--cluster=`) | Use federation | `lattice submit --site=X` (if federation enabled) |

## `srun` Within Allocations

Slurm users often use `srun` inside batch scripts to launch parallel tasks. In Lattice:

```bash
# Slurm pattern:
srun -n 256 ./my_mpi_program

# Lattice equivalent (inside a running allocation):
# Option 1: The entrypoint IS the parallel launch
# In the submission script, use the appropriate launcher directly:
mpirun -np 256 ./my_mpi_program
# or:
torchrun --nproc_per_node=4 ./train.py

# Option 2: Use lattice launch from another terminal
lattice launch --alloc=12345 -n 256 ./my_mpi_program
```

The compatibility layer translates `srun` to `lattice launch` when the compat aliases are active.

## Environment Variables

Slurm sets many environment variables in jobs. Lattice provides equivalent variables:

| Slurm Variable | Lattice Variable | Description |
|---------------|------------------|-------------|
| `SLURM_JOB_ID` | `LATTICE_ALLOC_ID` | Allocation ID |
| `SLURM_JOB_NAME` | `LATTICE_JOB_NAME` | Job name (from tags) |
| `SLURM_NODELIST` | `LATTICE_NODELIST` | Comma-separated node list |
| `SLURM_NNODES` | `LATTICE_NNODES` | Number of nodes |
| `SLURM_NPROCS` | `LATTICE_NPROCS` | Number of tasks |
| `SLURM_ARRAY_TASK_ID` | `LATTICE_TASK_INDEX` | Task group index |
| `SLURM_ARRAY_JOB_ID` | `LATTICE_TASK_GROUP_ID` | Task group parent ID |
| `SLURM_SUBMIT_DIR` | `LATTICE_SUBMIT_DIR` | Submission directory |
| `SLURM_JOBID` | `LATTICE_ALLOC_ID` | Alias for compatibility |

For migration convenience, the compat layer can also set `SLURM_*` variables (configurable: `compat.set_slurm_env=true`). This is disabled by default to avoid confusion.

## Partition-to-vCluster Mapping

Sites configure the mapping from Slurm partition names to Lattice vClusters:

```yaml
# lattice-compat.yaml
partition_mapping:
  normal: "hpc-batch"
  debug: "interactive"
  gpu: "ml-training"
  long: "hpc-batch"        # multiple partitions can map to one vCluster
  sensitive: "sensitive-secure"
qos_mapping:
  low: 1
  normal: 4
  high: 7
  urgent: 9
```

Unmapped partition names produce an error with a list of available vClusters.

## Migration Checklist

For site administrators:

- [ ] Deploy Lattice control plane alongside Slurm
- [ ] Configure partition-to-vCluster mapping
- [ ] Configure QoS-to-preemption-class mapping
- [ ] Tune cost function weights using RM-Replay with production traces
- [ ] Test representative batch scripts via compat layer
- [ ] Validate accounting (Waldur) captures match Slurm sacct data
- [ ] Train users on `lattice` CLI basics
- [ ] Run dual-stack for 2-4 weeks
- [ ] Migrate remaining users, decommission Slurm

For users:

- [ ] Test existing scripts with `lattice submit` (compat mode parses `#SBATCH`)
- [ ] Review warnings for unsupported directives
- [ ] Replace `srun` in scripts with direct launcher commands (`mpirun`, `torchrun`)
- [ ] (Optional) Migrate to native `lattice` CLI syntax for new workflows

## Cross-References

- [api-design.md](api-design.md) — Compatibility API command mapping
- [cli-design.md](cli-design.md) — Native CLI design and compat aliases
- [sessions.md](sessions.md) — `salloc` equivalent
- [dag-scheduling.md](dag-scheduling.md) — DAG dependencies (replace `--dependency`)
- [mpi-process-management.md](mpi-process-management.md) — MPI launch, PMI-2, srun replacement

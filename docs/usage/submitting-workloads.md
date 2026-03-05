# Submitting Workloads

## Basic Submission

```bash
# Run a script on 4 nodes for up to 24 hours
lattice submit --nodes=4 --walltime=24h train.sh

# With GPU constraints
lattice submit --nodes=8 --walltime=72h --constraint="gpu_type=GH200" -- torchrun train.py

# With a software environment (uenv)
lattice submit --nodes=2 --uenv=prgenv-gnu/24.11:v1 -- make -j run
```

## Script Directives

Lattice parses `#LATTICE` directives from your script (and `#SBATCH` for compatibility):

```bash
#!/bin/bash
#LATTICE --nodes=64
#LATTICE --walltime=72h
#LATTICE --uenv=prgenv-gnu/24.11:v1
#LATTICE --vcluster=ml-training
#LATTICE --tenant=physics
#LATTICE --name=large-training-run

torchrun --nproc_per_node=4 train.py --data /scratch/dataset
```

## Resource Constraints

```bash
# GPU type
lattice submit --constraint="gpu_type=GH200,gpu_count=4" script.sh

# Memory requirements
lattice submit --constraint="memory_gb>=512" script.sh

# Require unified memory (GH200/MI300A superchip)
lattice submit --constraint="require_unified_memory" script.sh

# Prefer same NUMA domain
lattice submit --constraint="prefer_same_numa" script.sh
```

## Task Groups (Job Arrays)

Submit multiple instances of the same job:

```bash
# 100 tasks, 20 running concurrently
lattice submit --task-group=0-99%20 sweep.sh

# Task index available as $LATTICE_TASK_INDEX
```

## Dependencies

```bash
# Run after job succeeds
lattice submit --depends-on=a1b2c3d4:success postprocess.sh

# Run after job completes (success or failure)
lattice submit --depends-on=a1b2c3d4:any cleanup.sh

# Multiple dependencies
lattice submit --depends-on=job1:success,job2:success merge.sh
```

## Data Staging

Lattice can pre-stage data to the hot tier before your job starts:

```bash
lattice submit --data-mount="s3://bucket/dataset:/data" --nodes=4 train.sh
```

The scheduler evaluates data readiness as part of the cost function — jobs with data already on the hot tier are prioritized.

## Lifecycle Types

### Bounded (batch) — default

```bash
lattice submit --walltime=24h train.sh
```

Job runs until completion or walltime, then terminates.

### Unbounded (service)

```bash
lattice submit --service --expose=8080 serve.sh
```

Runs indefinitely. Exposed ports are reachable via the network domain.

### Reactive (autoscaling)

```bash
lattice submit --reactive --min-nodes=1 --max-nodes=8 \
  --scale-metric=gpu_utilization --scale-target=0.8 serve.sh
```

Automatically scales between min and max nodes based on the target metric.

## Preemption Classes

Higher preemption class = harder to preempt:

```bash
# Best-effort (preempted first)
lattice submit --preemption-class=0 experiment.sh

# Normal priority (default: 5)
lattice submit train.sh

# High priority
lattice submit --preemption-class=8 critical-training.sh
```

## Checkpointing

If your application supports checkpointing, declare it:

```bash
# Signal-based (receives SIGUSR1 before preemption)
lattice submit --checkpoint=signal train.sh

# gRPC callback
lattice submit --checkpoint=grpc --checkpoint-port=9999 train.sh

# Shared memory flag
lattice submit --checkpoint=shmem train.sh

# Non-preemptible (no checkpoint, never preempted)
lattice submit --no-preempt train.sh
```

## Slurm Compatibility

Existing Slurm scripts work with minimal changes:

```bash
# These are equivalent
sbatch --nodes=4 --time=24:00:00 --partition=gpu train.sh
lattice submit --nodes=4 --walltime=24h --vcluster=gpu train.sh
```

Supported `#SBATCH` directives are automatically translated. See [Slurm Migration](slurm-migration.md) for details.

## Output Formats

```bash
# Default: human-readable table
lattice status

# JSON (for scripting)
lattice status -o json

# YAML
lattice status -o yaml

# Wide (more columns)
lattice status -o wide
```

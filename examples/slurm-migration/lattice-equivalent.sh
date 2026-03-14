#!/usr/bin/env bash
set -euo pipefail

# lattice-equivalent.sh — Same job as slurm-batch.sh using Lattice CLI
#
# This script shows how each Slurm concept maps to Lattice.
# Lattice uses the Compatibility API to accept Slurm-style batch scripts
# directly, but you can also use the native CLI for clearer semantics.

# ┌──────────────────────────────────────────────────────────────────────┐
# │ Slurm Concept          │ Lattice Equivalent                        │
# ├──────────────────────────────────────────────────────────────────────┤
# │ #SBATCH --job-name      │ --project                                │
# │ #SBATCH --nodes         │ --nodes                                  │
# │ #SBATCH --time          │ --walltime                               │
# │ #SBATCH --account       │ --tenant (via -t flag or LATTICE_TENANT) │
# │ #SBATCH --partition     │ --vcluster (scheduler policy selection)  │
# │ #SBATCH --gres=gpu:N    │ (GPU resources via uenv/constraints)     │
# │ #SBATCH --constraint    │ session create --constraint              │
# │ #SBATCH --dependency    │ --depends-on                             │
# │ #SBATCH --qos           │ --priority (0-10 scale)                  │
# │ #SBATCH --array         │ --task-group                             │
# │ module load             │ --uenv + --view (SquashFS images)        │
# │ srun                    │ (not needed, entrypoint runs directly)   │
# │ $SLURM_JOB_NODELIST     │ $LATTICE_NODELIST                       │
# │ $SLURM_NNODES           │ $LATTICE_NNODES                         │
# │ squeue                  │ lattice status                           │
# │ scancel                 │ lattice cancel                           │
# │ salloc                  │ lattice session create                   │
# │ sacct                   │ lattice status --all (+ Waldur for acct) │
# └──────────────────────────────────────────────────────────────────────┘

# Option A: Native Lattice CLI submission
#
# The uenv "pytorch:2.3" replaces 'module load' — it mounts a SquashFS
# image containing CUDA, NCCL, Python, and PyTorch, with near-zero overhead.
# No srun needed: Lattice handles distributed launch across allocated nodes.
lattice submit \
  --nodes 8 \
  --walltime 24:00:00 \
  --uenv "pytorch:2.3" \
  --view "default" \
  --priority 5 \
  --project "resnet-training" \
  --tenant "ml-team" \
  --depends-on "afterok:12345" \
  -- torchrun \
    --nproc_per_node=4 \
    train.py \
    --epochs 100 \
    --batch-size 256 \
    --data /scratch/dataset \
    --output /scratch/checkpoints

# Option B: Submit the Slurm script directly (Compatibility API)
#
# Lattice can parse #SBATCH directives from existing Slurm scripts.
# This is the easiest migration path — no script changes needed.
# Lattice maps directives to its native concepts automatically.
lattice submit slurm-batch.sh

# Monitoring equivalents:
#
# squeue → lattice status
# lattice status <allocation-id>
#
# sacct → lattice status --all (historical)
# lattice status --all --tenant ml-team
#
# scontrol show job → lattice diag
# lattice diag <allocation-id>
#
# tail -f slurm-*.out → lattice logs -f
# lattice logs <allocation-id> -f
#
# scancel → lattice cancel
# lattice cancel <allocation-id>

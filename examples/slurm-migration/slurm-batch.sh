#!/usr/bin/env bash
set -euo pipefail

# slurm-batch.sh — Typical Slurm sbatch script
#
# This is a standard Slurm batch script for a multi-node GPU training job.
# See lattice-equivalent.sh for the same job expressed using Lattice CLI.

#SBATCH --job-name=resnet-training
#SBATCH --nodes=8
#SBATCH --ntasks-per-node=4
#SBATCH --time=24:00:00
#SBATCH --account=ml-team
#SBATCH --partition=gpu
#SBATCH --gres=gpu:4
#SBATCH --constraint=gpu_mem:80gb
#SBATCH --output=slurm-%j.out
#SBATCH --error=slurm-%j.err
#SBATCH --dependency=afterok:12345
#SBATCH --qos=normal
#SBATCH --mail-type=END,FAIL
#SBATCH --mail-user=user@example.com

# Load modules (traditional HPC approach)
module load cuda/12.4
module load nccl/2.20
module load python/3.11

# Set up environment
export MASTER_ADDR=$(scontrol show hostnames $SLURM_JOB_NODELIST | head -n 1)
export MASTER_PORT=29500
export WORLD_SIZE=$((SLURM_NNODES * SLURM_NTASKS_PER_NODE))

# Run distributed training
srun torchrun \
  --nnodes=$SLURM_NNODES \
  --nproc_per_node=$SLURM_NTASKS_PER_NODE \
  --rdzv_backend=c10d \
  --rdzv_endpoint=$MASTER_ADDR:$MASTER_PORT \
  train.py \
  --epochs 100 \
  --batch-size 256 \
  --data /scratch/dataset \
  --output /scratch/checkpoints

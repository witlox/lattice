#!/usr/bin/env bash
set -euo pipefail

# 01-basic-submit.sh — Submit a basic batch job to Lattice
#
# This example demonstrates the most common `lattice submit` flags for
# a bounded (batch) allocation. The job runs a Python training script
# on 4 nodes with GPU resources and a 2-hour walltime.

# Submit a batch training job with explicit resource requests.
# - --nodes:    Number of compute nodes to allocate
# - --walltime: Maximum runtime in HH:MM:SS format (Slurm-compatible)
# - --uenv:     uenv SquashFS image to mount (software environment)
# - --view:     uenv view (named mount configuration within the image)
# - --priority: Priority class 0-10 (higher = more important)
# - --project:  Project/job name for tracking
# - --tenant:   Tenant (organizational unit) for quota accounting
# - --:         Everything after '--' is the entrypoint command
lattice submit \
  --nodes 4 \
  --walltime 2:00:00 \
  --uenv "pytorch:2.3" \
  --view "default" \
  --priority 5 \
  --project "resnet-training" \
  --tenant "ml-team" \
  -- python train.py --epochs 100 --batch-size 64

# The command prints the allocation ID on success:
#   Submitted allocation: a1b2c3d4-e5f6-7890-abcd-ef1234567890
#
# Use `lattice status <id>` to check progress.
# Use `lattice logs <id> -f` to follow output.
# Use `lattice cancel <id>` to stop the job.

echo ""
echo "--- Variations ---"

# Submit an inline command without a script file
lattice submit \
  --nodes 1 \
  --walltime 0:30:00 \
  --tenant "physics" \
  -- ./run_simulation.sh --config params.yaml

# Submit a Slurm-compatible batch script (with #SBATCH directives)
# Lattice parses #SBATCH headers and maps them to Lattice concepts.
lattice submit train.sh

# Submit with JSON output for programmatic use
lattice -o json submit \
  --nodes 2 \
  --walltime 1:00:00 \
  --tenant "ml-team" \
  -- python evaluate.py

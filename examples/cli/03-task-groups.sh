#!/usr/bin/env bash
set -euo pipefail

# 03-task-groups.sh — Task group (job array) submission
#
# Task groups are Lattice's equivalent of Slurm job arrays. They submit
# multiple identical allocations that differ only by a task index.
# Format: --task-group START-END[%MAX_CONCURRENT]
#   START:          First task index
#   END:            Last task index (inclusive)
#   STEP (optional): Index stride (default: 1), specified as START-END:STEP
#   %MAX_CONCURRENT: Maximum simultaneous tasks (throttle)

# Submit a 100-task group for hyperparameter sweep.
# Each task gets a unique index via environment variable.
# At most 20 tasks run concurrently (to avoid monopolizing the cluster).
lattice submit \
  --nodes 1 \
  --walltime 1:00:00 \
  --uenv "pytorch:2.3" \
  --task-group "0-99%20" \
  --project "hparam-sweep" \
  --tenant "ml-team" \
  -- python sweep.py --task-id '$LATTICE_TASK_ID'

# Submit a smaller array with a step size.
# This creates tasks with indices 0, 10, 20, ..., 90 (10 tasks total).
lattice submit \
  --nodes 1 \
  --walltime 0:30:00 \
  --task-group "0-99" \
  --project "ensemble-run" \
  --tenant "physics" \
  -- ./simulate.sh --seed '$LATTICE_TASK_ID'

# Submit a task group with dependencies.
# All tasks in this group start only after allocation <prev-id> succeeds.
lattice submit \
  --nodes 1 \
  --walltime 0:15:00 \
  --task-group "0-9" \
  --depends-on "afterok:<prev-allocation-id>" \
  --project "post-processing" \
  --tenant "physics" \
  -- python postprocess.py --chunk '$LATTICE_TASK_ID'

# Check status of a task group (shows individual task states)
# lattice status <allocation-id>

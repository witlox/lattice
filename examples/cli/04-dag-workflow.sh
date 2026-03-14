#!/usr/bin/env bash
set -euo pipefail

# 04-dag-workflow.sh — DAG (Directed Acyclic Graph) workflow submission
#
# DAGs define multi-stage pipelines where each stage is an allocation with
# dependency edges. Lattice supports Slurm-compatible dependency conditions:
#   afterok:     Start after dependency completes successfully
#   afternotok:  Start after dependency fails
#   afterany:    Start after dependency finishes (success or failure)
#   aftercorr:   Start corresponding task in a task group
#
# DAG definitions are YAML files. See examples/dag/ for sample pipelines.

# Submit a training pipeline DAG.
# The YAML file defines stages and their dependency edges.
lattice dag submit examples/dag/training-pipeline.yaml

# Submit with a tenant override (overrides tenant specified in the YAML).
lattice dag submit examples/dag/training-pipeline.yaml \
  --tenant "ml-team"

# Check DAG status (shows overall progress and per-stage state).
# Replace <dag-id> with the ID printed by dag submit.
# lattice dag status <dag-id>

# Check DAG status with detailed per-stage breakdown.
# lattice dag status <dag-id> --detail

# Cancel a DAG (cancels all pending and running stages).
# lattice dag cancel <dag-id>

# Cancel only pending stages (let running stages finish).
# lattice dag cancel <dag-id> --pending-only

# Submit the ETL pipeline with error handling.
# This DAG uses afternotok edges for failure recovery.
lattice dag submit examples/dag/multi-stage-etl.yaml

# Submit a checkpoint-aware workflow.
lattice dag submit examples/dag/checkpoint-workflow.yaml

# Use JSON output for scripting.
lattice -o json dag submit examples/dag/training-pipeline.yaml

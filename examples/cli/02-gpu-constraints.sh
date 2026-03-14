#!/usr/bin/env bash
set -euo pipefail

# 02-gpu-constraints.sh — Submit GPU jobs with topology constraints
#
# Lattice supports fine-grained GPU and topology constraints to ensure
# optimal placement on Slingshot/Ultra Ethernet interconnect fabrics.
# The scheduler's topology fitness factor (f4) packs allocations into
# the same dragonfly group for minimal inter-node communication latency.

# Multi-node GPU training with NVIDIA GH200 constraint.
# The session create command with --constraint allows specifying
# hardware requirements. The scheduler places nodes within the same
# Slingshot dragonfly group when possible (topology packing).
lattice session create \
  --nodes 8 \
  --walltime 24:00:00 \
  --uenv "pytorch:2.3" \
  --view "develop" \
  --constraint "gpu_type:GH200" \
  --name "llm-training"

# After the session is created, attach to it for interactive work.
# The session ID is printed on creation.
# lattice attach <session-id>

# Submit a job requesting AMD MI300X GPUs.
# Different GPU types are available depending on the cluster configuration.
lattice session create \
  --nodes 4 \
  --walltime 12:00:00 \
  --uenv "rocm:6.1" \
  --constraint "gpu_type:MI300X" \
  --name "mi300x-benchmark"

# Submit a batch job that needs nodes in the same network domain.
# Network domains provide L3 reachability via Slingshot VNI assignment.
# All allocations sharing a network domain can communicate directly.
lattice submit \
  --nodes 16 \
  --walltime 8:00:00 \
  --uenv "nccl-tests:latest" \
  --priority 7 \
  --project "nccl-allreduce-bench" \
  --tenant "hpc-benchmarks" \
  -- ./run_nccl_tests.sh --collective allreduce --size 1G

# Submit a job that prefers NUMA-local memory placement.
# The scheduler considers memory topology (f4 blends inter-node group
# packing with intra-node memory locality) when scoring nodes.
# NUMA preferences are enforced via numactl in the allocation prologue.
lattice submit \
  --nodes 2 \
  --walltime 4:00:00 \
  --uenv "tensorflow:2.16" \
  --priority 6 \
  --project "numa-aware-training" \
  --tenant "ml-team" \
  -- python train_numa.py --prefer-local-memory

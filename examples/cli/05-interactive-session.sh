#!/usr/bin/env bash
set -euo pipefail

# 05-interactive-session.sh — Interactive session management
#
# Sessions are Lattice's equivalent of Slurm's salloc. They create
# an allocation that you can attach to interactively. Sessions are
# useful for development, debugging, and exploratory work.

# Create an interactive session with 2 nodes and an 8-hour walltime.
# The session ID is printed on creation.
lattice session create \
  --nodes 2 \
  --walltime 8:00:00 \
  --uenv "pytorch:2.3" \
  --view "develop" \
  --name "dev-session"

# Output:
#   Session created: s1a2b3c4-d5e6-7890-abcd-ef1234567890

# List all active sessions.
lattice session list

# List only your own sessions.
lattice session list --mine

# Filter sessions by state.
lattice session list --state active

# Attach a terminal to a running session.
# This uses nsenter to join the session's namespace on the head node.
# Replace <session-id> with the ID from session create.
# lattice attach <session-id>

# Once attached, you have a shell on the allocated nodes with
# the uenv environment mounted. Run your commands interactively:
#   $ python train.py --debug
#   $ nvidia-smi
#   $ exit  # detach (session keeps running)

# Create a GPU session with hardware constraints.
lattice session create \
  --nodes 4 \
  --walltime 4:00:00 \
  --uenv "cuda:12.4" \
  --constraint "gpu_type:GH200" \
  --name "gpu-debug"

# Destroy a session when done (frees resources immediately).
# lattice session destroy <session-id>

# Force destroy without confirmation prompt.
# lattice session destroy <session-id> --force

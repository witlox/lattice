#!/usr/bin/env bash
set -euo pipefail

# 06-monitoring.sh — Monitoring and observability commands
#
# Lattice provides several commands for monitoring allocations, viewing
# logs, watching state changes, and inspecting diagnostics. Telemetry
# data is collected via eBPF with <0.3% overhead and stored in a
# time-series database (VictoriaMetrics).

ALLOC_ID="${1:-<allocation-id>}"

# --- Status ---

# Check the status of a specific allocation.
lattice status "$ALLOC_ID"

# List all allocations (with optional filtering).
lattice status --all

# JSON output for scripting.
lattice -o json status "$ALLOC_ID"

# --- Logs ---

# View recent logs from an allocation (last 100 lines by default).
lattice logs "$ALLOC_ID"

# Follow logs in real time (like tail -f).
# Logs are streamed via SSE from the node agent's ring buffer.
lattice logs "$ALLOC_ID" -f

# --- Live Dashboard ---

# Show a live resource usage dashboard for an allocation.
# Displays CPU, GPU, memory, and network utilization.
lattice top "$ALLOC_ID"

# Show cluster-wide resource usage.
lattice top --cluster

# --- Watch ---

# Watch allocation state changes in real time.
# Streams lifecycle events (pending -> staging -> running -> completed).
lattice watch "$ALLOC_ID"

# --- Diagnostics ---

# Show detailed diagnostics for an allocation.
# Includes scheduling decisions, placement info, and resource consumption.
lattice diag "$ALLOC_ID"

# --- Node Inspection ---

# List all compute nodes and their status.
lattice nodes

# Show detailed info for a specific node.
lattice nodes --node nid001234

# Wide output with extra columns (GPU type, memory topology, etc).
lattice -o wide nodes

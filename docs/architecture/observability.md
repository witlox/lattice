# User-Facing Observability & Debugging

## Design Principle

Lattice already collects high-resolution telemetry (eBPF, TSDB, three aggregation modes) for operator and scheduler use. This document describes the **user-facing surface** that lets job owners debug, monitor, and profile their own allocations without admin intervention.

All observability data flows through existing pipelines — no new collection infrastructure is required. The user-facing layer adds scoped query access, streaming endpoints, and interactive attach.

## Overview

```
┌─ User ──────────────────────────────────────────────────────┐
│  lattice attach / logs / top / watch / diag / compare       │
│         │           │          │         │         │         │
│         ▼           ▼          ▼         ▼         ▼         │
│    ┌─────────── lattice-api (gRPC + REST) ──────────────┐   │
│    │  Attach ──────────────── bidir stream to node agent │   │
│    │  Logs ────────────────── ring buffer (live) + S3    │   │
│    │  Metrics ─────────────── PromQL query to TSDB       │   │
│    │  StreamMetrics ───────── fan-out to node agents     │   │
│    │  Diagnostics ─────────── TSDB + fabric telemetry    │   │
│    │  Compare ─────────────── multi-alloc TSDB query     │   │
│    └────────────────────────────────────────────────────┘   │
│         │           │          │         │                   │
│         ▼           ▼          ▼         ▼                   │
│    Node Agents    S3 logs     TSDB    Slingshot CSIG        │
└─────────────────────────────────────────────────────────────┘
```

| Capability | Data Source | Latency | CLI Command |
|---|---|---|---|
| Attach to running allocation | Node agent (nsenter) | Real-time | `lattice attach <id>` |
| Log streaming (live tail) | Node agent ring buffer | Sub-second | `lattice logs <id> --follow` |
| Historical logs | S3 | Seconds | `lattice logs <id>` |
| Live metrics (`top`) | TSDB | 30s (prod mode) | `lattice top <id>` |
| Live telemetry stream (`watch`) | Node agents (push) | 1-30s | `lattice watch <id>` |
| Diagnostics | TSDB + fabric telemetry | 30s | `lattice diag <id>` |
| Cross-allocation comparison | TSDB | Seconds | `lattice compare <id1> <id2>` |
| Application profiling | User tools (via tools_uenv) | N/A | User-driven |

## Attach to Running Allocation

### Architecture

The attach mechanism provides an interactive terminal session inside a running allocation's execution environment. The node agent uses `nsenter` to enter the allocation's mount and network namespaces — this is **not** a new allocation, just a terminal session in the existing one.

```
User → FirecREST → lattice-api → gRPC bidir stream → node agent
                                                         │
                                                    nsenter into
                                                    mount/net ns
                                                         │
                                                    PTY ↔ shell
```

### Terminal Protocol

The gRPC bidirectional stream carries:

- **Client → Server:** stdin bytes, terminal resize events, signals (SIGINT, SIGTSTP)
- **Server → Client:** stdout/stderr bytes, exit code on completion

The stream begins with an `AttachStart` message specifying the target node (for multi-node allocations) and command (default: user's shell).

### Authorization Model

| vCluster Type | Who Can Attach | Additional Constraints |
|---|---|---|
| HPC (backfill) | Allocation owner | — |
| Service (bin-pack) | Allocation owner | — |
| Interactive (FIFO) | Allocation owner | Already has session; attach is secondary terminal |
| Medical (reservation) | Claiming user only | Session recorded, audit trail, signed uenv only |

### Medical Constraints

- Only the user who claimed the nodes (identity from Raft audit log) can attach
- All attach sessions are recorded (input + output) to the medical audit log
- Attach is only permitted when the allocation runs a signed uenv
- Session start/end events are Raft-committed audit entries

### Attach During Preemption

If the allocation is preempted while an attach session is active, the session is terminated gracefully. See [sessions.md](sessions.md) for the detailed preemption sequence. If the allocation is in `Checkpointing` state, new attach requests are rejected with: `"allocation is being checkpointed — attach unavailable until rescheduled"`.

### CLI Usage

```bash
# Attach to allocation (first node, user's shell)
lattice attach 12345

# Attach to a specific node
lattice attach 12345 --node=x1000c0s0b0n3

# Attach with a specific command
lattice attach 12345 --command="nvidia-smi -l 1"
```

### Slurm Compatibility

| Slurm | Lattice |
|---|---|
| `srun --jobid=123 --pty bash` | `lattice attach 123` |

## Log Streaming

### Dual-Path Architecture

Logs use two paths to balance latency and durability:

1. **Ring buffer (live tail):** Each node agent maintains a per-allocation ring buffer (default 64 MB) of stdout/stderr. Supports low-latency streaming for `--follow` mode. Data is ephemeral — lost when the allocation ends or the buffer wraps.

2. **S3 persistence:** Node agents periodically flush log chunks to S3 for durable storage. Available during and after allocation execution.

```
Process stdout/stderr
    │
    ├──→ Ring buffer (node agent, 64 MB)
    │         │
    │         └──→ gRPC StreamLogs (live tail)
    │
    └──→ S3 flush (periodic, configurable interval)
              │
              └──→ REST GET /logs (historical)
```

### Log Storage Layout

```
s3://{tenant}/{project}/{alloc_id}/logs/
    ├── stdout/{node_id}/{chunk_000..N}.log.zst
    ├── stderr/{node_id}/{chunk_000..N}.log.zst
    └── metadata.json    # timestamps, byte offsets, node list
```

Logs are compressed with zstd. The metadata file enables efficient range queries by time or byte offset.

### Streaming (Live Tail)

Via gRPC `StreamLogs` RPC (server-streaming). The client specifies:
- Allocation ID
- Stream filter: stdout, stderr, or both
- Node filter: specific node or all nodes
- Follow mode: whether to keep streaming as new output arrives
- Tail lines: number of lines from the ring buffer to replay on connect

### Historical Log Access

Via REST `GET /v1/allocations/{id}/logs`:
- Query params: `stream` (stdout/stderr), `node`, `since`, `until`, `offset`, `limit`
- Returns paginated log entries from S3
- Available after allocation completion (subject to retention policy)

### Medical Constraints

- Logs from medical allocations are encrypted at rest in the dedicated medical S3 pool
- All log access events are recorded in the medical audit log
- Log retention follows medical data retention policy (user-specified, minimum per regulation)
- Logs are only accessible to the claiming user and designated compliance reviewers

### CLI Usage

```bash
# View logs (all nodes, both streams)
lattice logs 12345

# Follow mode (live tail)
lattice logs 12345 --follow

# Filter by stream and node
lattice logs 12345 --stderr --node=x1000c0s0b0n3

# Tail last 100 lines
lattice logs 12345 --tail=100

# Historical range
lattice logs 12345 --since="2026-03-01T10:00:00Z" --until="2026-03-01T11:00:00Z"
```

### Slurm Compatibility

| Slurm | Lattice |
|---|---|
| `cat slurm-123.out` | `lattice logs 123` |
| `tail -f slurm-123.out` | `lattice logs 123 --follow` |

## User-Facing Live Metrics (`lattice top`)

### Query Path

Metrics are served from the TSDB (not directly from node agents). The lattice-api translates user queries into PromQL, scoped to the requesting user's allocations.

```
lattice top <id> → lattice-api → PromQL → TSDB → response
```

This reuses the existing telemetry pipeline. In prod mode, data has 30-second resolution. In debug mode (if switched), 1-second resolution.

### Metrics Catalog

| Metric | Description | Unit |
|---|---|---|
| `gpu_utilization` | SM occupancy per GPU | % |
| `gpu_memory_used` | GPU memory in use | bytes |
| `gpu_power_draw` | GPU power consumption | watts |
| `cpu_utilization` | CPU usage per node | % |
| `memory_used` | System memory in use | bytes |
| `network_tx_bytes` | Network bytes sent | bytes/s |
| `network_rx_bytes` | Network bytes received | bytes/s |
| `io_read_bytes` | Storage read throughput | bytes/s |
| `io_write_bytes` | Storage write throughput | bytes/s |
| `io_latency_p99` | Storage I/O latency (p99) | microseconds |

### Display Modes

| Mode | Flag | Content |
|---|---|---|
| Summary (default) | — | Aggregated across all nodes: mean GPU%, total mem, total I/O |
| Per-node | `--per-node` | One row per node |
| Per-GPU | `--per-gpu` | One row per GPU across all nodes |
| Wide | `--wide` | All metrics in a wide table |

### REST + gRPC Access

- **REST:** `GET /v1/allocations/{id}/metrics?mode=summary&duration=5m`
- **gRPC:** `QueryMetrics` RPC with `MetricsQueryRequest`

### CLI Usage

```bash
# Summary view (default)
lattice top 12345

# Per-node breakdown
lattice top 12345 --per-node

# Per-GPU breakdown
lattice top 12345 --per-gpu

# Wide format with all metrics
lattice top 12345 --wide

# Custom time window
lattice top 12345 --duration=1h
```

## Live Telemetry Stream (`lattice watch`)

### Push-Based Event Stream

Unlike `lattice top` (which queries TSDB), `lattice watch` opens a push-based stream from node agents for near-real-time events.

```
lattice watch <id> → lattice-api → fan-out → node agents
                          ↑
                     stream merge
                          ↑
             per-node MetricsEvent streams
```

### Relationship to Telemetry Modes

| Telemetry Mode | `lattice top` Resolution | `lattice watch` Resolution |
|---|---|---|
| prod | 30s (TSDB) | 30s (prod aggregation from node agent) |
| debug | 1s (TSDB) | 1s (raw events from node agent) |
| audit | 30s (TSDB) | 30s + access events |

Switching to debug mode (`lattice telemetry --alloc=12345 --mode=debug`) increases resolution for both `top` and `watch`.

### Stream Content

Each `MetricsEvent` contains:
- Timestamp and node ID
- Current metric values (GPU, CPU, memory, network, I/O)
- Threshold alerts (if any metric exceeds configured bounds)

Alerts are generated by node agents when metrics cross thresholds:
- GPU utilization drops below 10% (potential hang)
- GPU memory utilization exceeds 95% (OOM risk)
- Network error rate exceeds threshold
- I/O latency spike detected

### CLI Usage

```bash
# Watch all metrics (refreshing display)
lattice watch 12345

# Watch specific metrics
lattice watch 12345 --metrics=gpu_utilization,memory_used

# Watch with alerts only (suppress normal updates)
lattice watch 12345 --alerts-only
```

## Diagnostics View

### Network Diagnostics

Network health is critical for multi-node allocations. Diagnostics expose Slingshot-specific metrics that are otherwise invisible to users.

| Metric | Description | Source |
|---|---|---|
| CSIG congestion | In-band congestion signals per Slingshot group | eBPF CSIG tap |
| Group span | Number of dragonfly groups the allocation spans | Topology model |
| Inter-node bandwidth | Measured bandwidth between node pairs | eBPF network flow |
| NVLink throughput | GPU-to-GPU bandwidth (intra-node) | NVML |

### Storage Diagnostics

| Metric | Description | Source |
|---|---|---|
| QoS floor vs actual | Configured storage QoS vs measured throughput | VAST API + eBPF I/O |
| Latency histogram | I/O latency distribution (p50/p95/p99) | eBPF block I/O |
| Mount health | Per-mount status (NFS, S3, scratch) | Node agent |
| IOPS | Read/write operations per second | eBPF block I/O |

### Combined Diagnostics

`lattice diag` combines network and storage diagnostics into a single view with health indicators:

```
$ lattice diag 12345

Network:
  Group span:     2 groups (g3, g7)
  CSIG congestion: LOW (0.02 avg)
  Inter-node BW:  190 GB/s avg (target: 200 GB/s) ✓

Storage:
  /data/input (NFS):  12.5 GB/s read (QoS floor: 10 GB/s) ✓
  /scratch (NVMe):    6.2 GB/s write, p99 latency: 45µs ✓
  /home (NFS):        0.1 GB/s (idle) ✓

GPUs:
  SM occupancy:   92% avg across 256 GPUs ✓
  NVLink:         850 GB/s avg (of 900 GB/s) ✓
  ECC errors:     0 ✓
```

### CLI Usage

```bash
# Full diagnostics
lattice diag 12345

# Network only
lattice diag 12345 --network

# Storage only
lattice diag 12345 --storage
```

## Cross-Allocation Comparison

### TSDB Query

Compares metrics across multiple allocations by querying the same TSDB data used for `lattice top`. Useful for regression detection across training runs.

### Time Alignment

Comparisons use **relative-to-start** time alignment: each allocation's metrics are indexed from `t=0` (allocation start), not wall clock time. This allows meaningful comparison of allocations that ran at different times.

### CLI Usage

```bash
# Compare two allocations
lattice compare 12345 12346

# Compare specific metric
lattice compare 12345 12346 --metric=gpu_utilization

# JSON output for scripting
lattice compare 12345 12346 --output=json
```

### REST Interface

```
GET /v1/compare?ids=12345,12346&metrics=gpu_utilization,io_write_bytes&align=relative
```

## Application Profiling Integration

### Scope

Lattice provides **mechanisms** for profiling, not profiler implementations. Users bring their own profiling tools, delivered via `tools_uenv`.

### Profiler Delivery

Profiling tools are packaged as uenv images and mounted alongside the application uenv:

```yaml
environment:
  uenv: "prgenv-gnu/24.11:v1"        # application stack
  tools_uenv: "profiling/2024.1"     # profilers: nsight, vtune, darshan, etc.
```

The `tools_uenv` mount provides profiler binaries without contaminating the application environment.

### Usage Patterns

**Batch profiling (non-interactive):**
```bash
# Submit with profiling tools
lattice submit --uenv=prgenv-gnu/24.11:v1 --tools-uenv=profiling/2024.1 script.sh
# Script uses profiler internally (e.g., nsys profile ./train)
# Results written to output directory
```

**Interactive profiling (attach-based):**
```bash
# Attach and run profiler interactively
lattice attach 12345 --command="nsys profile --delay=60 -o /scratch/profile ./train"
```

### Darshan / Score-P Integration Notes

- **Darshan:** LD_PRELOAD-based I/O profiling. No Lattice-specific integration needed; user loads Darshan from `tools_uenv` and sets `LD_PRELOAD`. Darshan logs written to scratch/output.
- **Score-P:** Instrumentation-based profiling. User compiles with Score-P wrappers from `tools_uenv`. Lattice provides no special support beyond tools delivery and `attach`.

## Security Model

### Authorization

All observability endpoints are scoped by OIDC token claims:
- Users can only query their own allocations (or allocations in their tenant, if tenant-admin)
- Token scopes: `allocations:read` (metrics, logs, diagnostics), `allocations:attach` (interactive attach)
- Medical allocations: only the claiming user (verified against Raft audit log)

### Rate Limiting

All rate limits are **per user** (identified by OIDC subject claim). Tenant admins and system admins share the same limits unless overridden in system configuration.

| Endpoint | Rate Limit | Scope | Rationale |
|---|---|---|---|
| Attach | 5 concurrent sessions | Per user | Resource-intensive (PTY per session) |
| StreamLogs | 10 concurrent streams | Per user | Memory (ring buffer readers) |
| QueryMetrics | 60 req/min | Per user | TSDB query load |
| StreamMetrics | 5 concurrent streams | Per user | Node agent fan-out |
| Diagnostics | 30 req/min | Per user | TSDB + fabric query load |
| Compare | 10 req/min | Per user | Multi-alloc TSDB queries |

**When rate limit is exceeded:**
- Concurrent limits (Attach, StreamLogs, StreamMetrics): New request rejected with `429 Too Many Requests` and a message: `"maximum concurrent sessions reached (5/5). Close an existing session to open a new one."`
- Request-rate limits (QueryMetrics, Diagnostics, Compare): Request rejected with `429 Too Many Requests` and `Retry-After` header indicating seconds until the next request is allowed.
- No queueing — rejected requests must be retried by the client.

**Admin override:** System admins can adjust per-user rate limits via configuration:
```yaml
rate_limits:
  attach_max_concurrent: 10       # override default of 5
  query_metrics_per_minute: 120   # override default of 60
```

### Data Sensitivity

| Data Type | Sensitivity | Handling |
|---|---|---|
| Metrics (GPU%, CPU%, I/O) | Low | Standard OIDC scoping |
| Logs (stdout/stderr) | Medium | May contain application data; encrypted at rest for medical |
| Attach (interactive terminal) | High | Session recorded for medical; PTY access = code execution |
| Diagnostics (network/storage) | Low | Infrastructure metrics, no application data |
| Profiling output | Medium | Written to user's storage, no Lattice-managed persistence |

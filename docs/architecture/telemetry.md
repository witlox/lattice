# Telemetry Architecture

## Design Principle

Collect at high resolution, aggregate at configurable resolution, transmit out-of-band.

## Three-Layer Pipeline

### Layer 1: Collection (eBPF, always-on)

eBPF programs JIT-compiled into kernel, attached to tracepoints and kprobes.

**Kernel-level metrics:**
- CPU: context switches, runqueue depth, scheduling latency histograms
- Network: per-flow bytes/packets, Slingshot CSIG congestion signals from packet headers
- Block I/O: latency histograms, throughput per device (NVMe scratch, network mounts)
- Memory: allocation/free rates, NUMA locality, page faults

**GPU metrics (via NVML/DCGM hooks):**
- SM occupancy, memory utilization, power draw
- PCIe/NVLink throughput
- ECC error counts (feeds into checkpoint cost model)

**Storage overhead:** ~0.3% on compute-bound workloads. eBPF programs run in kernel context, no syscall overhead, no userspace daemon polling.

Data flows into per-CPU ring buffers (BPF_MAP_TYPE_RINGBUF), consumed by the node agent.

### Layer 2: Aggregation (Node Agent, switchable)

The node agent reads ring buffers and aggregates based on the current mode.

**Mode: prod (default)**
- 30-second aggregation windows
- Statistical summaries: p50, p95, p99, mean, max, count
- Bicubic interpolation for time-series smoothing (reduces storage, preserves trends)
- Transmitted on Slingshot telemetry traffic class (separate from compute traffic)
- Additional overhead: ~0.1%

**Mode: debug (per-job or per-node, time-limited)**
- 1-second or sub-second raw event streams
- Full per-flow network traces
- GPU kernel-level profiling (CUPTI integration)
- Stored to job-specific S3 path for user analysis
- Additional overhead: ~2-5% (acceptable for debugging)
- Auto-reverts to prod after configured duration (default: 30 minutes)

**Mode: audit (medical vCluster)**
- All file access events (open, read, write, close) with user identity
- All API calls logged with request/response metadata
- Network flow summaries (source, destination, bytes, duration)
- Signed with Sovra keys (if federation enabled) for tamper evidence
- Additional overhead: ~1%
- Retention: 7 years (cold tier, S3-compatible archive)

### Layer 3: Storage and Query

**Time-series store** (VictoriaMetrics, Mimir, or Thanos — TBD based on scale testing):
- Ingestion: all nodes stream aggregated metrics
- Auto-downsampling: raw → 1m → 5m → 1h → 1d
- Retention policy configurable per tenant/vCluster

**Three materialized views (label-based access control):**

| View | Audience | Content |
|---|---|---|
| Holistic | System admins | System-wide utilization, power, health, scheduling efficiency |
| Tenant | Tenant admins | Per-tenant resource usage, quota tracking, job statistics |
| vCluster | Scheduler | Metrics feeding into cost function (GPU util, I/O, congestion) |
| User | Allocation owners | Per-allocation metrics scoped by OIDC identity (via lattice-api) |

**Query interface:** PromQL-compatible API. Grafana dashboards for visualization.

**Debug traces:** Stored to `s3://{tenant}/{project}/{job_id}/telemetry/` with short retention (7 days default, configurable).

**Audit logs:** Append-only, encrypted at rest, stored to dedicated audit storage with long retention. Queryable for compliance reporting.

## Switching Telemetry Mode

Via Intent API:
```
PATCH /v1/allocations/{id}
{ "telemetry": { "mode": "debug", "duration": "30m" } }
```

Via CLI:
```bash
lattice telemetry --alloc=12345 --mode=debug --duration=30m
```

Switching is instant — the eBPF programs are always collecting at full resolution. Only the aggregation behavior changes.

## User-Facing Telemetry Query

The telemetry pipeline serves admin dashboards and the scheduler cost function. The user-facing query layer adds scoped access so allocation owners can query their own metrics without admin intervention.

### Query Path

```
User → lattice-api → PromQL (scoped by alloc/tenant/user) → TSDB → response
```

The lattice-api injects label filters to ensure users only see metrics for their own allocations. Tenant admins can query any allocation within their tenant.

### Scoping Rules

| Caller | Visible Scope |
|---|---|
| Allocation owner | Metrics for their own allocations |
| Tenant admin | Metrics for any allocation in their tenant |
| System admin | All metrics (holistic view) |

### User Metrics Catalog

| Metric | Description | Available In |
|---|---|---|
| `gpu_utilization` | SM occupancy per GPU | prod, debug, audit |
| `gpu_memory_used` | GPU memory in use | prod, debug, audit |
| `gpu_power_draw` | GPU power consumption | prod, debug, audit |
| `cpu_utilization` | CPU usage per node | prod, debug, audit |
| `memory_used` | System memory in use | prod, debug, audit |
| `network_tx_bytes` | Network bytes sent per second | prod, debug, audit |
| `network_rx_bytes` | Network bytes received per second | prod, debug, audit |
| `io_read_bytes` | Storage read throughput | prod, debug, audit |
| `io_write_bytes` | Storage write throughput | prod, debug, audit |
| `io_latency_p99` | Storage I/O latency (p99) | prod, debug, audit |

## Telemetry Streaming

For use cases requiring push-based updates (e.g., `lattice watch`), the `StreamMetrics` RPC fans out to node agents running the target allocation and merges their streams.

### Architecture

```
lattice-api receives StreamMetrics request
    → identifies nodes running allocation (from quorum state)
    → opens per-node metric streams to node agents
    → merges streams with allocation-scoped labels
    → returns unified server-streaming response to client
```

In **prod mode**, node agents emit aggregated snapshots every 30 seconds. In **debug mode**, raw events stream at 1-second intervals. The client receives the same resolution as the current telemetry mode — switching mode (via `PATCH /v1/allocations/{id}`) takes effect on active streams.

### Alert Generation

Node agents evaluate threshold rules locally and inject `MetricAlert` events into the stream when:
- GPU utilization < 10% for > 60s (potential hang)
- GPU memory > 95% (OOM risk)
- Network error rate exceeds 0.1%
- I/O p99 latency exceeds 10ms

## Cross-Allocation Comparison

Users can compare metrics across multiple allocations (e.g., successive training runs) via the `CompareMetrics` RPC or `GET /v1/compare`.

### TSDB Query

The lattice-api issues parallel PromQL queries for each allocation ID, scoped to the requesting user's permissions. Results are aligned by relative time (see below).

### Relative Time Alignment

Allocations may run at different wall-clock times. Comparison uses **relative-to-start** alignment: each allocation's metric series is indexed from `t=0` (the allocation's `started_at` timestamp). This allows apples-to-apples comparison of metrics across runs that started hours or days apart.

## Feedback to Scheduler

The telemetry system feeds key metrics back to the scheduling cost function:

| Metric | Cost Function Component | Effect |
|---|---|---|
| GPU utilization per job | Efficiency scoring | Low util → deprioritize for topology-premium placement |
| Network congestion (CSIG) | topology_fitness | Congested groups → avoid placing new jobs there |
| I/O throughput per job | data_readiness | High I/O demand → ensure storage QoS before scheduling |
| Node ECC errors | checkpoint cost model | Rising errors → increase checkpoint urgency |
| Power draw per node | energy_cost | Feeds into power budget constraint |

## Telemetry Aggregation Topology

For large systems (10,000+ nodes), direct streaming to a central store creates an ingestion bottleneck. Use hierarchical aggregation:

```
Nodes (per-group) → Group Aggregator → Central Store

Each Slingshot dragonfly group has a designated aggregator node.
Group aggregators perform first-level aggregation (merge per-node summaries).
Central store receives per-group aggregated streams.

In debug mode: bypasses group aggregation, streams directly for that job's nodes.
```

## Scheduler Self-Monitoring

Internal metrics for monitoring Lattice's own health. These metrics feed into canary criteria during rolling upgrades (cross-ref: [upgrades.md](upgrades.md)) and are available on the holistic dashboard.

### Scheduling Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `lattice_scheduling_cycle_duration_seconds` | histogram | `vcluster` | Time to complete one scheduling cycle |
| `lattice_scheduling_queue_depth` | gauge | `vcluster` | Number of pending allocations |
| `lattice_scheduling_proposals_total` | counter | `vcluster`, `result` (accepted/rejected) | Proposals sent to quorum |
| `lattice_scheduling_cost_function_duration_seconds` | histogram | `vcluster` | Time to evaluate the cost function for all candidates |
| `lattice_scheduling_backfill_jobs_total` | counter | `vcluster` | Allocations placed via backfill |

### Quorum Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `lattice_raft_leader` | gauge | `member_id` | 1 if this member is leader, 0 if follower |
| `lattice_raft_commit_latency_seconds` | histogram | `member_id` | Time from proposal to commit |
| `lattice_raft_log_entries` | gauge | `member_id` | Number of entries in the Raft log |
| `lattice_raft_snapshot_duration_seconds` | histogram | `member_id` | Time to create a Raft snapshot |

### API Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `lattice_api_requests_total` | counter | `method`, `status` | Total API requests |
| `lattice_api_request_duration_seconds` | histogram | `method` | Request latency |
| `lattice_api_active_streams` | gauge | `stream_type` (attach/logs/metrics) | Active streaming connections |

### Node Agent Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `lattice_agent_heartbeat_latency_seconds` | histogram | `node_id` | Heartbeat round-trip time |
| `lattice_agent_allocation_startup_seconds` | histogram | `node_id` | Time from allocation assignment to process start (includes uenv pull/mount) |
| `lattice_agent_ebpf_overhead_percent` | gauge | `node_id` | Measured eBPF collection overhead |

### Accounting Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `lattice_accounting_events_buffered` | gauge | — | Events in the in-memory accounting buffer |
| `lattice_accounting_events_dropped_total` | counter | — | Events dropped due to buffer overflow |

### Alerting Rules

Example alerting rules (PromQL-compatible):

| Rule | Condition | Severity |
|------|-----------|----------|
| Scheduling cycle slow | `histogram_quantile(0.99, lattice_scheduling_cycle_duration_seconds) > 30` | warning |
| Queue depth high | `lattice_scheduling_queue_depth > 100` for 5 minutes | warning |
| Raft commit slow | `histogram_quantile(0.99, lattice_raft_commit_latency_seconds) > 5` | critical |
| Node heartbeat missing | `time() - lattice_agent_last_heartbeat_timestamp > 60` | node degraded |
| API error rate spike | `rate(lattice_api_requests_total{status=~"5.."}[5m]) / rate(lattice_api_requests_total[5m]) > 0.05` | warning |
| Accounting buffer filling | `lattice_accounting_events_buffered > 8000` | warning |

### Dashboard Views

Three views matching the existing telemetry pattern:

| Dashboard | Audience | Key Panels |
|-----------|----------|------------|
| Holistic | System admins | All scheduler cycle times, quorum health, total queue depth, API throughput |
| Per-vCluster | Scheduler operators | vCluster-specific queue depth, cycle time, proposal accept rate, backfill rate |
| Per-quorum-member | Quorum operators | Raft log size, commit latency, leader status, snapshot timing |

## Monitoring Deployment

### Prometheus Scrape Configuration

All Lattice components expose metrics on a `/metrics` endpoint (Prometheus exposition format):

| Component | Default Metrics Port | Endpoint |
|-----------|---------------------|----------|
| Quorum members | 9100 | `http://{quorum-host}:9100/metrics` |
| API servers | 9101 | `http://{api-host}:9101/metrics` |
| vCluster schedulers | 9102 | `http://{scheduler-host}:9102/metrics` |
| Node agents | 9103 | `http://{node-host}:9103/metrics` |
| Checkpoint broker | 9104 | `http://{checkpoint-host}:9104/metrics` |

Example Prometheus scrape config:
```yaml
scrape_configs:
  - job_name: "lattice-quorum"
    static_configs:
      - targets: ["quorum-1:9100", "quorum-2:9100", "quorum-3:9100"]

  - job_name: "lattice-api"
    static_configs:
      - targets: ["api-1:9101", "api-2:9101"]

  - job_name: "lattice-scheduler"
    static_configs:
      - targets: ["scheduler-hpc:9102", "scheduler-ml:9102", "scheduler-interactive:9102"]

  - job_name: "lattice-agents"
    file_sd_configs:
      - files: ["/etc/prometheus/lattice-agents.json"]
        refresh_interval: 5m
    # Node agents are numerous; use file-based service discovery
    # populated from OpenCHAMI node inventory
```

### Alert Routing

Alerts are routed via Alertmanager (or compatible system):

| Severity | Route | Response Time |
|----------|-------|--------------|
| Critical | PagerDuty / on-call | Immediate (< 15 min) |
| Warning | Slack #lattice-alerts | Business hours (< 4 hours) |
| Info | Slack #lattice-info | Best effort |

Example Alertmanager route:
```yaml
route:
  receiver: "slack-info"
  routes:
    - match: { severity: "critical" }
      receiver: "pagerduty-oncall"
    - match: { severity: "warning" }
      receiver: "slack-alerts"
```

### Grafana Dashboards

Pre-built dashboards for the three views described above. Dashboards are defined as JSON and version-controlled in `infra/grafana/`:

```
infra/grafana/
├── holistic.json          # System-wide overview
├── per-vcluster.json      # vCluster-specific scheduling
├── per-quorum-member.json # Raft health
├── per-node.json          # Individual node health
└── user-allocation.json   # User-facing allocation metrics
```

Each dashboard uses the standard Lattice metric names. Data source: Prometheus (or compatible TSDB).

### TSDB Sizing

| Cluster Size | Metric Cardinality | Ingestion Rate | Storage (30-day retention) |
|-------------|-------------------|----------------|---------------------------|
| 100 nodes | ~50,000 series | ~10k samples/s | ~50 GB |
| 1,000 nodes | ~500,000 series | ~100k samples/s | ~500 GB |
| 10,000 nodes | ~5,000,000 series | ~1M samples/s | ~5 TB |

For clusters > 1000 nodes, use a horizontally scalable TSDB (VictoriaMetrics cluster, Mimir, or Thanos) with the hierarchical aggregation described in the Telemetry Aggregation Topology section above.

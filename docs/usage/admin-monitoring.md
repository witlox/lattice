# Cluster Monitoring & Observability

## Prometheus Metrics

Lattice exposes Prometheus-compatible metrics at `GET /metrics` on the REST port (default 8080).

### Key Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `lattice_allocations_total` | Counter | Total allocations by state |
| `lattice_allocations_active` | Gauge | Currently running allocations |
| `lattice_scheduling_cycle_duration_seconds` | Histogram | Scheduling cycle latency |
| `lattice_scheduling_placements_total` | Counter | Successful placements |
| `lattice_scheduling_preemptions_total` | Counter | Preemption events |
| `lattice_raft_commit_latency_seconds` | Histogram | Raft commit latency |
| `lattice_raft_sensitive_audit_entries_total` | Counter | Sensitive audit log entries |
| `lattice_api_request_duration_seconds` | Histogram | API request latency |
| `lattice_api_requests_total` | Counter | API requests by method and status |
| `lattice_nodes_total` | Gauge | Nodes by state |
| `lattice_checkpoint_duration_seconds` | Histogram | Checkpoint operation latency |

#### Allocation Dispatch Metrics

These cover the Dispatcher → agent bridge (RunAllocation, rollback,
completion-report flow). Most have labels; see below for typical
label values.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `lattice_dispatch_attempt_total` | Counter | `node_id`, `result` | One increment per `RunAllocation` RPC attempt. `result` ∈ `accepted` / `accepted_already_running` / `transient_failure` / `refusal_busy` / `refusal_unsupported` / `refusal_malformed`. |
| `lattice_dispatch_rollback_total` | Counter | `reason` | `RollbackDispatch` commits. `reason` ∈ `retry_exhausted` / `partial_accept` / `unsupported_capability` / `malformed_request` / `silent_sweep`. |
| `lattice_dispatch_rollback_stop_sent_total` | Counter | `result` | Best-effort `StopAllocation` RPCs issued after a rollback. `result` ∈ `success` / `failure`. |
| `lattice_dispatch_skipped_unschedulable_total` | Counter | `node_id`, `node_state` | Dispatcher skipped an allocation because an assigned node was not Ready. Sustained non-zero rate suggests the scheduler is placing onto Draining / Degraded / Down nodes. |
| `lattice_completion_report_cross_node_total` | Counter | `node_id` | Reports rejected because the source node is not in `allocation.assigned_nodes` (INV-D12). Non-zero indicates a buggy agent or spoofing attempt. |
| `lattice_completion_report_phase_regression_total` | Counter | `current_phase`, `reported_phase` | Reports rejected for phase regression (INV-D7). |
| `lattice_dispatch_unknown_refusal_reason_total` | Counter | `reason_code` | Unknown `refusal_reason` values received — indicates rolling-upgrade skew between agent and lattice-api versions. |
| `lattice_node_degraded_by_dispatch_total` | Counter | `node_id` | Nodes transitioned to Degraded via the `max_node_dispatch_failures` cap. |
| `lattice_cancel_stop_sent_total` | Counter | `result` | `StopAllocation` fan-out triggered by a user cancel (INT-1). |

### Scrape Configuration

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'lattice'
    static_configs:
      - targets:
        - 'lattice-01:8080'
        - 'lattice-02:8080'
        - 'lattice-03:8080'
```

## Grafana Dashboards

Pre-built dashboards are in `infra/grafana/dashboards/`:

- **Cluster Overview** — node states, allocation throughput, queue depth
- **Scheduling Performance** — cycle latency, placement rate, preemption rate
- **Raft Health** — commit latency, leader elections, log compaction
- **Per-Tenant Usage** — resource consumption, fair-share deficit

Import via Grafana UI or provision from `infra/grafana/provisioning/`.

## Alerting Rules

Pre-configured alerting rules in `infra/alerting/`:

| Alert | Condition |
|-------|-----------|
| `LatticeRaftNoLeader` | No Raft leader for > 30s |
| `LatticeNodeDown` | Node heartbeat lost for > 5m |
| `LatticeSchedulingStalled` | No placements for > 10m with pending jobs |
| `LatticeHighPreemptionRate` | > 10 preemptions/minute |
| `LatticeCheckpointFailure` | Checkpoint success rate < 90% |
| `LatticeDiskSpaceLow` | Raft data directory > 80% full |
| `LatticeDispatchRollbackRate` | `rate(lattice_dispatch_rollback_total[5m]) > 0.1` for 10m |
| `LatticeDispatchSkippedUnschedulable` | Non-zero `lattice_dispatch_skipped_unschedulable_total` sustained > 5m (scheduler placing on non-Ready nodes) |
| `LatticeCrossNodeCompletionReports` | Any `lattice_completion_report_cross_node_total` increment (possible spoofing or buggy agent) |
| `LatticeDispatchRefusalSkew` | Any `lattice_dispatch_unknown_refusal_reason_total` increment (agent/control-plane version skew) |

## TSDB Integration

Lattice pushes per-node telemetry to VictoriaMetrics (or any Prometheus-compatible remote write endpoint).

```yaml
telemetry:
  tsdb_endpoint: "http://victoriametrics:8428"
  prod_interval_seconds: 30
```

Telemetry includes CPU, memory, GPU utilization, network I/O, and disk I/O per node.

## Audit Log

Sensitive workload operations are recorded in the Raft-committed audit log:

```bash
# Query audit log
curl "http://lattice-01:8080/api/v1/audit?tenant=sensitive-team&from=2026-03-01"
```

Audit entries include: node claims/releases, allocation lifecycle events, and access log entries. Retention: 7 years (configurable).

## Health Check

```bash
curl http://lattice-01:8080/healthz
# {"status": "ok"}
```

Used by Docker/Kubernetes health probes and load balancers.

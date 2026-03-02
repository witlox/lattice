# Accounting

## Design Principle

Lattice schedules, Waldur accounts. Accounting is asynchronous and optional (feature-flagged like federation). Waldur unavailability never blocks scheduling.

## What is Waldur

[Waldur](https://waldur.com/) is a hybrid cloud orchestrator with HPC integration, accounting, billing, and self-service portal. It provides:

- Resource usage tracking and billing
- Project-level budget management
- Self-service quota requests
- Invoice generation

Integration is via Waldur's REST API.

## Integration Pattern

```
Lattice ──async push──→ Waldur (accounting events)
Waldur ──API call──→ Lattice (quota updates)
```

Lattice pushes accounting events to Waldur asynchronously. Waldur can push quota updates back. The two systems are loosely coupled — neither depends on the other for core functionality.

## Accounting Events

Events pushed from Lattice to Waldur:

| Event | Trigger | Payload |
|-------|---------|---------|
| `allocation.started` | Allocation enters Running state | tenant, project, user, resources (nodes, GPUs, GPU type), estimated duration |
| `allocation.completed` | Allocation reaches terminal state | actual duration, GPU-hours consumed, exit status, storage bytes written |
| `allocation.checkpointed` | Checkpoint written | checkpoint storage consumed, checkpoint duration |
| `quota.updated` | Waldur updates a tenant's quota | new quota values (Waldur → Lattice direction) |

Events are timestamped and include the allocation ID for correlation.

## Entity Mapping

| Lattice Entity | Waldur Entity | Notes |
|---------------|---------------|-------|
| Tenant | Customer | 1:1 mapping |
| Project (within tenant) | Project | 1:1 mapping |
| vCluster | Offering | Each vCluster type is a service offering |
| Allocation | Order | Each allocation is a resource order |

## Waldur API Endpoints Used

| Direction | Endpoint | Purpose |
|-----------|----------|---------|
| Lattice → Waldur | `POST /api/marketplace-orders/` | Report resource usage |
| Lattice → Waldur | `POST /api/invoices/{id}/items/` | Add billing line items |
| Waldur → Lattice | `GET /api/customers/{id}/quotas/` | Read project quotas |
| Waldur → Lattice | `PUT /v1/tenants/{id}/quotas` | Update tenant quotas in Lattice |

## Authentication

Waldur API token is stored in a secrets manager (never in config files):

```yaml
waldur:
  token_secret_ref: "vault://lattice/waldur-token"
```

The token is loaded at startup and refreshed on rotation. Cross-ref: [security.md](security.md) for secret management.

## Failure Handling

Waldur unavailability must never block scheduling:

1. **Buffer:** Accounting events are buffered in a bounded in-memory queue (default: 10,000 events)
2. **Persist:** If the buffer fills, overflow events are persisted to disk (WAL-style append log)
3. **Replay:** On Waldur reconnection, buffered and persisted events are replayed in order
4. **Alert:** If the disk buffer exceeds a threshold (default: 100,000 events), an alert is raised via scheduler self-monitoring (cross-ref: [telemetry.md](telemetry.md))
5. **Degrade gracefully:** If both buffer and disk are full, events are dropped with a counter metric (`lattice_accounting_events_dropped_total`). Scheduling continues.

### Operational Response to Buffer Overflow

When the accounting buffer fills and events are dropped:

1. **Detect:** `lattice_accounting_events_dropped_total` counter increments. Alert fires when > 0.
2. **Impact:** Billing data is incomplete. GPU-hours and allocation events are missing from Waldur. This affects invoice accuracy but never affects scheduling.
3. **Respond:**
   - Check Waldur availability (`lattice admin accounting status`)
   - If Waldur is down: wait for recovery. Buffered events will replay. Dropped events are lost.
   - If Waldur is up but slow: check push interval and batch size. Increase `push_interval_seconds` to allow larger batches.
4. **Recovery:** Dropped events cannot be recovered from the accounting pipeline. However, the quorum has allocation state (start/end times, node assignments). An admin can reconstruct missing billing data from quorum logs:
   ```bash
   lattice admin accounting reconcile --since=2026-03-01 --until=2026-03-02
   ```
   This command reads allocation history from the quorum and generates compensating events for Waldur.
5. **Prevention:** Size the buffer for expected Waldur outage duration. Rule of thumb: `buffer_size = events_per_minute × max_expected_outage_minutes`. For a busy cluster (100 events/min) and 2-hour outage target: `buffer_size = 12000`.

## Quota Feedback Loop

Waldur can act as the budget authority, updating Lattice tenant quotas:

1. Waldur detects budget exhaustion (e.g., project spent its allocated compute hours)
2. Waldur calls lattice-api: `PUT /v1/tenants/{id}/quotas` with reduced limits
3. Lattice updates hard/soft quotas (cross-ref: [quota-enforcement.md](quota-enforcement.md))
4. Effect: tenant's new allocations are blocked (hard quota) or deprioritized (soft quota)

Conversely, when a tenant purchases more compute:
1. Waldur increases the tenant's quota
2. Lattice picks up the new limits
3. Previously-starved allocations can now be scheduled

## Medical Accounting

Medical allocations have additional accounting requirements:

- All accounting events include the claiming user's identity (not just tenant)
- Idle node time (nodes claimed but no running allocation) is billable — Waldur receives `node.claimed` and `node.released` events
- Accounting events for medical allocations are also written to the Raft-committed audit log (cross-ref: [sensitive-workloads.md](sensitive-workloads.md))
- Waldur must retain medical billing records for 7 years (configured on the Waldur side)

## Configuration

```yaml
accounting:
  enabled: true                     # feature flag, default: false
  provider: "waldur"
  waldur:
    api_url: "https://waldur.example.com/api/"
    token_secret_ref: "vault://lattice/waldur-token"
    push_interval_seconds: 60       # batch push interval
    buffer_size: 10000              # in-memory event buffer
    disk_buffer_path: "/var/lib/lattice/accounting-wal"
    disk_buffer_max_events: 100000
```

When `accounting.enabled` is false, no accounting code runs and no Waldur dependency exists (same pattern as federation).

## Cross-References

- [quota-enforcement.md](quota-enforcement.md) — Waldur updates quotas, hard vs. soft semantics
- [failure-modes.md](failure-modes.md) — Accounting service failure buffering
- [security.md](security.md) — Waldur API token management
- [sensitive-workloads.md](sensitive-workloads.md) — Medical billing and audit requirements
- [telemetry.md](telemetry.md) — Accounting buffer metrics in scheduler self-monitoring

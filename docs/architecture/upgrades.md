# Upgrades and Rollouts

## Design Principle

Zero-downtime upgrades. No running allocation is disrupted by an upgrade. Components are upgraded independently. Protocol backward compatibility ensures mixed-version operation during rolling upgrades.

## Protocol Versioning

All gRPC services are versioned (`lattice.v1.*`):

- New fields are additive (backward compatible within a major version)
- Breaking changes require a new version (`lattice.v2.*`)
- During rolling upgrades, node agents and quorum members must support both version N and N-1
- Version negotiation on connection establishment: components advertise supported versions, use the highest common version

## Upgrade Order

Components are upgraded in dependency order, from leaf to core:

```
1. Node agents (rolling, batched)
2. vCluster schedulers (rolling)
3. API servers (rolling)
4. Quorum members (Raft rolling membership change, one at a time)
```

This order ensures that core components (quorum) speak the old protocol until all clients (node agents, schedulers) are upgraded. The quorum is upgraded last because it's the most critical and the hardest to roll back.

## Node Agent Rolling Upgrade

### Procedure

For each batch of nodes:

1. **Drain:** Stop scheduling new allocations to the node. Node enters `Draining` state. If no allocations are running, it transitions directly to `Drained`.
2. **Wait:** Running allocations complete naturally. The scheduler loop transitions the node from `Draining` to `Drained` once all allocations finish. For urgent upgrades: checkpoint running allocations and migrate (cross-ref: [checkpoint-broker.md](checkpoint-broker.md)).
3. **Upgrade:** Replace node agent binary while node is `Drained`. Configuration is preserved.
4. **Restart:** Node agent starts, re-registers with quorum using new protocol version.
5. **Health check:** Node passes health check (heartbeat, GPU detection, network test).
6. **Undrain:** Operator runs `undrain`. Node transitions from `Drained` to `Ready` and is available for scheduling.

### Canary Strategy

1. Upgrade 1-2 nodes first (canary set)
2. Monitor canary nodes for the observation window (default: 15 minutes):
   - Scheduling cycle latency within SLO (cross-ref: [telemetry.md](telemetry.md) scheduler self-monitoring)
   - No increase in allocation failures on canary nodes
   - Heartbeat latency stable
   - Node health check pass rate = 100%
3. If canary passes: proceed with rolling batches (batch size configurable, default: 5% of nodes)
4. If canary fails: stop rollout, revert canary nodes (see Rollback below)

### Batch Sizing

| Cluster Size | Canary Size | Batch Size | Total Batches |
|-------------|-------------|------------|---------------|
| < 50 nodes | 1 node | 5 nodes | ~10 |
| 50-500 nodes | 2 nodes | 25 nodes | ~20 |
| 500+ nodes | 5 nodes | 50 nodes | varies |

## vCluster Scheduler Rolling Upgrade

Schedulers are stateless — they read state from the quorum each cycle:

1. Stop scheduler instance
2. Upgrade binary
3. Restart
4. Verify: scheduling cycle completes successfully, proposals accepted by quorum

During scheduler downtime, the affected vCluster pauses scheduling (no new allocations). Running allocations are unaffected. Multiple scheduler replicas (if deployed) provide continuity.

## API Server Rolling Upgrade

API servers are stateless, behind a load balancer:

1. Remove instance from load balancer
2. Drain active connections (grace period: 30s)
3. Upgrade binary
4. Restart
5. Health check passes → re-add to load balancer

Client impact: brief connection reset for long-lived streams (StreamMetrics, StreamLogs). Clients reconnect automatically.

## Quorum Rolling Upgrade

The most sensitive upgrade. One member at a time, maintaining quorum majority throughout:

### 3-Member Quorum

1. Upgrade follower A: remove from Raft group → upgrade → re-add
2. Wait for follower A to catch up (Raft log sync)
3. Upgrade follower B: remove → upgrade → re-add
4. Wait for follower B to catch up
5. Trigger leader transfer to an upgraded follower
6. Upgrade old leader: remove → upgrade → re-add

**Constraint:** Never more than 1 member down simultaneously (2/3 majority required).

### 5-Member Quorum

Same procedure but can upgrade 2 followers in parallel (3/5 majority maintained):

1. Upgrade followers A and B in parallel
2. Wait for catch-up
3. Upgrade followers C and D in parallel
4. Wait for catch-up
5. Leader transfer → upgrade old leader

**Constraint:** Never more than 2 members down simultaneously (3/5 majority required).

### Quorum Upgrade Verification

After each member upgrade:
- Raft log replication is current (no lag)
- Commit latency within SLO (< 5s)
- Leader election succeeds if triggered
- All node ownership state is consistent

## Canary Criteria

Metrics from scheduler self-monitoring (cross-ref: [telemetry.md](telemetry.md)) that gate rollout progression:

| Metric | Threshold | Severity |
|--------|-----------|----------|
| `lattice_scheduling_cycle_duration_seconds` | p99 < 30s | Warning: pause rollout |
| `lattice_scheduling_proposals_total{result="rejected"}` | No increase > 10% | Warning: pause rollout |
| `lattice_agent_heartbeat_latency_seconds` | p99 < 5s | Warning: pause rollout |
| `lattice_raft_commit_latency_seconds` | p99 < 5s | Critical: stop rollout |
| `lattice_api_requests_total{status="5xx"}` | No increase > 5% | Warning: pause rollout |
| Allocation failure rate | No increase | Critical: stop rollout |

## Rollback

### Node Agent Rollback

1. Drain canary/failed nodes
2. Replace binary with previous version
3. Restart
4. Verify old-version operation
5. Protocol backward compatibility ensures the rolled-back agent works with the rest of the cluster

### Scheduler/API Rollback

Stateless — replace binary and restart.

### Quorum Rollback

1. Remove new-version member from Raft group
2. Add old-version member back
3. Protocol backward compatibility ensures mixed-version operation during the transition

Rollback is always safe because N-1 protocol support is maintained throughout the upgrade window.

## Configuration Hot-Reload

Not all changes require a binary upgrade. Configuration changes that can be hot-reloaded via quorum without restart:

| Change | Hot-Reloadable | Mechanism |
|--------|---------------|-----------|
| Cost function weights | Yes | Quorum config update, schedulers pick up next cycle |
| vCluster policies | Yes | Quorum config update |
| Telemetry mode (prod/debug/audit) | Yes | API call to node agent |
| Tenant quotas | Yes | Quorum config update |
| Node drain/undrain | Yes | API call |
| Protocol version | No | Binary upgrade required |
| Raft cluster size | No | Membership change (safe, but not hot-reload) |

## Cross-References

- [telemetry.md](telemetry.md) — Scheduler self-monitoring metrics used for canary criteria
- [failure-modes.md](failure-modes.md) — Failure detection during upgrades
- [security.md](security.md) — Certificate rotation during upgrades
- [checkpoint-broker.md](checkpoint-broker.md) — Checkpoint before drain for urgent upgrades

# Node Lifecycle

## Design Principle

Nodes follow a formal state machine with well-defined transitions, timeouts, and operator actions. The node agent drives transitions locally; the quorum records ownership changes with strong consistency. Running allocations are never disrupted by state transitions unless the node is genuinely unhealthy.

## State Machine

```
                    ┌────────────────────────────────────────────┐
                    │                                            │
                    ▼                                            │
  ┌─────────┐   boot   ┌──────────┐   health ok   ┌─────────┐    │
  │ Unknown │────────→ │ Booting  │──────────────→│  Ready  │    │
  └─────────┘          └──────────┘               └────┬────┘    │
       ▲                     │                         │         │
       │               boot fail                       │         │
       │                     │        ┌────────────────┤         │
       │                     ▼        │                │         │
       │               ┌──────────┐   │  drain cmd     │         │
       │               │  Failed  │   │       │        │         │
       │               └──────────┘   │       ▼        │         │
       │                     │        │  ┌──────────┐  │  remediated
       │               wipe/reboot    │  │ Draining │  │         │
       │                     │        │  └─────┬────┘  │         │
       │                     │        │   allocs done  │         │
       │                     │        │        │       │         │
       │                     │        │        ▼       │         │
       │                     │        │  ┌──────────┐  │         │
       │                     │        │  │ Drained  │  │         │
       │                     │        │  └─────┬────┘  │         │
       │                     │        │ undrain│       │         │
       │                     │        │        │       │         │
       │                     │        │        ▼       │         │
       │                     │        └──→ (Ready) ◄───┘         │
       │                     │                                   │
       │                     │    heartbeat miss    ┌───────────┐│
       │                     │    ┌────────────────→│ Degraded  ││
       │                     │    │   (Ready)       └─────┬─────┘│
       │                     │    │                 grace timeout│
       │                     │    │                       │      │
       │                     │    │                       ▼      │
       │                     └────┼──────────────────┌─────────┐ │
       │                          │                  │  Down   │ │
       └──────────────────────────┼──────────────────└────┬────┘ │
                                  │                 reboot│      │
                                  │                       └──────┘
                                  │
                         heartbeat resume
                          (within grace)
                                  │
                                  └──→ (Ready)
```

## States

| State | Description | Schedulable | Allocations Run |
|-------|-------------|-------------|-----------------|
| `Unknown` | Node exists in inventory but has never reported | No | No |
| `Booting` | OpenCHAMI booting/reimaging the node | No | No |
| `Ready` | Healthy, agent reporting, available for scheduling | Yes | Yes |
| `Degraded` | Heartbeat missed or minor issue detected | No (new) | Yes (existing) |
| `Down` | Confirmed failure, grace period expired | No | No (requeued) |
| `Draining` | Operator or scheduler requested drain, waiting for allocations to finish | No (new) | Yes (existing, draining) |
| `Drained` | All allocations completed/migrated after drain | No | No |
| `Failed` | Boot failure or unrecoverable hardware error | No | No |

## Transitions

### Ready → Degraded

**Trigger:** First missed heartbeat.

**Timeout:** `heartbeat_interval` (default: 30s). If no heartbeat received within this window, the quorum marks the node `Degraded`.

**Effect:** Node is removed from scheduling candidates for new allocations. Running allocations continue undisturbed. No user notification.

**Sensitive override:** Sensitive nodes use a longer degradation window (default: 2 minutes) to avoid false positives from transient network issues.

### Degraded → Ready

**Trigger:** Heartbeat resumes within the grace period.

**Effect:** Node re-enters the scheduling pool. No allocation disruption occurred. Event logged but no alert.

### Degraded → Down

**Trigger:** Grace period expired without heartbeat recovery.

**Timeouts:**
| Node Type | Grace Period | Rationale |
|-----------|-------------|-----------|
| Standard | 60s | Balance between fast recovery and false positive avoidance |
| Sensitive | 5 minutes | Sensitive allocations are high-value; avoid premature requeue |
| Borrowed | 30s | Borrowed nodes should be reclaimed quickly |

**Effect:**
1. All allocations on the node are evaluated per their requeue policy (cross-ref: [failure-modes.md](failure-modes.md))
2. Node ownership released (Raft commit)
3. Alert raised to operators
4. OpenCHAMI notified for out-of-band investigation (Redfish BMC check)

### Ready → Draining

**Trigger:** Explicit operator command (`lattice node drain <id>`) or scheduler-initiated (upgrade, conformance drift on sensitive node).

**Effect:**
1. Node removed from scheduling candidates
2. Running allocations continue until completion
3. For urgent drains: scheduler may trigger checkpoint on running allocations (cross-ref: [checkpoint-broker.md](checkpoint-broker.md))
4. No new allocations assigned

### Draining → Drained

**Trigger:** All running allocations on the node have completed, been checkpointed, or been migrated.

**Effect:** Node is idle and safe for maintenance. Operator can upgrade, reboot, or reimage.

### Drained → Ready

**Trigger:** Operator undrain (`lattice node undrain <id>`). Typically after maintenance.

**Precondition:** Node agent health check passes (heartbeat, GPU detection, network test, conformance fingerprint computed).

**Effect:** Node re-enters scheduling pool.

### Any → Down (hardware failure)

**Trigger:** OpenCHAMI Redfish BMC detects critical hardware failure (PSU, uncorrectable ECC, GPU fallen off bus).

**Effect:** Immediate transition to `Down`, bypassing grace period. Same allocation handling as Degraded → Down.

### Down → Booting

**Trigger:** Operator or automated remediation initiates reboot/reimage via OpenCHAMI.

**Effect:** Node enters `Booting` state. OpenCHAMI BSS serves the appropriate image.

### Booting → Ready

**Trigger:** Node agent starts, passes health check, reports to quorum.

**Health check:** Heartbeat received, GPU count matches capabilities, NIC firmware detected, conformance fingerprint computed and reported.

### Booting → Failed

**Trigger:** Boot timeout (default: 10 minutes) or repeated boot failures (3 consecutive).

**Effect:** Node marked `Failed`. Alert raised. Operator must investigate.

## Sensitive Node Lifecycle Extensions

Sensitive nodes have additional constraints:

| Event | Standard Node | Sensitive Node |
|-------|--------------|-------------|
| Claim | Scheduler assigns | User claims explicitly, Raft-committed |
| Degraded grace | 60s | 5 minutes |
| Down → requeue | Automatic | Operator intervention required |
| Release | Node returns to pool | Node must be wiped (OpenCHAMI secure erase) before returning |
| Conformance drift | Deprioritized | Immediate `Draining`, audit logged |

### Sensitive Release Sequence

```
1. User releases sensitive allocation
2. Quorum releases node ownership (Raft commit, audit entry)
3. Node enters Draining (if other sensitive allocations) or proceeds to wipe
4. OpenCHAMI initiates secure wipe:
   a. GPU memory clear
   b. NVMe secure erase (if present)
   c. RAM scrub
   d. Reboot into clean image
5. Wipe confirmation reported to quorum (Raft commit, audit entry)
6. Node transitions to Ready and returns to general pool
```

### Wipe Failure Handling

If the OpenCHAMI secure wipe fails or times out during sensitive node release:

1. **Timeout:** Default wipe timeout is 30 minutes (configurable: `sensitive.wipe_timeout`). If wipe does not complete within this window, the node enters a `Quarantine` state (treated as `Down` by the scheduler).
2. **Quarantine:** Quarantined nodes are excluded from scheduling and flagged for operator intervention. They do not return to the general pool.
3. **Operator intervention:** The operator investigates (BMC console, hardware diagnostics) and either:
   - Retries the wipe: `lattice admin node wipe <id> --force`
   - Replaces the node hardware
   - Marks the node as permanently failed: `lattice node disable <id>`
4. **Audit:** Wipe failures are logged as critical audit events (Raft-committed for sensitive nodes). The audit entry records: node ID, wipe start time, failure reason, operator action.
5. **Alert:** `lattice_sensitive_wipe_failure_total` counter incremented; critical alert fired.

## Operator Commands

| Command | Effect | Confirmation Required |
|---------|--------|----------------------|
| `lattice node drain <id>` | Start draining | No |
| `lattice node drain <id> --urgent` | Drain with checkpoint trigger | Yes (allocations will be checkpointed) |
| `lattice node undrain <id>` | Re-enable scheduling | No |
| `lattice node disable <id>` | Transition to Down immediately | Yes (allocations will be requeued/failed) |
| `lattice node enable <id>` | Re-enable a disabled node (Down → Ready) | No |
| `lattice node status <id>` | Show current state, allocations, health | No |
| `lattice node list --state=degraded` | List nodes in specific state | No |

## Heartbeat Protocol

Node agents send heartbeats to the quorum at a configurable interval:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `heartbeat_interval` | 10s | How often the agent sends a heartbeat |
| `heartbeat_timeout` | 30s | Quorum marks `Degraded` after this silence |
| `grace_period` | 60s | `Degraded` → `Down` after this additional silence |
| `sensitive_grace_period` | 5m | Extended grace for sensitive nodes |

Heartbeats include:
- Monotonic sequence number (replay detection)
- Node health summary (GPU count, temperature, ECC errors)
- Conformance fingerprint (if recomputed since last heartbeat)
- Running allocation count

Heartbeats are lightweight (~200 bytes) and sent over the management traffic class (cross-ref: [security.md](security.md)).

## Agent Restart and State Recovery

The node agent persists active allocation state to `/var/lib/lattice/agent-state.json` (configurable via `--state-file`). This enables workload survival across agent restarts.

**On graceful shutdown (SIGTERM):**
1. Agent writes current allocation state (PIDs, cgroup paths, runtime type, mount points) to the state file
2. Agent exits without killing workloads (systemd `KillMode=process`)

The pid written to the state file is sourced from the in-memory
`LocalAllocation.pid` field (INT-4): when the runtime monitor task
spawns the workload, it calls `AllocationManager::set_pid()`
immediately after `runtime.spawn()` returns a handle with a concrete
pid. Without this wiring, reattach after agent restart would fall
back to the cgroup-scan path only, and `lattice session attach`
(which ultimately needs `nsenter` into the workload's namespaces)
would have no way to look up the target pid locally.

**On startup:**
1. Agent reads the persisted state file
2. For each allocation, checks if the process is still alive (`kill(pid, 0)`)
3. Alive processes are reattached — agent resumes heartbeating their status;
   the `reattach_in_progress` flag is raised on the next heartbeat
   (one-way, cleared after a successful CompletionReport or the
   reattach-grace timer elapses — INV-D5).
4. Dead processes are treated as orphans — cgroup scopes are destroyed, mounts cleaned up
5. Stray cgroup scopes under `workload.slice/alloc-*.scope` with no matching state entry are also cleaned up
6. Agent re-registers with quorum and resumes normal operation

**Crash recovery:** If the agent crashes without writing the state file, the startup scan of cgroup scopes under `workload.slice/` provides a fallback discovery mechanism for orphaned workloads.

## Cross-References

- [failure-modes.md](failure-modes.md) — Allocation requeue on node failure
- [conformance.md](conformance.md) — Conformance drift triggers drain on sensitive nodes
- [upgrades.md](upgrades.md) — Drain/undrain during rolling upgrades
- [checkpoint-broker.md](checkpoint-broker.md) — Checkpoint on urgent drain
- [sensitive-workloads.md](sensitive-workloads.md) — Sensitive node claim/release/wipe
- [security.md](security.md) — Heartbeat authentication (mTLS, sequence numbers)

# Node Lifecycle

## Design Principle

Nodes follow a formal state machine with well-defined transitions, timeouts, and operator actions. The node agent drives transitions locally; the quorum records ownership changes with strong consistency. Running allocations are never disrupted by state transitions unless the node is genuinely unhealthy.

## State Machine

```
                    ┌──────────────────────────────────────────────┐
                    │                                              │
                    ▼                                              │
  ┌─────────┐   boot    ┌──────────┐   health ok   ┌─────────┐   │
  │ Unknown  │────────→│ Booting   │─────────────→│  Ready  │   │
  └─────────┘          └──────────┘              └────┬────┘   │
       ▲                     │                         │         │
       │               boot fail                       │         │
       │                     │         ┌───────────────┤         │
       │                     ▼         │               │         │
       │               ┌──────────┐   │  drain cmd    │         │
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
       │                     │        │  undrain│      │         │
       │                     │        │        │       │         │
       │                     │        │        ▼       │         │
       │                     │        └──→ (Ready) ◄───┘         │
       │                     │                                    │
       │                     │    heartbeat miss    ┌───────────┐│
       │                     │    ┌────────────────→│ Degraded  ││
       │                     │    │   (Ready)        └─────┬─────┘│
       │                     │    │                  grace timeout│
       │                     │    │                        │      │
       │                     │    │                        ▼      │
       │                     └────┼──────────────────┌─────────┐ │
       │                          │                  │  Down   │ │
       └──────────────────────────┼──────────────────└────┬────┘ │
                                  │                  reboot│      │
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

**Medical override:** Medical nodes use a longer degradation window (default: 2 minutes) to avoid false positives from transient network issues.

### Degraded → Ready

**Trigger:** Heartbeat resumes within the grace period.

**Effect:** Node re-enters the scheduling pool. No allocation disruption occurred. Event logged but no alert.

### Degraded → Down

**Trigger:** Grace period expired without heartbeat recovery.

**Timeouts:**
| Node Type | Grace Period | Rationale |
|-----------|-------------|-----------|
| Standard | 60s | Balance between fast recovery and false positive avoidance |
| Medical | 5 minutes | Medical allocations are high-value; avoid premature requeue |
| Borrowed | 30s | Borrowed nodes should be reclaimed quickly |

**Effect:**
1. All allocations on the node are evaluated per their requeue policy (cross-ref: [failure-modes.md](failure-modes.md))
2. Node ownership released (Raft commit)
3. Alert raised to operators
4. OpenCHAMI notified for out-of-band investigation (Redfish BMC check)

### Ready → Draining

**Trigger:** Explicit operator command (`lattice node drain <id>`) or scheduler-initiated (upgrade, conformance drift on medical node).

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

## Medical Node Lifecycle Extensions

Medical nodes have additional constraints:

| Event | Standard Node | Medical Node |
|-------|--------------|-------------|
| Claim | Scheduler assigns | User claims explicitly, Raft-committed |
| Degraded grace | 60s | 5 minutes |
| Down → requeue | Automatic | Operator intervention required |
| Release | Node returns to pool | Node must be wiped (OpenCHAMI secure erase) before returning |
| Conformance drift | Deprioritized | Immediate `Draining`, audit logged |

### Medical Release Sequence

```
1. User releases medical allocation
2. Quorum releases node ownership (Raft commit, audit entry)
3. Node enters Draining (if other medical allocations) or proceeds to wipe
4. OpenCHAMI initiates secure wipe:
   a. GPU memory clear
   b. NVMe secure erase (if present)
   c. RAM scrub
   d. Reboot into clean image
5. Wipe confirmation reported to quorum (Raft commit, audit entry)
6. Node transitions to Ready and returns to general pool
```

## Operator Commands

| Command | Effect | Confirmation Required |
|---------|--------|----------------------|
| `lattice node drain <id>` | Start draining | No |
| `lattice node drain <id> --urgent` | Drain with checkpoint trigger | Yes (allocations will be checkpointed) |
| `lattice node undrain <id>` | Re-enable scheduling | No |
| `lattice node disable <id>` | Transition to Down immediately | Yes (allocations will be requeued/failed) |
| `lattice node status <id>` | Show current state, allocations, health | No |
| `lattice node list --state=degraded` | List nodes in specific state | No |

## Heartbeat Protocol

Node agents send heartbeats to the quorum at a configurable interval:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `heartbeat_interval` | 10s | How often the agent sends a heartbeat |
| `heartbeat_timeout` | 30s | Quorum marks `Degraded` after this silence |
| `grace_period` | 60s | `Degraded` → `Down` after this additional silence |
| `medical_grace_period` | 5m | Extended grace for medical nodes |

Heartbeats include:
- Monotonic sequence number (replay detection)
- Node health summary (GPU count, temperature, ECC errors)
- Conformance fingerprint (if recomputed since last heartbeat)
- Running allocation count

Heartbeats are lightweight (~200 bytes) and sent over the management traffic class (cross-ref: [security.md](security.md)).

## Cross-References

- [failure-modes.md](failure-modes.md) — Allocation requeue on node failure
- [conformance.md](conformance.md) — Conformance drift triggers drain on medical nodes
- [upgrades.md](upgrades.md) — Drain/undrain during rolling upgrades
- [checkpoint-broker.md](checkpoint-broker.md) — Checkpoint on urgent drain
- [sensitive-workloads.md](sensitive-workloads.md) — Medical node claim/release/wipe
- [security.md](security.md) — Heartbeat authentication (mTLS, sequence numbers)

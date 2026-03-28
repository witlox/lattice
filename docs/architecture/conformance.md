# Node Conformance & Configuration Drift

## Problem

In large-scale HPC systems, nodes gradually drift from their intended configuration: firmware versions diverge, driver updates are applied unevenly, kernel parameters change. This **configuration drift** causes:

- **Silent performance degradation.** A 64-node NCCL training run where one node has a different NIC firmware version may see unexplained slowdowns or hangs.
- **Correctness issues.** Mismatched GPU driver versions can produce different numerical results.
- **Compliance violations.** Regulated workloads require provable consistency of the execution environment.

## Design Principle

**The scheduler does not manage node configuration — OpenCHAMI does.** The scheduler only needs to know whether nodes are the same or different, and how strict the workload's homogeneity requirements are. Detection is the node agent's job. Remediation is OpenCHAMI's job.

## Conformance Fingerprint

Each node agent computes a **conformance fingerprint**: a hash of the node's configuration-critical software and firmware versions.

Components included in the fingerprint:
- GPU driver version (e.g., NVIDIA 550.54.14)
- NIC firmware version (Slingshot/UE adapter firmware)
- BIOS/BMC firmware version (reported via Redfish/OpenCHAMI)
- Kernel version and boot parameters
- uenv base image hash (for sensitive: the hardened OS image)

The fingerprint is a content hash (SHA-256 of the sorted component list). Nodes with identical fingerprints belong to the same **conformance group**.

### Reporting

The node agent reports the conformance fingerprint alongside its existing health data. This is **eventually consistent** — conformance group membership does not go through Raft (it's derived from node agent reports, same as health status).

Exception: for sensitive nodes, conformance state changes are recorded in the Raft-committed audit log (per sensitive workload requirements).

### Staleness

The node agent recomputes the fingerprint:
- On startup
- Periodically (default: every 6 hours)
- On explicit request from the scheduler (e.g., after OpenCHAMI remediation)

If a node hasn't reported a fingerprint within the staleness window, the scheduler treats it as **unknown conformance** — equivalent to a unique conformance group of one.

## Scheduling Integration

### Cost Function (f₉)

See [scheduling-algorithm.md](scheduling-algorithm.md) for the full cost function. The conformance factor `f₉` scores how homogeneous the candidate node set is:

```
f₉(j, candidates) = largest_conformance_group_size(candidates) / j.requested_nodes
```

- 1.0 → all candidate nodes share the same fingerprint
- 0.5 → half the nodes match, half differ
- Low values → highly heterogeneous set

### Node Selection

During node selection (solver step 2a), the solver prefers nodes from the same conformance group:

1. Among nodes satisfying constraints (GPU type, topology, etc.), group by conformance fingerprint
2. Select the largest conformance group that can satisfy the node count
3. If no single group is large enough, merge groups (with a scoring penalty via f₉)
4. For single-node jobs, conformance is irrelevant (f₉ = 1.0 trivially)

### Per-vCluster Policy

| vCluster Type | Conformance Behavior |
|---|---|
| HPC Batch | Soft preference (w₉=0.10). Prefers homogeneous sets but will mix if needed. |
| ML Training | Strong preference (w₉=0.25). Multi-node training is sensitive to driver mismatches. |
| Service | Weak preference (w₉=0.05). Services are usually single-node or tolerate heterogeneity. |
| Sensitive | Hard constraint at solver level (drifted nodes excluded before scoring). w₉=0.10 as tiebreaker among conformant nodes. |
| Interactive | Ignored (w₉=0.00). Short-lived, single-node, not sensitive to drift. |

## Drift Response

When the scheduler detects that a node's conformance fingerprint has changed (or diverged from the majority in its group):

1. **Continue running workloads.** Existing allocations are not disrupted — the drift already happened, and disrupting would make things worse.
2. **Stop scheduling new work.** The node is deprioritized for new allocations (it now belongs to a smaller conformance group, scoring lower on f₉).
3. **Signal OpenCHAMI.** The scheduler (or node agent) notifies OpenCHAMI that the node has drifted, triggering remediation (firmware update, reboot into correct image, etc.).
4. **For sensitive nodes:** additionally flag the drift in the audit log and set the node to `Draining` (transitioning to `Drained` once active allocations complete) — no new sensitive claims until remediated and verified. After remediation, an operator undoes the drain (`Drained` → `Ready`).

The scheduler does **not** attempt to remediate drift itself. It only avoids scheduling on drifted nodes and signals the infrastructure layer to fix them.

### OpenCHAMI Coordination

When the scheduler detects drift:

1. **Signal:** The node agent (or scheduler) calls OpenCHAMI SMD to report the drift:
   ```
   PATCH /hsm/v2/State/Components/{xname}
   { "Flag": "Warning", "FlagMsg": "conformance_drift: expected=<hash_a>, actual=<hash_b>" }
   ```

2. **OpenCHAMI response:** OpenCHAMI evaluates the drift against its remediation policy:
   - Minor drift (kernel param change): schedule firmware update at next maintenance window
   - Major drift (GPU driver version): schedule immediate reboot into correct image via BSS
   - Critical drift (sensitive node): immediate remediation, operator notified

3. **Wait for remediation:** The scheduler does not re-enable the node automatically. After OpenCHAMI remediates (reboot, firmware flash), the node agent:
   - Recomputes conformance fingerprint on startup
   - Reports new fingerprint to quorum
   - If fingerprint matches expected baseline: node returns to Ready
   - If still drifted: remains deprioritized, alert escalated

4. **Timeout:** If a node remains drifted for longer than `drift_remediation_timeout` (default: 24 hours):
   - Alert escalated to critical
   - Node transitions to `Down` (removed from scheduling entirely)
   - Operator must investigate and manually undrain after fix

5. **Sensitive nodes (stricter):**
   - Drift triggers immediate `Draining` (no grace period for new claims)
   - Remediation timeout: 4 hours (shorter, due to regulatory risk)
   - After remediation: conformance re-verified AND admin approval required before accepting sensitive claims again

## Relationship to Existing Concepts

- **NodeHealth** tracks whether the node is functional (Healthy/Degraded/Down/Draining). Conformance is orthogonal — a node can be Healthy but drifted.
- **NodeCapabilities** tracks what the node *has* (GPU type, memory). Conformance tracks whether the node's software stack matches expectations.
- **Topology (GroupId)** tracks physical location. Conformance tracks software configuration. Both are inputs to node selection: pack by topology AND by conformance group.

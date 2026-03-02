# Sensitive, Medical & Regulated Workload Design

## Threat Model

Sensitive & Medical workloads on shared HPC infrastructure face regulatory requirements (Swiss FADP, EU GDPR, potentially HIPAA for international collaboration). The design must be defensible to an auditor.

**What we must prove:**
1. Patient data was only accessible to authorized users during processing
2. No other tenant's workload ran on the same physical nodes simultaneously
3. Data was encrypted at rest and in transit
4. All access was logged with user identity and timestamp
5. Data was destroyed when no longer needed
6. Data did not leave the designated jurisdiction

## Isolation Model: User Claims Node

Unlike other vClusters where the scheduler assigns nodes, **medical nodes are claimed by a specific user**:

```
Dr. X authenticates via OIDC (institutional IdP)
  → Requests 4 nodes via FirecREST: POST /v1/allocations (medical vCluster)
  → Quorum records: nodes N1-N4 owned by user:dr-x, tenant:hospital-a
  → Strong consistency: Raft commit before any workload starts
  → OpenCHAMI boots N1-N4 with hardened medical image (if not already)
  → All activity on N1-N4 audited under dr-x's identity
  → When released:
    → Quorum releases node ownership (Raft commit)
    → OpenCHAMI wipes node (memory scrub, storage secure erase if NVMe present)
    → Node returns to general pool only after wipe confirmation
```

**No clever optimization on medical nodes.** If Dr. X claims 4 nodes at 9am and runs nothing until 2pm, those nodes sit idle. The cost is real and should be visible to the tenant's accounting. But there is no co-scheduling, no borrowing, no time-sharing.

## OS Image

Medical nodes boot a hardened image via OpenCHAMI BSS:
- Minimal kernel, no unnecessary services
- Mandatory access control (SELinux/AppArmor enforcing)
- No SSH daemon (all access via API gateway)
- Encrypted swap (if any)
- Audit daemon (auditd) logging all syscalls to audit subsystem
- Node agent with audit mode telemetry enabled by default

## Software Delivery

Medical allocations use **signed uenv images only**:

```yaml
environment:
  uenv: "medical/validated-2024.1"  # curated, audited base stack
  sign_required: true                # image signature verified before mount
  scan_required: true                # CVE scan passed
  approved_bases_only: true          # can only use admin-approved base images
```

The uenv registry enforces:
- Image signing (with Sovra keys or site-specific PKI)
- Vulnerability scanning (integrated with JFrog/Nexus security scanning)
- Approved base image list (maintained by site security team)
- Audit log of all image pulls

## Storage

Medical data lives in a dedicated storage pool:

```yaml
storage_policy:
  pool: "medical-encrypted"          # dedicated VAST view/tenant
  encryption: "aes-256-at-rest"      # VAST native encryption
  access_logging: "full"             # every read/write logged via VAST audit
  wipe_on_release: true              # VAST secure delete on allocation end
  data_sovereignty: "ch"             # data stays in Swiss jurisdiction
  retention:
    data: "user_specified"           # user declares retention period
    audit_logs: "7_years"            # regulatory minimum
  tier_restriction: "hot_only"       # no copies on shared warm/cold tiers
```

## Network Isolation

Medical allocations get a dedicated Slingshot VNI:

```yaml
connectivity:
  network_domain: "medical-{user}-{alloc_id}"  # unique per allocation
  policy:
    ingress: deny-all-except:
      - same_domain                  # only processes in this allocation
      - data_gateway                 # controlled data ingress endpoint
    egress: deny-all-except:
      - data_gateway                 # controlled data egress
```

With Ultra Ethernet: network-level encryption (UET built-in) provides an additional layer without performance penalty.

## Audit Trail

### What is logged (strong consistency via Raft):
- Node claim: user identity, timestamp, node IDs
- Node release: user identity, timestamp, wipe confirmation
- Allocation start/stop: what ran, which uenv image (with hash), which data paths
- Data access: every file open/read/write (from eBPF audit telemetry)
- API calls: every FirecREST/lattice-api call related to medical allocations
- Checkpoint events: when, where, what was written
- Attach sessions: user identity, start/end timestamps, target node, session recording reference
- Log access events: who accessed logs, when, which allocation
- Metrics queries: user identity, allocation queried, timestamp

### Storage:
- Append-only log (no deletions, no modifications)
- Encrypted at rest (Sovra-managed keys if federation enabled, site PKI otherwise)
- 7-year retention on cold tier (S3-compatible, immutable storage)
- Cryptographically signed entries (tamper-evident)

### Query Interface

The audit log is queryable via a dedicated API endpoint and CLI:

**API:**
```
GET /v1/audit/logs?user=dr-x&since=2026-03-01&until=2026-03-15
GET /v1/audit/logs?allocation=12345
GET /v1/audit/logs?node=x1000c0s0b0n0&since=2026-03-01
GET /v1/audit/logs?data_path=s3://medical-data/patient-001/
```

**CLI:**
```bash
lattice audit query --user=dr-x --since=2026-03-01 --until=2026-03-15
lattice audit query --alloc=12345
lattice audit query --node=x1000c0s0b0n0 --since=2026-03-01 --output=json
```

**Scoping:**
| Caller | Visible Scope |
|--------|---------------|
| Claiming user | Own audit events only |
| Tenant admin (compliance reviewer) | All audit events for their tenant |
| System admin | All audit events |

**Indexing:** Audit entries are indexed by:
- User ID (primary query dimension for compliance reporting)
- Allocation ID (all events for a specific allocation)
- Node ID (all events on a specific node)
- Timestamp (range queries, required for all queries)
- Event type (filter by: claim, release, data_access, attach, etc.)

**Performance targets:**
| Query Scope | Expected Latency |
|-------------|-----------------|
| Single allocation (any timeframe) | < 1s |
| Single user, 1-day range | < 2s |
| Single user, 30-day range | < 10s |
| Tenant-wide, 1-day range | < 30s |

Queries spanning more than 90 days may be served from cold tier (S3 archive) with higher latency (minutes).

**Export:** For regulatory submissions, audit logs can be exported as signed JSON bundles:
```bash
lattice audit export --user=dr-x --since=2026-01-01 --until=2026-06-30 --output=audit-report.json.sig
```
The export includes cryptographic signatures for tamper evidence.

## Observability Constraints

Every user-facing observability feature has medical-specific restrictions. The principle: observability must not weaken the isolation model.

### Attach

- **Claiming user only.** The user who claimed the nodes (identity verified against Raft audit log) is the only user permitted to attach. No delegation, no shared access.
- **Session recording.** All attach sessions are recorded (input + output bytes) and stored in the medical audit log. The session recording reference is a Raft-committed audit entry.
- **Signed uenv only.** Attach is only permitted when the allocation runs a signed, vulnerability-scanned uenv image. This prevents attaching to environments with unvetted tools.
- **No concurrent attach from different sessions.** One active attach session per allocation at a time (prevents accidental data exposure via shared terminal).

### Logs

- **Encrypted at rest.** Logs from medical allocations are stored in the dedicated encrypted S3 pool (same as medical data).
- **Access-logged.** Every log access (live tail or historical) generates an audit entry with user identity and timestamp.
- **Restricted access.** Only the claiming user and designated compliance reviewers (via tenant admin role) can access logs.
- **Retention follows data policy.** Log retention matches the allocation's medical data retention policy, not the default log retention.

### Metrics

- **Low sensitivity, still scoped.** Metrics (GPU%, CPU%, I/O rates) do not contain patient data, but are still scoped to the claiming user. Tenant admins can view aggregated usage.
- **No cross-tenant visibility.** Even system admins see medical allocation metrics only in aggregate (holistic view), not per-allocation detail.

### Diagnostics

- **No cross-allocation comparison for medical.** The `CompareMetrics` RPC rejects requests that include medical allocation IDs alongside non-medical ones. Comparison within a single medical tenant is permitted (same claiming user).
- **Network diagnostics scoped.** Network diagnostics for medical allocations only show the allocation's own VNI traffic, not fabric-wide metrics.

### Profiling

- **Signed tools_uenv only.** Profiling tools must be delivered via a signed, approved `tools_uenv` image. Users cannot load arbitrary profiler binaries.
- **Profile output stays in medical pool.** All profiling output is written to the encrypted medical storage pool and is subject to the same access logging and retention policies.

## Federation Constraints

Medical data **does not federate** by default:
- Data stays at the designated site (data sovereignty)
- Compute can theoretically federate (run at remote site), but only if:
  - Remote site meets the same compliance requirements
  - Data does not transit (remote compute accesses data via encrypted API, not bulk transfer)
  - Both sites' Sovra instances have a medical workspace with hospital CRK
- In practice: medical jobs run where the data is. Period.

## Conformance Requirements

Medical nodes have **strict conformance enforcement**. Unlike general workloads where conformance is a soft preference, medical workloads treat configuration drift as a hard constraint:

- **Pre-claim validation.** Before a node can be claimed for medical use, the scheduler verifies its conformance fingerprint matches the expected baseline for the medical vCluster. Drifted nodes are rejected.
- **Drift triggers drain.** If a medical node's conformance fingerprint changes during operation (e.g., a firmware update was missed), the node agent flags the drift. The scheduler will not assign new medical claims to the node until OpenCHAMI remediates it.
- **Audit trail.** Conformance state changes on medical nodes are recorded in the Raft-committed audit log (which firmware/driver versions were active during the allocation).

This is deliberately conservative: medical workloads do not tolerate the subtle failures that configuration drift can cause, and regulatory compliance requires provable consistency of the execution environment.

## Scheduler Behavior

The medical vCluster scheduler is intentionally simple:
- **Algorithm:** Reservation-based (not knapsack). User claims nodes, scheduler validates and commits.
- **No backfill.** Medical nodes are not shared.
- **No preemption.** Medical allocations are never preempted.
- **No elastic borrowing.** Medical nodes cannot be borrowed by other vClusters.
- **Fair-share:** Not applicable (nodes are user-claimed, not queue-scheduled).
- **Conformance:** Hard constraint — only nodes matching the expected conformance baseline are eligible.
- **Cost function weights:** priority=0.9, conformance=1.0, everything else near-zero.

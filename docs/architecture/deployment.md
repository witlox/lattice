# Deployment & Bootstrapping

## Design Principle

Lattice deploys on bare metal managed by OpenCHAMI. The bootstrap sequence is deterministic: infrastructure first, then control plane, then compute nodes. Each step is idempotent and can be retried. The system can be fully rebuilt from configuration files and Raft snapshots.

## Prerequisites

Before deploying Lattice:

| Dependency | Required | Notes |
|------------|----------|-------|
| OpenCHAMI | Yes | Node inventory, BMC discovery, boot service, identity (OPAAL) |
| VAST (or compatible NFS+S3) | Yes | Hot tier storage, QoS API |
| OIDC Provider | Yes | User authentication (institutional IdP) |
| PKI / Certificate Authority | Yes | mTLS certificates for all components |
| Secrets Manager | Yes | API tokens, TLS keys (Vault or equivalent) |
| Time-series database | Yes | VictoriaMetrics, Mimir, or Thanos |
| Slingshot/UE fabric | Yes | Network with VNI support |
| Waldur | Optional | External accounting (feature-flagged) |
| Sovra | Optional | Federation trust (feature-flagged) |

## Network Topology

Lattice runs on the high-speed network (HSN — Slingshot/Ultra Ethernet, 200G+). When co-deployed with PACT, the two systems use different networks for clean failure isolation (PACT ADR-017):

| System | Network | Ports | Traffic |
|--------|---------|-------|---------|
| **PACT** | Management (1G) | gRPC 9443, Raft 9444 | Admin ops, boot overlay, config, shell |
| **Lattice** | HSN (200G+) | gRPC 50051, Raft 9000, REST 8080 | Scheduling, heartbeats, telemetry, allocation lifecycle |

```
Node (dual-homed):
├── Management NIC (1G Ethernet)
│   └── pact-agent ←mTLS→ pact-journal:9443
│
├── HSN NIC (200G+ Slingshot/UE)
│   ├── lattice-node-agent ←mTLS→ lattice-quorum:50051
│   └── workload traffic (MPI, NCCL, storage data plane)
│
└── SPIRE agent socket (local, network-agnostic)
    ├── pact-agent obtains SVID → uses on management net
    └── lattice-node-agent obtains SVID → uses on HSN
```

**Configuration:** Set `bind_network: hsn` in quorum and node-agent config (default). This resolves to the HSN interface at startup. In standalone mode without PACT, `bind_network: any` (default `0.0.0.0`) is acceptable.

**Failure isolation:** Management net down → PACT degraded, lattice unaffected. HSN down → lattice paused, PACT unaffected (admin access works). See `specs/failure-modes.md` for full matrix.

## Bootstrap Sequence

### Phase 1: Infrastructure (OpenCHAMI)

```
1. Deploy OpenCHAMI services:
   - Magellan (BMC discovery)
   - SMD (State Management Daemon)
   - BSS (Boot Script Service)
   - OPAAL (Authentication)
2. Discover nodes via Redfish BMC scan
3. Register node inventory in SMD
4. Prepare boot images:
   - Standard compute image (Linux + node agent)
   - Sensitive hardened image (minimal kernel, SELinux, no SSH)
5. Generate PKI:
   - Site root CA
   - Intermediate CA for OPAAL
   - Pre-provision quorum member certificates
```

### Phase 2: Control Plane

```
1. Deploy quorum members (3 or 5 nodes, dedicated hardware):
   a. Install lattice-quorum binary
   b. Configure Raft cluster membership
   c. Load TLS certificates (pre-provisioned)
   d. Initialize Raft cluster:
      - First member bootstraps as single-node cluster
      - Additional members join via Raft AddMember
   e. Verify: Raft leader elected, all members healthy

2. Deploy API servers (2+ for redundancy):
   a. Install lattice-api binary
   b. Configure quorum endpoints, TLS, OIDC provider
   c. Place behind load balancer
   d. Health check: /healthz returns 200

3. Deploy vCluster schedulers:
   a. One scheduler instance per vCluster type
   b. Configure cost function weights (from config file or quorum)
   c. Verify: scheduling cycle runs (empty, no nodes yet)

4. Deploy checkpoint broker:
   a. Install lattice-checkpoint binary
   b. Configure quorum and VAST API endpoints
```

### Phase 3: Compute Nodes

```
1. Configure BSS with standard compute image + cloud-init template:
   - cloud-init installs node agent binary
   - cloud-init generates TLS certificate via OPAAL
   - cloud-init configures quorum endpoint

2. Boot nodes (batch: groups of 50-100):
   - PXE boot → BSS serves image → cloud-init runs → node agent starts

3. Node agent startup:
   a. Generate TLS cert from OPAAL (if not pre-provisioned)
   b. Discover local hardware (GPUs via NVML/ROCm-SMI, NVMe if present, NIC)
   c. Compute conformance fingerprint
   d. Register with quorum (first heartbeat)
   e. Report capabilities and health

4. Quorum auto-discovers nodes from first heartbeat.
   No manual node registration required.

5. Verify: `lattice node list` shows all nodes in Ready state.
```

### Phase 4: Configuration

```
1. Create tenants:
   lattice admin tenant create --name="physics" --max-nodes=200

2. Create vClusters:
   lattice admin vcluster create --name="hpc-batch" \
     --scheduler=hpc-backfill \
     --tenant=physics \
     --nodes=x1000c0s0b0n[0-199]

3. Configure cost function weights (or use defaults):
   lattice admin vcluster set-weights --name="hpc-batch" \
     --priority=0.20 --wait-time=0.25 --fair-share=0.25 ...

4. (Optional) Configure Waldur accounting:
   lattice admin config set accounting.enabled=true
   lattice admin config set accounting.waldur.api_url="https://..."

5. (Optional) Configure federation:
   lattice admin federation add-peer --endpoint=... --workspace=...

6. Test: submit a test allocation.
```

## Quorum Initialization

### First-Time Bootstrap

The first quorum member initializes a new Raft cluster using the `--bootstrap`
flag. This flag must only be passed **once** — on the very first startup of
node 1. All subsequent restarts omit it; the persisted Raft state (WAL +
snapshots) is sufficient to rejoin.

```bash
# First-ever start of node 1:
lattice-server --config /etc/lattice/server.yaml --bootstrap

# All subsequent restarts (including systemd):
lattice-server --config /etc/lattice/server.yaml
```

This creates an empty Raft log and elects node 1 as leader.

### Adding Members

Subsequent members join the existing cluster:

```bash
# On the leader (or any member):
lattice-quorum membership add --node-id=quorum-2 --addr=quorum-2:4001

# On the new member:
lattice-quorum --join=quorum-1:4001 \
  --node-id=quorum-2 \
  --listen=0.0.0.0:4001 \
  --data-dir=/var/lib/lattice/raft
```

The new member syncs the Raft log from the leader and becomes a follower.

### Initial State

A freshly bootstrapped quorum has:
- Empty node registry (populated when nodes boot)
- Empty tenant/vCluster configuration (created by admin)
- Empty sensitive audit log
- Default system configuration

## Disaster Recovery

### Raft Snapshot + WAL Recovery

The quorum periodically snapshots its state and writes a WAL (Write-Ahead Log):

```
/var/lib/lattice/raft/
├── snapshots/
│   ├── snap-000100.bin     # Raft state at log index 100
│   └── snap-000200.bin     # Raft state at log index 200
├── wal/
│   ├── wal-000200-000300   # Log entries 200-300
│   └── wal-000300-000400   # Log entries 300-400
└── metadata.json           # Current term, voted_for, last_applied
```

**Backup:** Snapshots are replicated to S3 (configurable interval, default: hourly):
```
s3://lattice-backup/raft/snap-{timestamp}.bin
```

### Recovery Procedure

If all quorum members are lost:

```
1. Provision new quorum hardware (3 or 5 nodes)
2. Retrieve latest snapshot from S3:
   aws s3 cp s3://lattice-backup/raft/snap-latest.bin /var/lib/lattice/raft/

3. Bootstrap from snapshot:
   lattice-quorum --recover-from=/var/lib/lattice/raft/snap-latest.bin \
     --node-id=quorum-1 --bootstrap

4. Add remaining quorum members (join the recovered leader)

5. Node agents will reconnect automatically (they retry with backoff)

6. Verify state:
   lattice admin raft status
   lattice node list
```

**Data loss window:** From the last snapshot to the failure. With hourly snapshots, at most 1 hour of Raft commits could be lost. In practice, node ownership changes are infrequent (scheduling cycles), so data loss is minimal.

### Partial Quorum Loss

If a minority of quorum members fail (1 of 3, or 2 of 5):

1. The cluster continues operating (Raft majority maintained)
2. Replace failed members via Raft membership change:
   ```bash
   lattice-quorum membership remove --node-id=quorum-2
   lattice-quorum membership add --node-id=quorum-2-new --addr=...
   ```
3. New member syncs from leader automatically
4. No data loss, no downtime

### Non-Raft State Backup

The Raft snapshot captures quorum state (node ownership, tenants, sensitive audit). Other stateful components require separate backup strategies:

| Component | State Location | Backup Strategy |
|-----------|---------------|----------------|
| TSDB (metrics) | VictoriaMetrics / Thanos | TSDB-native snapshot + S3 replication |
| S3 logs | `s3://{tenant}/{project}/{alloc_id}/logs/` | S3 bucket versioning + cross-region replication |
| Accounting WAL | `/var/lib/lattice/accounting-wal` | Include in node backup or replicate to S3 |
| Sensitive audit log | Raft state (primary) + S3 archive (cold) | Covered by Raft snapshot; S3 archive has its own retention |
| Grafana dashboards | `infra/grafana/` (version-controlled) | Git repository |

**Recommended schedule:** Daily backup verification for TSDB snapshots. Accounting WAL backed up on the same schedule as Raft snapshots.

### Quorum Hardware Replacement

When a quorum member's hardware fails and must be replaced:

1. **Remove the failed member from the Raft cluster:**
   ```bash
   lattice-quorum membership remove --node-id=quorum-2
   ```
   The cluster continues operating with the remaining majority.

2. **Provision new hardware:**
   - Install the same OS and lattice-quorum binary
   - Generate a new TLS certificate from the site CA (same CN format)
   - Configure the same data directory path

3. **Add the new member to the cluster:**
   ```bash
   # On an existing member:
   lattice-quorum membership add --node-id=quorum-2-new --addr=new-host:4001

   # On the new hardware:
   lattice-quorum --join=quorum-1:4001 \
     --node-id=quorum-2-new \
     --listen=0.0.0.0:4001 \
     --data-dir=/var/lib/lattice/raft
   ```

4. **Verify:** The new member syncs the full Raft log from the leader. Check with `lattice admin raft status`.

5. **Cleanup:** Remove old member's data directory from failed hardware (if recoverable). Update monitoring/alerting to reference the new member.

**Important:** Replace one member at a time. Wait for the new member to fully sync before replacing another. For a 3-member quorum, never have more than 1 member down simultaneously.

## Configuration Management

All configuration is stored in two places:

| Configuration | Storage | Update Mechanism |
|---------------|---------|-----------------|
| Raft cluster membership | Raft log | Membership change commands |
| Tenant/vCluster definitions | Raft state machine | API calls (Raft-committed) |
| Cost function weights | Raft state machine | Hot-reloadable via API |
| Component config (listen addr, TLS paths) | Local config files | Restart required |
| Node agent config | cloud-init template | Reboot to apply changes |

Config files are version-controlled alongside deployment manifests. Changes to Raft-stored configuration are applied via API and take effect immediately.

## Capacity Planning

| Cluster Size | Quorum Members | API Servers | Scheduler Instances | Quorum Hardware |
|-------------|----------------|-------------|--------------------|--------------------|
| < 100 nodes | 3 | 2 | 1 per vCluster type | 4 CPU, 16 GB RAM, 100 GB SSD |
| 100-1000 nodes | 3 | 3 | 1 per vCluster type | 8 CPU, 32 GB RAM, 200 GB SSD |
| 1000-5000 nodes | 5 | 5 | 2 per vCluster type | 16 CPU, 64 GB RAM, 500 GB SSD |
| 5000+ nodes | 5 | 5+ (behind LB) | 2+ per vCluster type | 32 CPU, 128 GB RAM, 1 TB SSD |

**Quorum hardware notes:** Quorum members are latency-sensitive (Raft commits). Dedicated NVMe SSD for WAL. Not co-located with compute workloads. Prefer separate hardware or at minimum separate failure domains.

## Backup Verification

Snapshots replicated to S3 should be verified periodically to ensure they are restorable:

```bash
# Verify the latest snapshot is readable and consistent
lattice admin backup verify --source=s3://lattice-backup/raft/snap-latest.bin

# Verify a specific snapshot
lattice admin backup verify --source=s3://lattice-backup/raft/snap-20260301T120000.bin
```

Verification checks:
- Snapshot file integrity (checksum match)
- Raft metadata consistency (term, index, membership)
- Deserialization of state machine (all entries parseable)

**Recommended schedule:** Weekly automated verification via cron or CI pipeline. Alert on failure.

### Snapshot Retention Policy

Local snapshots are retained on quorum member disks:
- Keep the last 5 snapshots (default, configurable: `raft.snapshot_retention_count`)
- Older snapshots are deleted after a new snapshot is confirmed written

S3 snapshots follow a lifecycle policy:
- Keep all snapshots for 7 days (hourly granularity)
- After 7 days: keep one snapshot per day for 30 days
- After 30 days: keep one snapshot per week for 90 days
- After 90 days: delete (unless sensitive audit retention requires longer)

Configure via S3 lifecycle rules on the `lattice-backup` bucket.

## Component Log Management

Lattice components log to stdout/stderr by default, managed by the system's init system (systemd journald or equivalent).

**Recommended log rotation:**

| Component | Log Volume | Rotation |
|-----------|-----------|----------|
| Quorum members | Low (Raft events, membership changes) | journald default (rotate at 4 GB or 1 month) |
| API servers | Medium (request logs, access logs) | journald or file rotation (rotate at 1 GB, keep 7 files) |
| vCluster schedulers | Low-Medium (scheduling cycle logs) | journald default |
| Node agents | Low per-node (heartbeats, allocation lifecycle) | journald default |
| Checkpoint broker | Low (checkpoint decisions) | journald default |

For centralized log collection, configure journald to forward to a log aggregator (e.g., Loki, Elasticsearch) via `systemd-journal-remote` or a sidecar agent.

**Structured logging:** All components emit JSON-formatted logs with fields: `timestamp`, `level`, `component`, `message`, and context-specific fields (e.g., `allocation_id`, `node_id`).

## Test/Dev Deployment (GCP)

For integration testing without bare metal, use the GCP test infrastructure:

```
infra/gcp/
├── terraform/main.tf           # 3 quorum + 2 compute + registry + TSDB
├── packer/lattice-compute.pkr.hcl  # Pre-baked image with podman + squashfs-tools
scripts/deploy/
├── make-provision-bundle.sh    # Single tarball: binaries + scripts + systemd units
├── install-quorum.sh           # Reusable, no GCP-specific logic
├── install-compute.sh          # Reusable, HMAC token generation
└── validate.sh                 # Structured test runner (15 tests)
```

Workflow:
1. `packer build` — create compute image (once)
2. `terraform apply` — provision VMs
3. `make-provision-bundle.sh` — package release
4. SCP bundle to nodes, `install-quorum.sh` (node 1 with `--bootstrap`), `install-compute.sh`
5. `validate.sh` — run test matrix
6. `terraform destroy` — manual cleanup

The deploy scripts are reusable on-prem — no GCP-specific logic in `install-*.sh`.

## Cross-References

- [system-architecture.md](system-architecture.md) — Seven-layer architecture overview
- [security.md](security.md) — PKI, mTLS, certificate provisioning
- [upgrades.md](upgrades.md) — Rolling upgrade procedure (after initial deployment)
- [failure-modes.md](failure-modes.md) — Component failure and recovery
- [node-lifecycle.md](node-lifecycle.md) — Node boot and registration

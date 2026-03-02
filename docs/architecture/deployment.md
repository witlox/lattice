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
   - Medical hardened image (minimal kernel, SELinux, no SSH)
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

The first quorum member initializes a single-node Raft cluster:

```bash
lattice-quorum --bootstrap \
  --node-id=quorum-1 \
  --listen=0.0.0.0:4001 \
  --data-dir=/var/lib/lattice/raft
```

This creates an empty Raft log and elects itself as leader.

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
- Empty medical audit log
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

## Cross-References

- [system-architecture.md](system-architecture.md) — Seven-layer architecture overview
- [security.md](security.md) — PKI, mTLS, certificate provisioning
- [upgrades.md](upgrades.md) — Rolling upgrade procedure (after initial deployment)
- [failure-modes.md](failure-modes.md) — Component failure and recovery
- [node-lifecycle.md](node-lifecycle.md) — Node boot and registration

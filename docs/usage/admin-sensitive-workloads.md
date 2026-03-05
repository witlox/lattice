# Managing Sensitive Workloads

Sensitive workloads (financial, defense, regulated research) require strict isolation, auditing, and data handling. Lattice provides a dedicated scheduling mode for these workloads.

## How It Works

1. **User claims nodes** — not the scheduler. The user's identity is recorded as the owner in the Raft audit log.
2. **Full isolation** — claimed nodes run only the owner's workloads. No sharing.
3. **Hardened OS** — OpenCHAMI provisions a hardened boot image for claimed nodes.
4. **Encrypted storage** — a dedicated encrypted pool is assigned. All access is logged.
5. **Signed software only** — only vulnerability-scanned, signed uenv images are allowed.
6. **Wipe on release** — when the claim ends, storage is crypto-erased and nodes are re-provisioned.

## Submitting Sensitive Workloads

```bash
# Submit to the sensitive vCluster
lattice submit --vcluster=sensitive --nodes=4 --walltime=168h analysis.sh
```

The sensitive scheduler uses a reservation model (not backfill). Priority is fixed at the highest level; the only tiebreaker is conformance fitness.

## Node Claiming

Sensitive allocations claim specific nodes. Once claimed:
- Nodes are exclusively owned by the claiming user
- The claim is Raft-committed with the user's identity
- No other workloads (even from the same tenant) can run on claimed nodes

## Audit Trail

Every sensitive operation is logged:

```bash
# Query sensitive audit entries
curl "http://lattice-01:8080/api/v1/audit?scope=sensitive"
```

Logged events:
- Node claim / release
- Allocation start / completion
- Data access (read/write operations)
- Software image loads
- Storage wipe confirmation

Retention: 7 years (per regulatory requirements).

## Network Isolation

Sensitive allocations get a unique Slingshot VNI (network domain). Ingress and egress are denied except to the designated data gateway. With Ultra Ethernet, wire-level encryption is enabled.

## Admin Responsibilities

- **Provision hardened images** via OpenCHAMI for sensitive nodes
- **Maintain signed uenv registry** — only approved images should be signed
- **Monitor audit log** — set up alerting for unexpected access patterns
- **Test wipe procedures** — verify crypto-erase completes on node release
- **Designate sensitive-capable nodes** — not all nodes need to support sensitive workloads

## Configuration

No special server configuration is needed. The sensitive scheduler is a built-in vCluster type. Create a sensitive vCluster:

```bash
lattice admin vcluster create \
  --name=sensitive \
  --scheduler-type=sensitive-reservation \
  --description="Regulated workloads with full isolation"
```

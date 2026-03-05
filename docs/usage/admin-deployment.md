# Deployment & Administration

## Architecture Overview

A Lattice deployment consists of:

- **3-5 quorum members** — Raft consensus nodes running `lattice-server`
- **N compute nodes** — each running `lattice-agent`
- **VictoriaMetrics** (or compatible TSDB) — telemetry storage
- **S3-compatible storage** — checkpoint and log persistence
- **VAST** (optional) — data staging and QoS

## Deployment Methods

### Docker Compose (dev/test)

```bash
cd infra/docker
docker compose up -d
```

This starts a 3-node quorum with VictoriaMetrics. See `infra/docker/docker-compose.yml`.

### Systemd (production)

Install the binaries and unit files:

```bash
# Server (quorum members)
cp target/release/lattice-server /usr/local/bin/
cp infra/systemd/lattice-server.service /etc/systemd/system/
cp config/production.yaml /etc/lattice/config.yaml
systemctl enable --now lattice-server

# Agent (compute nodes)
cp target/release/lattice-agent /usr/local/bin/
cp infra/systemd/lattice-agent.service /etc/systemd/system/
systemctl enable --now lattice-agent
```

## Configuration

Example configs are in `config/`:

| File | Purpose |
|------|---------|
| `config/minimal.yaml` | Single-node dev mode, no optional features |
| `config/production.yaml` | Full reference with all sections documented |

See the [production config](https://github.com/witlox/lattice/blob/main/config/production.yaml) for every option with explanations.

### Required Sections

- **`quorum`** — Raft node ID, peers, data directory
- **`api`** — gRPC and REST listen addresses
- **`storage`** — S3 endpoint, NFS paths
- **`telemetry`** — TSDB endpoint, aggregation mode

### Optional Sections

- **`node_agent`** — heartbeat timing, grace periods
- **`network`** — VNI pool range for Slingshot
- **`checkpoint`** — checkpoint evaluation and timeout tuning
- **`scheduling`** — cycle interval, backfill depth
- **`accounting`** — Waldur integration (requires `accounting` feature)
- **`rate_limit`** — per-user API rate limiting
- **`federation`** — Sovra cross-site federation (requires `federation` feature)
- **`compat`** — Slurm compatibility settings

## Quorum Management

### Initial Bootstrap

The first quorum member starts as a single-node cluster. Add peers by listing them in each node's config:

```yaml
quorum:
  node_id: 1
  data_dir: /var/lib/lattice/raft
  peers:
    - id: 2
      address: "lattice-02:9000"
    - id: 3
      address: "lattice-03:9000"
```

### Raft Status

```bash
curl http://lattice-01:8080/api/v1/raft/status
```

### Backup & Restore

```bash
# Create backup
curl -X POST http://lattice-01:8080/api/v1/admin/backup

# Verify backup integrity
curl http://lattice-01:8080/api/v1/admin/backup/verify

# Restore (requires restart)
curl -X POST http://lattice-01:8080/api/v1/admin/restore \
  -d '{"path": "/var/lib/lattice/backups/backup-20260305T120000Z.tar.gz"}'
```

## Node Management

### Agent Registration

Agents register automatically on startup:

```bash
lattice-agent \
  --node-id=nid001234 \
  --quorum-endpoint=http://lattice-01:50051 \
  --gpu-count=4 \
  --gpu-type=GH200 \
  --cpu-cores=72 \
  --memory-gb=512
```

### Draining Nodes

```bash
# Drain a node (existing jobs complete, no new jobs scheduled)
lattice admin drain nid001234 --reason="maintenance"

# Undrain
lattice admin undrain nid001234
```

### Node States

| State | Meaning |
|-------|---------|
| **Ready** | Available for scheduling |
| **Draining** | No new jobs; existing jobs continue |
| **Down** | Heartbeat lost beyond grace period |
| **Degraded** | Heartbeat late but within grace period |
| **Claimed** | Reserved for sensitive workload |

## Tenant Management

```bash
# Create a tenant
lattice admin tenant create --name="physics" --max-nodes=100

# List tenants
lattice admin tenant list

# Update quota
lattice admin tenant update physics --max-nodes=200
```

## TLS Configuration

### Server TLS

```yaml
api:
  tls_cert: /etc/lattice/tls/server.crt
  tls_key: /etc/lattice/tls/server.key
```

### Mutual TLS (mTLS)

```yaml
api:
  tls_cert: /etc/lattice/tls/server.crt
  tls_key: /etc/lattice/tls/server.key
  tls_ca: /etc/lattice/tls/ca.crt  # Require client certificates
```

## Feature Flags

Compile-time features control optional integrations:

| Feature | Enables |
|---------|---------|
| `oidc` | JWT/OIDC token validation |
| `accounting` | Waldur billing integration |
| `federation` | Sovra cross-site federation |
| `nvidia` | NVIDIA GPU discovery (nvml) |
| `ebpf` | eBPF kernel telemetry (Linux only) |

Build with features:

```bash
cargo build --release --features oidc,accounting,nvidia
```

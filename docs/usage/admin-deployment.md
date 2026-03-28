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

Download binaries from [GitHub Releases](https://github.com/witlox/lattice/releases/latest) and install:

```bash
ARCH=$(uname -m | sed 's/aarch64/arm64/')

# Server (quorum members)
curl -sSfL "https://github.com/witlox/lattice/releases/latest/download/lattice-server-${ARCH}.tar.gz" | tar xz
sudo mv lattice-server /usr/local/bin/
sudo cp infra/systemd/lattice-server.service /etc/systemd/system/
sudo cp config/production.yaml /etc/lattice/config.yaml
sudo systemctl enable --now lattice-server

# Agent (compute nodes) — single binary per architecture, all GPU support included
curl -sSfL "https://github.com/witlox/lattice/releases/latest/download/lattice-agent-${ARCH}.tar.gz" | tar xz
sudo mv lattice-agent /usr/local/bin/
sudo cp infra/systemd/lattice-agent.service /etc/systemd/system/
sudo systemctl enable --now lattice-agent
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

The first quorum member initializes the Raft cluster using the `--bootstrap` flag. This flag must only be passed **once** — on the very first startup of node 1. All subsequent restarts (including systemd restarts) omit it.

```bash
# First-ever start of node 1 — initializes the Raft cluster:
lattice-server --config /etc/lattice/server.yaml --bootstrap

# All subsequent restarts — no --bootstrap:
lattice-server --config /etc/lattice/server.yaml
# (or via systemd, which never passes --bootstrap)
```

Configure peers in each node's config:

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

Nodes 2 and 3 never need `--bootstrap` — they join via Raft membership replication from the leader.

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

The drain lifecycle is: **Ready → Draining → Drained → Ready**.

```bash
# Drain a node (existing jobs complete, no new jobs scheduled)
lattice admin drain nid001234 --reason="maintenance"

# If no active allocations, node goes directly to Drained.
# If allocations are running, node stays in Draining until they complete.
# The scheduler loop automatically transitions Draining → Drained.

# Undrain (only works from Drained state)
lattice admin undrain nid001234
```

Undrain only works when the node is in `Drained` state. If the node is still `Draining` (allocations running), wait for them to complete or cancel them first.

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

| Feature | Crate | Enables |
|---------|-------|---------|
| `oidc` | lattice-api | JWT/OIDC token validation |
| `accounting` | lattice-api | Waldur billing integration |
| `federation` | lattice-api | Sovra cross-site federation |
| `nvidia` | lattice-node-agent | NVIDIA GPU discovery (nvml-wrapper) |
| `rocm` | lattice-node-agent | AMD GPU discovery (rocm-smi) |
| `ebpf` | lattice-node-agent | eBPF kernel telemetry (Linux only) |

Pre-built release binaries ship with **all features enabled**. GPU libraries are loaded at runtime — nodes without GPUs simply report no GPU hardware. To build from source:

```bash
# Server with all features
cargo build --release -p lattice-api --all-features

# Agent with all features
cargo build --release -p lattice-node-agent --all-features
```

### Release Artifacts

| Artifact | Architecture | GPU Support |
|----------|-------------|-------------|
| `lattice-server-x86_64.tar.gz` | x86_64 | n/a |
| `lattice-server-arm64.tar.gz` | arm64 | n/a |
| `lattice-x86_64.tar.gz` | x86_64 | n/a (CLI) |
| `lattice-arm64.tar.gz` | arm64 | n/a (CLI) |
| `lattice-agent-x86_64.tar.gz` | x86_64 | NVIDIA + AMD ROCm + eBPF |
| `lattice-agent-arm64.tar.gz` | arm64 | NVIDIA + AMD ROCm + eBPF |
| `rm-replay-x86_64.tar.gz` | x86_64 | n/a |
| `rm-replay-arm64.tar.gz` | arm64 | n/a |

GPU discovery is automatic at runtime. The agent detects available hardware and uses the appropriate provider:

| Hardware | Discovery Method | Runtime Dependency |
|----------|-----------------|-------------------|
| NVIDIA (H100, A100, GH200) | nvml-wrapper (`libnvidia-ml.so` via dlopen) | NVIDIA driver installed |
| AMD (MI300X, MI250) | rocm-smi CLI | ROCm toolkit installed |
| CPU-only nodes | No GPU discovery runs | None |

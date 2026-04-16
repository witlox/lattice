#!/usr/bin/env bash
# Deploy lattice binaries to GCP test cluster.
#
# Prerequisites:
#   - terraform apply completed (infra/gcp/)
#   - lattice-server and lattice-agent binaries built for linux-amd64
#   - gcloud configured with SSH access
#
# Usage:
#   ./scripts/gcp-deploy.sh [--build]  # --build cross-compiles first
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BIN_DIR="$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release"

# SSH identity for the lattice VM user. Override with LATTICE_SSH_KEY if your
# terraform apply used a different public key.
LATTICE_SSH_KEY="${LATTICE_SSH_KEY:-$HOME/.ssh/google_compute_engine}"
SSH_OPTS=(-i "$LATTICE_SSH_KEY" -o StrictHostKeyChecking=no -o IdentitiesOnly=yes)

# Long-lived HMAC bearer token for node agents to authenticate
# against the quorum (server.yaml's oidc_hmac_secret). Generated
# once per deploy and stored in each compute VM's /etc/lattice/agent.env.
HMAC_SECRET="${LATTICE_OIDC_HMAC_SECRET:-lattice-test-hmac-secret}"
AGENT_BEARER_TOKEN="$(python3 - <<PYEOF
import json, base64, hmac, hashlib, time
h = base64.urlsafe_b64encode(json.dumps({'alg':'HS256','typ':'JWT'}).encode()).rstrip(b'=').decode()
p = base64.urlsafe_b64encode(json.dumps({
    'sub':'node-agent',
    'exp':int(time.time())+86400*365,
    'scope':'operator',
}).encode()).rstrip(b'=').decode()
m = f"{h}.{p}"
s = base64.urlsafe_b64encode(hmac.new(b"${HMAC_SECRET}", m.encode(), hashlib.sha256).digest()).rstrip(b'=').decode()
print(f"{m}.{s}")
PYEOF
)"

# ─── Build (optional) ────────────────────────────────────────

if [[ "${1:-}" == "--build" ]]; then
  echo "=== Cross-compiling for linux-amd64 ==="
  cargo build --release --target x86_64-unknown-linux-gnu \
    -p lattice-api -p lattice-node-agent -p lattice-cli
fi

# ─── Read terraform outputs ──────────────────────────────────

cd "$PROJECT_DIR/infra/gcp"
QUORUM_IPS=($(terraform output -json quorum_ips | jq -r '.[]'))
COMPUTE_IPS=($(terraform output -json compute_ips | jq -r '.[]'))
REGISTRY_IP=$(terraform output -raw registry_ip)
TSDB_IP=$(terraform output -raw tsdb_ip)

echo "Quorum:  ${QUORUM_IPS[*]}"
echo "Compute: ${COMPUTE_IPS[*]}"
echo "Registry: $REGISTRY_IP"
echo "TSDB:     $TSDB_IP"
cd "$PROJECT_DIR"

# ─── Deploy binaries ─────────────────────────────────────────

deploy_binary() {
  local ip=$1 binary=$2 dest=$3
  echo "  Deploying $binary → $ip:$dest"
  scp "${SSH_OPTS[@]}" "$BIN_DIR/$binary" "lattice@$ip:/tmp/$binary"
  ssh "${SSH_OPTS[@]}" "lattice@$ip" "sudo mv /tmp/$binary $dest && sudo chmod +x $dest"
}

echo "=== Deploying lattice-server to quorum nodes ==="
for ip in "${QUORUM_IPS[@]}"; do
  deploy_binary "$ip" lattice-server /usr/local/bin/lattice-server
done

echo "=== Deploying lattice-agent to compute nodes ==="
for ip in "${COMPUTE_IPS[@]}"; do
  deploy_binary "$ip" lattice-agent /usr/local/bin/lattice-agent
done

echo "=== Deploying lattice CLI to quorum-1 (for testing) ==="
deploy_binary "${QUORUM_IPS[0]}" lattice /usr/local/bin/lattice

# ─── Fix ownership on data dirs ──────────────────────────────
# setup-quorum.sh / setup-compute.sh create /var/lib/lattice and
# /etc/lattice as root — the lattice service user can't write to
# them without this chown. Creating a `lattice` group idempotently
# covers the case where useradd didn't auto-create one.

echo "=== Fixing data-dir ownership on quorum nodes ==="
for ip in "${QUORUM_IPS[@]}"; do
  ssh "${SSH_OPTS[@]}" "lattice@$ip" \
    "sudo groupadd -r lattice 2>/dev/null || true;
     sudo usermod -g lattice lattice 2>/dev/null || true;
     sudo mkdir -p /var/lib/lattice/raft /etc/lattice;
     sudo chown -R lattice:lattice /var/lib/lattice /etc/lattice"
done

echo "=== Fixing data-dir ownership on compute nodes ==="
for ip in "${COMPUTE_IPS[@]}"; do
  ssh "${SSH_OPTS[@]}" "lattice@$ip" \
    "sudo groupadd -r lattice 2>/dev/null || true;
     sudo useradd -r -g lattice -d /var/lib/lattice -s /bin/bash lattice 2>/dev/null || true;
     sudo mkdir -p /var/lib/lattice /etc/lattice /scratch;
     sudo chown -R lattice:lattice /var/lib/lattice /etc/lattice /scratch"
done

# ─── Recover from failed startup-script on compute nodes ─────
# The Ubuntu 24.04 startup-script in setup-compute.sh apt-installed
# `nsenter` (which doesn't exist as a package — it's part of
# util-linux and preinstalled). The failure aborted the script
# before it wrote the systemd unit. Reapply the essentials here so
# a re-deploy after a fresh `terraform apply` still gets to healthy.

QUORUM_INTERNAL_IP="10.0.0.10"  # quorum-1, matches main.tf network_ip

echo "=== Ensuring compute node runtime deps + systemd unit ==="
for i in "${!COMPUTE_IPS[@]}"; do
  ip="${COMPUTE_IPS[$i]}"
  node_id="x1000c0s0b0n${i}"
  ssh "${SSH_OPTS[@]}" "lattice@$ip" "sudo bash -s" <<COMPUTE_FIX
set -euo pipefail
# Install what setup-compute.sh should have installed.
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq podman squashfs-tools squashfuse fuse3 util-linux curl jq
mkdir -p /var/lib/lattice /var/cache/lattice/uenv /etc/lattice /scratch
chown -R lattice:lattice /var/lib/lattice /var/cache/lattice /etc/lattice /scratch

# Resolve node-local facts at setup time (systemd ExecStart doesn't
# shell-expand \$(cmd) — it takes the string literally).
INTERNAL_IP=\$(hostname -I | awk '{print \$1}')
CPU_CORES=\$(nproc)
MEMORY_GB=\$(free -g | awk '/Mem:/{print \$2}')
cat > /etc/lattice/agent.env <<EOF
RUST_LOG=info,lattice=debug
LATTICE_QUORUM_ENDPOINT=http://${QUORUM_INTERNAL_IP}:50051
LATTICE_STATE_FILE=/var/lib/lattice/agent-state.json
LATTICE_ADVERTISE_ADDR=\${INTERNAL_IP}:50052
LATTICE_AGENT_TOKEN=${AGENT_BEARER_TOKEN}
EOF

# systemd unit.
cat > /etc/systemd/system/lattice-agent.service <<EOF
[Unit]
Description=Lattice Node Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/lattice-agent --node-id ${node_id} --quorum-endpoint http://${QUORUM_INTERNAL_IP}:50051 --cpu-cores \${CPU_CORES} --memory-gb \${MEMORY_GB}
EnvironmentFile=-/etc/lattice/agent.env
Restart=on-failure
RestartSec=5
KillMode=process
User=root
Group=root
LimitNOFILE=65536
AmbientCapabilities=CAP_SYS_ADMIN CAP_SYS_PTRACE CAP_NET_ADMIN

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
COMPUTE_FIX
done

# ─── Stop any in-flight services before bootstrap ────────────
# Re-running the deploy after a partial failure leaves stale
# lattice-server / lattice-agent processes holding the Raft (9000)
# and gRPC/REST (8080/50051) ports. Stop them cleanly first so
# --bootstrap can bind. `|| true` so the first run (no units yet)
# doesn't fail.

echo "=== Stopping any stale services ==="
for ip in "${QUORUM_IPS[@]}"; do
  ssh "${SSH_OPTS[@]}" "lattice@$ip" \
    "sudo systemctl stop lattice-server 2>/dev/null || true;
     sudo pkill -9 -f /usr/local/bin/lattice-server 2>/dev/null || true" || true
done

# Wipe any partial Raft state from previous failed bootstraps. With
# ${WIPE_RAFT_STATE:-1} set to 0, the existing state is preserved — set
# to 0 when re-running the deploy to push a new binary without resetting
# the cluster.
if [[ "${WIPE_RAFT_STATE:-1}" == "1" ]]; then
  echo "=== Wiping Raft state (set WIPE_RAFT_STATE=0 to preserve) ==="
  for ip in "${QUORUM_IPS[@]}"; do
    ssh "${SSH_OPTS[@]}" "lattice@$ip" \
      "sudo rm -rf /var/lib/lattice/raft/*;
       sudo mkdir -p /var/lib/lattice/raft;
       sudo chown -R lattice:lattice /var/lib/lattice/raft"
  done
fi
for ip in "${COMPUTE_IPS[@]}"; do
  ssh "${SSH_OPTS[@]}" "lattice@$ip" \
    "sudo systemctl stop lattice-agent 2>/dev/null || true;
     sudo pkill -9 -f /usr/local/bin/lattice-agent 2>/dev/null || true" || true
done

# ─── Bootstrap quorum (ordered start) ────────────────────────
# openraft's initialize writes the membership log entry and needs a
# quorum to commit it — which means quorum-2 and quorum-3 have to be
# reachable BEFORE quorum-1 runs --bootstrap. Otherwise the leader
# election loops forever trying to replicate to unreachable peers.
#
# Sequence:
#   1. Start lattice-server on quorum-2 and quorum-3 (they'll sit as
#      uninitialized learners waiting for membership).
#   2. Run --bootstrap foregrounded on quorum-1 for ~20s — long
#      enough to form quorum and commit the initial config.
#   3. Start lattice-server via systemd on quorum-1 so it continues
#      running after bootstrap exits.

echo "=== Starting quorum-2 and quorum-3 (pre-bootstrap) ==="
for ip in "${QUORUM_IPS[@]:1}"; do
  ssh "${SSH_OPTS[@]}" "lattice@$ip" "sudo systemctl start lattice-server"
done
sleep 5

echo "=== Bootstrapping Raft cluster on quorum-1 ==="
# Bootstrap runs foregrounded for 20s to form quorum. The exit code
# of a timeout-killed `lattice-server` can vary (124/137/143 or
# propagated), and ssh itself may add its own code. Disable set -e
# around this one command so the script keeps going regardless —
# success is measured by the healthz check a few steps down.
set +e
ssh "${SSH_OPTS[@]}" "lattice@${QUORUM_IPS[0]}" \
  "sudo -u lattice timeout 20 \
     /usr/local/bin/lattice-server --config /etc/lattice/server.yaml --bootstrap; true"
set -e

echo "=== Starting quorum-1 (post-bootstrap) ==="
ssh "${SSH_OPTS[@]}" "lattice@${QUORUM_IPS[0]}" "sudo systemctl start lattice-server"

echo "=== Waiting for quorum health ==="
sleep 5
for i in {1..30}; do
  if curl -sf "http://${QUORUM_IPS[0]}:8080/healthz" > /dev/null 2>&1; then
    echo "  Quorum healthy after ${i}s"
    break
  fi
  sleep 1
done

echo "=== Starting compute agents ==="
for ip in "${COMPUTE_IPS[@]}"; do
  ssh "${SSH_OPTS[@]}" "lattice@$ip" "sudo systemctl start lattice-agent"
done

sleep 3
echo "=== Cluster deployed ==="
echo "API: http://${QUORUM_IPS[0]}:8080"
echo "gRPC: ${QUORUM_IPS[0]}:50051"
curl -s "http://${QUORUM_IPS[0]}:8080/healthz" && echo " (healthy)"
echo ""
echo "Run tests: ./scripts/gcp-test.sh"

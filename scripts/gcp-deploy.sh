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
  scp -o StrictHostKeyChecking=no "$BIN_DIR/$binary" "lattice@$ip:/tmp/$binary"
  ssh -o StrictHostKeyChecking=no "lattice@$ip" "sudo mv /tmp/$binary $dest && sudo chmod +x $dest"
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

# ─── Bootstrap quorum ────────────────────────────────────────

echo "=== Bootstrapping Raft cluster on quorum-1 ==="
ssh -o StrictHostKeyChecking=no "lattice@${QUORUM_IPS[0]}" \
  "sudo -u lattice lattice-server --config /etc/lattice/server.yaml --bootstrap &
   sleep 3; kill %1 2>/dev/null; true"

echo "=== Starting quorum services ==="
for ip in "${QUORUM_IPS[@]}"; do
  ssh -o StrictHostKeyChecking=no "lattice@$ip" "sudo systemctl start lattice-server"
done

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
  ssh -o StrictHostKeyChecking=no "lattice@$ip" "sudo systemctl start lattice-agent"
done

sleep 3
echo "=== Cluster deployed ==="
echo "API: http://${QUORUM_IPS[0]}:8080"
echo "gRPC: ${QUORUM_IPS[0]}:50051"
curl -s "http://${QUORUM_IPS[0]}:8080/healthz" && echo " (healthy)"
echo ""
echo "Run tests: ./scripts/gcp-test.sh"

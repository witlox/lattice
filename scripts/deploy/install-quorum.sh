#!/usr/bin/env bash
# install-quorum.sh — Install lattice-server on a quorum node.
#
# Reusable on-prem. No GCP-specific logic.
#
# Usage: ./install-quorum.sh <node-id> <listen-addr> <raft-peers> [--bootstrap]
#   node-id:     Raft node ID (1, 2, or 3)
#   listen-addr: This node's IP or hostname
#   raft-peers:  Comma-separated "id=addr" pairs (e.g., "1=mgmt-1:9000,2=mgmt-2:9000,3=mgmt-3:9000")
#   --bootstrap: Initialize Raft cluster (only on node 1, only on first start)
#
# Expects binaries at /opt/lattice/bin/ (placed by provision bundle or Packer).

set -euo pipefail

NODE_ID="${1:?Usage: install-quorum.sh <node-id> <listen-addr> <raft-peers> [--bootstrap]}"
LISTEN_ADDR="${2:?}"
RAFT_PEERS="${3:?}"
shift 3

BOOTSTRAP=false
for arg in "$@"; do
    case "$arg" in
        --bootstrap) BOOTSTRAP=true ;;
    esac
done

BIN_DIR="/opt/lattice/bin"
CONF_DIR="/etc/lattice"
DATA_DIR="/var/lib/lattice/raft"

echo "=== Installing lattice-server (node $NODE_ID) ==="

# Create directories
mkdir -p "$CONF_DIR" "$DATA_DIR"

# Create lattice user if it doesn't exist
id -u lattice &>/dev/null || useradd --system --no-create-home --shell /usr/sbin/nologin lattice

# Symlink binaries
for bin in lattice-server lattice; do
    [ -f "$BIN_DIR/$bin" ] && ln -sf "$BIN_DIR/$bin" "/usr/local/bin/$bin"
done

# Parse peers into YAML
PEER_YAML=""
IFS=',' read -ra PEER_LIST <<< "$RAFT_PEERS"
for peer in "${PEER_LIST[@]}"; do
    PEER_ID="${peer%%=*}"
    PEER_ADDR="${peer#*=}"
    if [ "$PEER_ID" != "$NODE_ID" ]; then
        PEER_YAML="$PEER_YAML
    - id: $PEER_ID
      address: \"$PEER_ADDR\""
    fi
done

# Write config
cat > "$CONF_DIR/server.yaml" <<EOF
role: QuorumMember
quorum:
  node_id: $NODE_ID
  raft_listen_address: "0.0.0.0:9000"
  data_dir: $DATA_DIR
  election_timeout_ms: 500
  heartbeat_interval_ms: 200
  snapshot_threshold: 10000
  peers: $PEER_YAML
api:
  grpc_address: "0.0.0.0:50051"
  rest_address: "0.0.0.0:8080"
  oidc_issuer: ""
storage:
  s3_endpoint: ""
  nfs_home_path: "/home"
  local_scratch_path: "/scratch"
telemetry:
  default_mode: "prod"
  tsdb_endpoint: ""
  prod_interval_seconds: 30
EOF

# Write env file
cat > "$CONF_DIR/server.env" <<EOF
RUST_LOG=info,lattice=debug
LATTICE_OIDC_HMAC_SECRET=${LATTICE_HMAC_SECRET:-lattice-test-hmac-secret}
EOF

# Install systemd unit
if [ -f /opt/lattice/systemd/lattice-server.service ]; then
    cp /opt/lattice/systemd/lattice-server.service /etc/systemd/system/
fi

# Set ownership
chown -R lattice:lattice "$DATA_DIR"

# Bootstrap Raft cluster if requested (only on node 1, first start)
if [ "$BOOTSTRAP" = true ]; then
    echo "Bootstrapping Raft cluster..."
    sudo -u lattice "$BIN_DIR/lattice-server" \
        --config "$CONF_DIR/server.yaml" \
        --bootstrap &
    BOOTSTRAP_PID=$!
    sleep 3

    if kill -0 "$BOOTSTRAP_PID" 2>/dev/null; then
        echo "Bootstrap succeeded — stopping bootstrap instance"
        kill "$BOOTSTRAP_PID"
        wait "$BOOTSTRAP_PID" 2>/dev/null || true
    else
        echo "WARNING: Bootstrap process exited early — check logs"
    fi
fi

# Enable and start
systemctl daemon-reload
systemctl enable lattice-server
systemctl start lattice-server

echo "lattice-server started (node $NODE_ID, listen $LISTEN_ADDR)"

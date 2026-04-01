#!/bin/bash
# Setup a lattice quorum node.
# Template vars: node_id, node_count, peer_ips, lattice_version
set -euo pipefail

NODE_ID=${node_id}
PEER_IPS="${peer_ips}"
VERSION="${lattice_version}"

echo "=== Setting up lattice quorum node $NODE_ID ==="

# Install dependencies
apt-get update -qq
apt-get install -y -qq curl jq

# Create lattice user
useradd -r -m -d /var/lib/lattice -s /bin/bash lattice || true
mkdir -p /var/lib/lattice/raft /etc/lattice

# Download lattice-server binary
# In production: download from GitHub Releases
# For now: placeholder — binary must be uploaded separately
cat > /usr/local/bin/lattice-server-placeholder <<'SCRIPT'
#!/bin/bash
echo "lattice-server not yet installed — upload binary to /usr/local/bin/lattice-server"
exit 1
SCRIPT
chmod +x /usr/local/bin/lattice-server-placeholder

# Generate config
IFS=',' read -ra PEERS <<< "$PEER_IPS"
PEER_CONFIG=""
for i in $(seq 0 $((${node_count} - 1))); do
  PEER_ID=$((i + 1))
  if [ "$PEER_ID" != "$NODE_ID" ]; then
    PEER_CONFIG="$PEER_CONFIG
    - id: $PEER_ID
      address: \"$${PEERS[$i]}:9000\""
  fi
done

cat > /etc/lattice/server.yaml <<EOF
role: QuorumMember
quorum:
  node_id: $NODE_ID
  raft_listen_address: "0.0.0.0:9000"
  data_dir: /var/lib/lattice/raft
  election_timeout_ms: 500
  heartbeat_interval_ms: 200
  snapshot_threshold: 10000
  peers: $PEER_CONFIG
api:
  grpc_address: "0.0.0.0:50051"
  rest_address: "0.0.0.0:8080"
  oidc_issuer: ""
  oidc_hmac_secret: "lattice-test-hmac-secret"
storage:
  s3_endpoint: ""
  nfs_home_path: "/home"
  local_scratch_path: "/scratch"
telemetry:
  default_mode: "prod"
  tsdb_endpoint: ""
  prod_interval_seconds: 30
EOF

# Generate HMAC token for agents
cat > /etc/lattice/server.env <<EOF
RUST_LOG=info,lattice=debug
LATTICE_OIDC_HMAC_SECRET=lattice-test-hmac-secret
EOF

# Install systemd service
cat > /etc/systemd/system/lattice-server.service <<EOF
[Unit]
Description=Lattice Scheduler Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStartPre=+/bin/bash -c 'mkdir -p /var/lib/lattice/raft'
ExecStart=/usr/local/bin/lattice-server --config /etc/lattice/server.yaml
EnvironmentFile=-/etc/lattice/server.env
Restart=on-failure
RestartSec=5
User=lattice
Group=lattice
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload

echo "=== Quorum node $NODE_ID setup complete ==="
echo "Upload lattice-server binary to /usr/local/bin/lattice-server"
echo "Then: systemctl start lattice-server"
echo "First node only: lattice-server --config /etc/lattice/server.yaml --bootstrap"

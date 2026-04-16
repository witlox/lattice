#!/bin/bash
# Setup a lattice compute node (node agent).
# Template vars: quorum_ip, node_index, lattice_version
set -euo pipefail

QUORUM_IP="${quorum_ip}"
NODE_INDEX=${node_index}
NODE_ID="x1000c0s0b0n$NODE_INDEX"
VERSION="${lattice_version}"

echo "=== Setting up lattice compute node $NODE_ID ==="

# Install runtime dependencies
apt-get update -qq
apt-get install -y -qq \
  podman \
  squashfs-tools \
  squashfuse \
  fuse3 \
  util-linux \
  curl \
  jq

# Create directories
mkdir -p /var/lib/lattice /var/cache/lattice/uenv /etc/lattice /scratch

# Generate agent env
cat > /etc/lattice/agent.env <<EOF
RUST_LOG=info,lattice=debug
LATTICE_QUORUM_ENDPOINT=http://$QUORUM_IP:50051
LATTICE_AGENT_TOKEN=\$(python3 -c "
import json, base64, hmac, hashlib, time
header = base64.urlsafe_b64encode(json.dumps({'alg':'HS256','typ':'JWT'}).encode()).rstrip(b'=').decode()
payload = base64.urlsafe_b64encode(json.dumps({'sub':'node-agent-$NODE_ID','exp':int(time.time())+86400*365,'scope':'operator'}).encode()).rstrip(b'=').decode()
msg = f'{header}.{payload}'
sig = base64.urlsafe_b64encode(hmac.new(b'lattice-test-hmac-secret', msg.encode(), hashlib.sha256).digest()).rstrip(b'=').decode()
print(f'{msg}.{sig}')
")
LATTICE_STATE_FILE=/var/lib/lattice/agent-state.json
EOF

# INV-D1: advertise the node's internal VPC IP so the Dispatcher can
# call NodeAgentService.RunAllocation. The default grpc_addr is
# 0.0.0.0:50052 (bind-any) which is rejected by the control plane's
# is_trivially_invalid_address check. Evaluate at setup time so the IP
# is baked into the env file rather than requiring systemd to shell
# out on every start.
INTERNAL_IP=$(hostname -I | awk '{print $1}')
echo "LATTICE_ADVERTISE_ADDR=$INTERNAL_IP:50052" >> /etc/lattice/agent.env

# Install systemd service
cat > /etc/systemd/system/lattice-agent.service <<EOF
[Unit]
Description=Lattice Node Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/lattice-agent \\
  --node-id $NODE_ID \\
  --quorum-endpoint http://$QUORUM_IP:50051 \\
  --cpu-cores $(nproc) \\
  --memory-gb $(free -g | awk '/Mem:/{print $2}')
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

# Verify podman works
podman --version
echo "Podman storage driver: $(podman info --format '{{.Store.GraphDriverName}}')"

echo "=== Compute node $NODE_ID setup complete ==="
echo "Upload lattice-agent binary to /usr/local/bin/lattice-agent"
echo "Then: systemctl start lattice-agent"

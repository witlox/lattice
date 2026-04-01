#!/usr/bin/env bash
# install-compute.sh — Install lattice-agent on a compute node.
#
# Reusable on-prem. No GCP-specific logic.
#
# Usage: ./install-compute.sh <node-id> <quorum-endpoint>
#   node-id:          xname identifier (e.g., x1000c0s0b0n0)
#   quorum-endpoint:  gRPC endpoint of a quorum member (e.g., http://mgmt-1:50051)
#
# Expects:
#   - /opt/lattice/bin/lattice-agent
#   - Podman, squashfs-tools, nsenter installed (via Packer image or apt)
#   - LATTICE_AGENT_TOKEN env var or mTLS certs

set -euo pipefail

NODE_ID="${1:?Usage: install-compute.sh <node-id> <quorum-endpoint>}"
QUORUM_ENDPOINT="${2:?}"

BIN_DIR="/opt/lattice/bin"

echo "=== Installing lattice-agent (node $NODE_ID) ==="

# Create directories
mkdir -p /var/lib/lattice /var/cache/lattice/uenv /etc/lattice /scratch

# Symlink binary
[ -f "$BIN_DIR/lattice-agent" ] && ln -sf "$BIN_DIR/lattice-agent" /usr/local/bin/lattice-agent

# Write env file
cat > /etc/lattice/agent.env <<EOF
RUST_LOG=info,lattice=debug
LATTICE_QUORUM_ENDPOINT=$QUORUM_ENDPOINT
LATTICE_STATE_FILE=/var/lib/lattice/agent-state.json
EOF

# Add token auth if HMAC secret is available
if [ -n "${LATTICE_HMAC_SECRET:-}" ]; then
    # Generate a service account JWT signed with the shared HMAC secret
    TOKEN=$(python3 -c "
import json, base64, hmac, hashlib, time
header = base64.urlsafe_b64encode(json.dumps({'alg':'HS256','typ':'JWT'}).encode()).rstrip(b'=').decode()
payload = base64.urlsafe_b64encode(json.dumps({
    'sub': 'agent-$NODE_ID',
    'exp': int(time.time()) + 86400 * 365,
    'scope': 'operator'
}).encode()).rstrip(b'=').decode()
msg = f'{header}.{payload}'
sig = base64.urlsafe_b64encode(
    hmac.new(b'$LATTICE_HMAC_SECRET', msg.encode(), hashlib.sha256).digest()
).rstrip(b'=').decode()
print(f'{msg}.{sig}')
")
    echo "LATTICE_AGENT_TOKEN=$TOKEN" >> /etc/lattice/agent.env
fi

# Install systemd unit
if [ -f /opt/lattice/systemd/lattice-agent.service ]; then
    cp /opt/lattice/systemd/lattice-agent.service /etc/systemd/system/
fi

# Enable and start
systemctl daemon-reload
systemctl enable lattice-agent
systemctl start lattice-agent

echo "lattice-agent started (node $NODE_ID → $QUORUM_ENDPOINT)"

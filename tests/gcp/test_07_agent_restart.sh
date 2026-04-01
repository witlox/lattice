#!/bin/bash
# Test 07: Agent restart preserves state and node re-registers
set -euo pipefail

COMPUTE_IP="${COMPUTE_IPS[0]}"

echo "Checking node is registered..."
NODES_BEFORE=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes" | jq length)
echo "Nodes before restart: $NODES_BEFORE"

echo "Restarting agent on $COMPUTE_IP..."
ssh -o StrictHostKeyChecking=no "lattice@$COMPUTE_IP" "sudo systemctl restart lattice-agent"

echo "Waiting for re-registration..."
sleep 10

NODES_AFTER=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes" | jq length)
echo "Nodes after restart: $NODES_AFTER"

[ "$NODES_AFTER" -ge "$NODES_BEFORE" ] || { echo "node lost after restart"; exit 1; }

echo "Agent restart preserved registration."

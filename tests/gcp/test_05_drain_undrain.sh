#!/bin/bash
# Test 05: Drain and undrain a compute node
set -euo pipefail

# Get first node ID
NODE_ID=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes" | jq -r '.[0].id')
echo "Testing drain/undrain on node $NODE_ID"

# Drain
echo "Draining node..."
DRAIN_RESP=$(curl -sf -X POST "$API_URL/api/v1/nodes/$NODE_ID/drain" \
  -H "Authorization: Bearer $TOKEN")
echo "Drain response: $DRAIN_RESP"

# Check state (should be Drained if no active allocations)
sleep 1
STATE=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes/$NODE_ID" | jq -r '.state')
echo "Node state after drain: $STATE"
[ "$STATE" = "drained" ] || [ "$STATE" = "draining" ] || { echo "unexpected: $STATE"; exit 1; }

# Undrain (wait for Drained if Draining)
if [ "$STATE" = "draining" ]; then
  echo "Waiting for Drained..."
  for i in {1..30}; do
    STATE=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes/$NODE_ID" | jq -r '.state')
    [ "$STATE" = "drained" ] && break
    sleep 1
  done
fi

echo "Undraining node..."
curl -sf -X POST "$API_URL/api/v1/nodes/$NODE_ID/undrain" \
  -H "Authorization: Bearer $TOKEN"

STATE=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes/$NODE_ID" | jq -r '.state')
echo "Node state after undrain: $STATE"
[ "$STATE" = "ready" ] || { echo "expected ready, got $STATE"; exit 1; }

echo "Drain/undrain lifecycle complete."

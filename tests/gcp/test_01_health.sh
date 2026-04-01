#!/bin/bash
# Test 01: Cluster health — all components reachable
set -euo pipefail

echo "Checking API health..."
HEALTH=$(curl -sf "$API_URL/healthz")
[ "$HEALTH" = "ok" ] || { echo "healthz returned: $HEALTH"; exit 1; }

echo "Checking nodes registered..."
NODES=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes" | jq length)
[ "$NODES" -ge 2 ] || { echo "expected >=2 nodes, got $NODES"; exit 1; }

echo "All $NODES nodes registered, API healthy."

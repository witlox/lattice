#!/bin/bash
# Test 02: Submit a bare process allocation (no uenv, no container)
set -euo pipefail

echo "Submitting bare allocation..."
RESP=$(curl -sf -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tenant": "test",
    "entrypoint": "/bin/echo hello-lattice",
    "nodes": 1
  }')

ALLOC_ID=$(echo "$RESP" | jq -r '.allocation_id')

echo "Allocation $ALLOC_ID"
[ -n "$ALLOC_ID" ] && [ "$ALLOC_ID" != "null" ] || { echo "no allocation ID"; exit 1; }

echo "Bare allocation submitted successfully."

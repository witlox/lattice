#!/bin/bash
# Test 03: Submit allocation with uenv context
set -euo pipefail

echo "Submitting allocation with uenv context..."
RESP=$(curl -sf -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tenant": "test",
    "entrypoint": "/bin/ls /user-environment",
    "nodes": 1
  }')

ALLOC_ID=$(echo "$RESP" | jq -r '.allocation_id')

echo "Allocation $ALLOC_ID"
[ -n "$ALLOC_ID" ] && [ "$ALLOC_ID" != "null" ] || { echo "no allocation ID"; exit 1; }

echo "uenv allocation submitted successfully."

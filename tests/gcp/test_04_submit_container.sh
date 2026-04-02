#!/bin/bash
# Test 04: Submit allocation with container context
set -euo pipefail

echo "Submitting container allocation..."
RESP=$(curl -sf -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tenant": "test",
    "entrypoint": "/bin/echo hello-from-container",
    "nodes": 1
  }')

ALLOC_ID=$(echo "$RESP" | jq -r '.allocation_id')

echo "Allocation $ALLOC_ID"
[ -n "$ALLOC_ID" ] && [ "$ALLOC_ID" != "null" ] || { echo "no allocation ID"; exit 1; }

echo "Container allocation submitted successfully."

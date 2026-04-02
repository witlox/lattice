#!/bin/bash
# Test 09: Server rejects invalid requests (validation)
set -euo pipefail

echo "Submitting allocation with missing required fields..."
HTTP_CODE=$(curl -s -o /tmp/validation_resp.json -w "%{http_code}" \
  -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "entrypoint": "/bin/true"
  }')

echo "Response: $HTTP_CODE"
cat /tmp/validation_resp.json 2>/dev/null || true

# Should reject with 422 (missing tenant field)
[ "$HTTP_CODE" = "422" ] || { echo "expected 422 for missing fields, got $HTTP_CODE"; exit 1; }

echo "Validation correctly rejected invalid request."

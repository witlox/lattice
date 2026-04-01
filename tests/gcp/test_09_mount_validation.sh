#!/bin/bash
# Test 09: Mount overlap validation — server rejects overlapping mounts
set -euo pipefail

echo "Submitting allocation with overlapping mounts..."
HTTP_CODE=$(curl -s -o /tmp/overlap_resp.json -w "%{http_code}" \
  -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tenant_id": "test",
    "entrypoint": "/bin/true",
    "nodes": 1,
    "resources": {"cpus": 1, "memory_gb": 1},
    "environment": {
      "images": [
        {"spec": "a:v1", "image_type": "uenv", "mount_point": "/opt"},
        {"spec": "b:v1", "image_type": "uenv", "mount_point": "/opt/env"}
      ]
    }
  }')

echo "Response: $HTTP_CODE"
cat /tmp/overlap_resp.json | jq . 2>/dev/null || cat /tmp/overlap_resp.json

[ "$HTTP_CODE" = "400" ] || { echo "expected 400 for overlapping mounts, got $HTTP_CODE"; exit 1; }

echo "Mount overlap correctly rejected."

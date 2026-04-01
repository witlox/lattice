#!/bin/bash
# Test 03: Submit allocation with uenv image reference
set -euo pipefail

echo "Submitting allocation with uenv..."
RESP=$(curl -sf -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tenant_id": "test",
    "entrypoint": "/bin/ls /user-environment",
    "nodes": 1,
    "resources": {"cpus": 1, "memory_gb": 1},
    "environment": {
      "images": [{
        "spec": "prgenv-gnu/24.11:v1",
        "image_type": "uenv",
        "mount_point": "/user-environment"
      }]
    }
  }')

ALLOC_ID=$(echo "$RESP" | jq -r '.id')
IMAGES=$(echo "$RESP" | jq '.environment.images | length')

echo "Allocation $ALLOC_ID with $IMAGES images"
[ "$IMAGES" -ge 1 ] || { echo "expected >=1 images"; exit 1; }

IMAGE_TYPE=$(echo "$RESP" | jq -r '.environment.images[0].image_type')
[ "$IMAGE_TYPE" = "uenv" ] || { echo "expected uenv, got $IMAGE_TYPE"; exit 1; }

echo "uenv allocation submitted successfully."

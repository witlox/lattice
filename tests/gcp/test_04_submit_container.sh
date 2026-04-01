#!/bin/bash
# Test 04: Submit allocation with OCI container image
set -euo pipefail

echo "Pushing test image to registry..."
# Push a minimal image to the test registry for the container test
ssh -o StrictHostKeyChecking=no "lattice@${COMPUTE_IPS[0]}" \
  "podman pull --tls-verify=false docker.io/library/alpine:latest 2>/dev/null && \
   podman tag alpine:latest $REGISTRY_IP:5000/test/alpine:latest && \
   podman push --tls-verify=false $REGISTRY_IP:5000/test/alpine:latest" || true

echo "Submitting container allocation..."
RESP=$(curl -sf -X POST "$API_URL/api/v1/allocations" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"tenant_id\": \"test\",
    \"entrypoint\": \"/bin/echo hello-from-container\",
    \"nodes\": 1,
    \"resources\": {\"cpus\": 1, \"memory_gb\": 1},
    \"environment\": {
      \"images\": [{
        \"spec\": \"$REGISTRY_IP:5000/test/alpine:latest\",
        \"image_type\": \"oci\"
      }]
    }
  }")

ALLOC_ID=$(echo "$RESP" | jq -r '.id')
IMAGE_TYPE=$(echo "$RESP" | jq -r '.environment.images[0].image_type')

echo "Allocation $ALLOC_ID with image_type=$IMAGE_TYPE"
[ "$IMAGE_TYPE" = "oci" ] || { echo "expected oci, got $IMAGE_TYPE"; exit 1; }

echo "Container allocation submitted successfully."

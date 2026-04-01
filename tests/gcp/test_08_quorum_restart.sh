#!/bin/bash
# Test 08: Quorum restart without --bootstrap (the crash-loop fix)
set -euo pipefail

QUORUM_IP=$(cd "$PROJECT_DIR/infra/gcp" 2>/dev/null && terraform output -json quorum_ips | jq -r '.[0]' || echo "$API_URL" | sed 's|http://||;s|:.*||')

echo "Restarting quorum-1 (without --bootstrap)..."
ssh -o StrictHostKeyChecking=no "lattice@$QUORUM_IP" "sudo systemctl restart lattice-server"

echo "Waiting for quorum health..."
for i in {1..30}; do
  if curl -sf "$API_URL/healthz" > /dev/null 2>&1; then
    echo "  Quorum healthy after restart (${i}s)"
    exit 0
  fi
  sleep 1
done

echo "Quorum failed to recover after 30s"
exit 1

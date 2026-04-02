#!/bin/bash
# Test 08: Quorum restart without --bootstrap (the crash-loop fix)
set -euo pipefail

echo "Restarting quorum-1 (without --bootstrap)..."
gcloud compute ssh lattice-test-quorum-1 --zone=europe-west1-b \
  --command="sudo systemctl restart lattice-server" 2>/dev/null

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

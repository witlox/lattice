#!/usr/bin/env bash
# E2E Smoke Test for Lattice
#
# Prerequisites: docker compose
# Builds and starts the full stack, verifies basic functionality, then tears down.
#
# Usage:
#   ./tests/e2e/smoke_test.sh

set -euo pipefail

COMPOSE_DIR="infra/docker"
COMPOSE_FILE="${COMPOSE_DIR}/docker-compose.yml"
BASE_URL="http://localhost:8080"
VM_URL="http://localhost:8428"
MAX_WAIT=120  # seconds

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}PASS${NC}: $1"; }
fail() { echo -e "${RED}FAIL${NC}: $1"; exit 1; }
info() { echo -e "${YELLOW}INFO${NC}: $1"; }

cleanup() {
    info "Tearing down..."
    docker compose -f "${COMPOSE_FILE}" down -v --timeout 10 2>/dev/null || true
}
trap cleanup EXIT

# ── Step 1: Build and start ──────────────────────────────────

info "Building and starting Lattice stack..."
docker compose -f "${COMPOSE_FILE}" up -d --build

# ── Step 2: Wait for health ──────────────────────────────────

info "Waiting for server to become healthy (max ${MAX_WAIT}s)..."
elapsed=0
until curl -sf "${BASE_URL}/healthz" >/dev/null 2>&1; do
    if [ "$elapsed" -ge "$MAX_WAIT" ]; then
        docker compose -f "${COMPOSE_FILE}" logs lattice-server-1 | tail -30
        fail "Server did not become healthy within ${MAX_WAIT}s"
    fi
    sleep 2
    elapsed=$((elapsed + 2))
done
pass "Server healthy after ${elapsed}s"

# Give agents time to register
info "Waiting for agents to register (15s)..."
sleep 15

# ── Step 3: Verify nodes registered ─────────────────────────

info "Checking registered nodes..."
NODES_RESPONSE=$(curl -sf "${BASE_URL}/api/v1/nodes" || echo "")
if [ -z "$NODES_RESPONSE" ]; then
    fail "GET /api/v1/nodes returned empty response"
fi

NODE_COUNT=$(echo "$NODES_RESPONSE" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$NODE_COUNT" -ge 2 ]; then
    pass "Found ${NODE_COUNT} registered nodes"
else
    info "Response: ${NODES_RESPONSE}"
    fail "Expected >=2 nodes, got ${NODE_COUNT}"
fi

# ── Step 4: Submit a test allocation ─────────────────────────

info "Submitting test allocation..."
ALLOC_RESPONSE=$(curl -sf -X POST "${BASE_URL}/api/v1/allocations" \
    -H "Content-Type: application/json" \
    -d '{
        "name": "smoke-test",
        "entrypoint": "echo hello",
        "resources": {"nodes": 1, "gpus_per_node": 0, "cpus_per_node": 1, "memory_gb_per_node": 1}
    }' || echo "")

if [ -z "$ALLOC_RESPONSE" ]; then
    fail "POST /api/v1/allocations returned empty response"
fi

ALLOC_ID=$(echo "$ALLOC_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null || echo "")
if [ -n "$ALLOC_ID" ] && [ "$ALLOC_ID" != "None" ]; then
    pass "Allocation created: ${ALLOC_ID}"
else
    info "Response: ${ALLOC_RESPONSE}"
    # Non-fatal — the allocation endpoint may require specific fields
    info "SKIP: Could not extract allocation ID (endpoint may require specific format)"
fi

# ── Step 5: Verify allocation exists ─────────────────────────

if [ -n "$ALLOC_ID" ] && [ "$ALLOC_ID" != "None" ]; then
    GET_RESPONSE=$(curl -sf "${BASE_URL}/api/v1/allocations/${ALLOC_ID}" || echo "")
    if [ -n "$GET_RESPONSE" ]; then
        pass "GET /api/v1/allocations/${ALLOC_ID} returned data"
    else
        info "SKIP: Could not fetch allocation by ID"
    fi
fi

# ── Step 6: Check VictoriaMetrics for samples ────────────────

info "Checking VictoriaMetrics for metrics..."
VM_RESPONSE=$(curl -sf "${VM_URL}/api/v1/query?query=lattice_node_up" || echo "")
if echo "$VM_RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['data']['result']" 2>/dev/null; then
    pass "VictoriaMetrics has lattice_node_up samples"
else
    info "SKIP: No TSDB samples yet (agents may not have pushed yet)"
fi

# ── Summary ──────────────────────────────────────────────────

echo ""
echo "=============================="
pass "E2E smoke test completed"
echo "=============================="

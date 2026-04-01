#!/usr/bin/env bash
# Run the lattice GCP integration test suite.
#
# Prerequisites:
#   - Cluster deployed via gcp-deploy.sh
#   - terraform outputs available
#
# Usage:
#   ./scripts/gcp-test.sh [--test PATTERN]  # run specific test
#   ./scripts/gcp-test.sh                    # run all tests
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEST_DIR="$PROJECT_DIR/tests/gcp"
PATTERN="${2:-}"

# ─── Read cluster IPs ────────────────────────────────────────

cd "$PROJECT_DIR/infra/gcp"
API_IP=$(terraform output -json quorum_ips | jq -r '.[0]')
COMPUTE_IPS=($(terraform output -json compute_ips | jq -r '.[]'))
REGISTRY_IP=$(terraform output -raw registry_internal_ip)
cd "$PROJECT_DIR"

API_URL="http://$API_IP:8080"
GRPC_URL="$API_IP:50051"

echo "═══════════════════════════════════════════════════════"
echo " Lattice GCP Integration Tests"
echo " API: $API_URL"
echo " Compute: ${COMPUTE_IPS[*]}"
echo " Registry: $REGISTRY_IP:5000"
echo "═══════════════════════════════════════════════════════"

# ─── Generate HMAC token for CLI ──────────────────────────────

TOKEN=$(python3 -c "
import json, base64, hmac, hashlib, time
header = base64.urlsafe_b64encode(json.dumps({'alg':'HS256','typ':'JWT'}).encode()).rstrip(b'=').decode()
payload = base64.urlsafe_b64encode(json.dumps({'sub':'test-admin','exp':int(time.time())+3600,'scope':'admin'}).encode()).rstrip(b'=').decode()
msg = f'{header}.{payload}'
sig = base64.urlsafe_b64encode(hmac.new(b'lattice-test-hmac-secret', msg.encode(), hashlib.sha256).digest()).rstrip(b'=').decode()
print(f'{msg}.{sig}')
")

# ─── Test runner ──────────────────────────────────────────────

PASS=0
FAIL=0
SKIP=0
RESULTS=""

run_test() {
  local name=$1 script=$2
  if [[ -n "$PATTERN" && ! "$name" =~ $PATTERN ]]; then
    SKIP=$((SKIP + 1))
    return
  fi
  echo ""
  echo "── Test: $name ──"
  if bash "$script"; then
    echo "  ✔ PASS: $name"
    PASS=$((PASS + 1))
    RESULTS="$RESULTS\n  ✔ $name"
  else
    echo "  ✘ FAIL: $name"
    FAIL=$((FAIL + 1))
    RESULTS="$RESULTS\n  ✘ $name"
  fi
}

# ─── Test Suite ───────────────────────────────────────────────

# Export for test scripts
export API_URL GRPC_URL TOKEN REGISTRY_IP COMPUTE_IPS

for test_script in "$TEST_DIR"/test_*.sh; do
  test_name=$(basename "$test_script" .sh | sed 's/^test_//')
  run_test "$test_name" "$test_script"
done

# ─── Summary ─────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════════════"
echo " Results: $PASS passed, $FAIL failed, $SKIP skipped"
echo -e "$RESULTS"
echo "═══════════════════════════════════════════════════════"

exit $FAIL

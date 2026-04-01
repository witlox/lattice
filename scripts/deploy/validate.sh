#!/usr/bin/env bash
# validate.sh — Run the lattice test matrix against a deployed cluster.
#
# Usage: ./validate.sh <api-endpoint> <compute-nodes> [registry-ip]
#   api-endpoint:  REST API (e.g., http://10.0.0.10:8080)
#   compute-nodes: Comma-separated compute node xnames
#   registry-ip:   Optional OCI registry IP (for container tests)
#
# Exits 0 if all tests pass, 1 if any fail.

set -uo pipefail
# Note: NOT set -e — test failures should be counted, not abort the script.

API="${1:?Usage: validate.sh <api-endpoint> <compute-nodes> [registry-ip]}"
NODES="${2:?}"
REGISTRY="${3:-}"

IFS=',' read -ra NODE_LIST <<< "$NODES"
FIRST_NODE="${NODE_LIST[0]}"

# Generate HMAC token if not provided
if [ -z "${LATTICE_TOKEN:-}" ]; then
    HMAC_SECRET="${LATTICE_HMAC_SECRET:-lattice-test-hmac-secret}"
    LATTICE_TOKEN=$(python3 -c "
import json, base64, hmac, hashlib, time
header = base64.urlsafe_b64encode(json.dumps({'alg':'HS256','typ':'JWT'}).encode()).rstrip(b'=').decode()
payload = base64.urlsafe_b64encode(json.dumps({
    'sub': 'test-admin',
    'exp': int(time.time()) + 3600,
    'scope': 'admin'
}).encode()).rstrip(b'=').decode()
msg = f'{header}.{payload}'
sig = base64.urlsafe_b64encode(
    hmac.new(b'$HMAC_SECRET', msg.encode(), hashlib.sha256).digest()
).rstrip(b'=').decode()
print(f'{msg}.{sig}')
")
    export LATTICE_TOKEN
fi

AUTH="-H 'Authorization: Bearer $LATTICE_TOKEN'"

PASS=0
FAIL=0
SKIP=0

run_test() {
    local id="$1" name="$2"
    shift 2

    printf "  [%3s] %-55s " "$id" "$name"
    if eval "$@" &>/dev/null 2>&1; then
        echo "PASS"
        ((PASS++))
    else
        echo "FAIL"
        ((FAIL++))
    fi
}

expect_fail() {
    ! "$@"
}

echo "════════════════════════════════════════════════════════"
echo " Lattice GCP Validation"
echo " API:      $API"
echo " Nodes:    ${NODE_LIST[*]}"
echo " Registry: ${REGISTRY:-none}"
echo "════════════════════════════════════════════════════════"
echo ""

# --- Health ---
echo "Health:"
run_test 1 "API healthz reachable" \
    "curl -sf '$API/healthz'"
run_test 2 "Nodes registered (>= ${#NODE_LIST[@]})" \
    "[ \$(curl -sf $AUTH '$API/api/v1/nodes' | jq length) -ge ${#NODE_LIST[@]} ]"
run_test 3 "Auth discovery endpoint" \
    "curl -sf '$API/api/v1/auth/discovery' | jq -e .idp_url"

# --- Auth enforcement ---
echo ""
echo "Auth enforcement:"
run_test 4 "Unauthenticated → 401" \
    "[ \$(curl -so /dev/null -w '%{http_code}' '$API/api/v1/nodes') = '401' ]"
run_test 5 "Authenticated → 200" \
    "[ \$(curl -so /dev/null -w '%{http_code}' $AUTH '$API/api/v1/nodes') = '200' ]"
run_test 6 "Healthz is public (no auth)" \
    "curl -sf '$API/healthz'"

# --- Submit ---
echo ""
echo "Submit:"
run_test 7 "Submit bare allocation" \
    "curl -sf -X POST $AUTH -H 'Content-Type: application/json' '$API/api/v1/allocations' \
     -d '{\"tenant_id\":\"test\",\"entrypoint\":\"/bin/echo hello\",\"nodes\":1,\"resources\":{\"cpus\":1,\"memory_gb\":1}}'"
run_test 8 "Submit with uenv image ref" \
    "curl -sf -X POST $AUTH -H 'Content-Type: application/json' '$API/api/v1/allocations' \
     -d '{\"tenant_id\":\"test\",\"entrypoint\":\"/bin/ls\",\"nodes\":1,\"resources\":{\"cpus\":1,\"memory_gb\":1},\"environment\":{\"images\":[{\"spec\":\"prgenv-gnu/24.11:v1\",\"image_type\":\"uenv\",\"mount_point\":\"/user-environment\"}]}}'"

if [ -n "$REGISTRY" ]; then
    run_test 9 "Submit with OCI container ref" \
        "curl -sf -X POST $AUTH -H 'Content-Type: application/json' '$API/api/v1/allocations' \
         -d '{\"tenant_id\":\"test\",\"entrypoint\":\"/bin/echo\",\"nodes\":1,\"resources\":{\"cpus\":1,\"memory_gb\":1},\"environment\":{\"images\":[{\"spec\":\"$REGISTRY:5000/test/alpine:latest\",\"image_type\":\"oci\"}]}}'"
else
    echo "  [  9] Container submit (no registry)                         SKIP"
    ((SKIP++))
fi

# --- Validation ---
echo ""
echo "Validation:"
run_test 10 "Overlapping mounts → 400" \
    "[ \$(curl -so /dev/null -w '%{http_code}' -X POST $AUTH -H 'Content-Type: application/json' '$API/api/v1/allocations' \
     -d '{\"tenant_id\":\"test\",\"entrypoint\":\"/bin/true\",\"nodes\":1,\"resources\":{\"cpus\":1,\"memory_gb\":1},\"environment\":{\"images\":[{\"spec\":\"a:v1\",\"image_type\":\"uenv\",\"mount_point\":\"/opt\"},{\"spec\":\"b:v1\",\"image_type\":\"uenv\",\"mount_point\":\"/opt/env\"}]}}') = '400' ]"

# --- Drain lifecycle ---
echo ""
echo "Drain lifecycle:"
run_test 11 "Drain node" \
    "curl -sf -X POST $AUTH '$API/api/v1/nodes/$FIRST_NODE/drain'"
run_test 12 "Node state → drained (no active allocs)" \
    "sleep 1; [ \$(curl -sf $AUTH '$API/api/v1/nodes/$FIRST_NODE' | jq -r .state) = 'drained' ]"
run_test 13 "Undrain node" \
    "curl -sf -X POST $AUTH '$API/api/v1/nodes/$FIRST_NODE/undrain'"
run_test 14 "Node state → ready" \
    "[ \$(curl -sf $AUTH '$API/api/v1/nodes/$FIRST_NODE' | jq -r .state) = 'ready' ]"

# --- Quorum restart ---
echo ""
echo "Quorum restart:"
run_test 15 "Restart quorum (no --bootstrap)" \
    "ssh -o StrictHostKeyChecking=no lattice@\$(echo '$API' | sed 's|http://||;s|:.*||') 'sudo systemctl restart lattice-server' && sleep 5 && curl -sf '$API/healthz'"

# --- Summary ---
echo ""
echo "════════════════════════════════════════════════════════"
TOTAL=$((PASS + FAIL + SKIP))
echo " Results: $PASS passed, $FAIL failed, $SKIP skipped (of $TOTAL)"
echo "════════════════════════════════════════════════════════"

[ "$FAIL" -eq 0 ] && exit 0 || exit 1

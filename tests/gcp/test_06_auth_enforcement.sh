#!/bin/bash
# Test 06: Auth enforcement — unauthenticated requests rejected
set -euo pipefail

echo "Testing unauthenticated access..."
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/api/v1/nodes")
echo "No-auth response: $HTTP_CODE"
[ "$HTTP_CODE" = "401" ] || { echo "expected 401, got $HTTP_CODE"; exit 1; }

echo "Testing authenticated access..."
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer $TOKEN" "$API_URL/api/v1/nodes")
echo "Auth response: $HTTP_CODE"
[ "$HTTP_CODE" = "200" ] || { echo "expected 200, got $HTTP_CODE"; exit 1; }

echo "Testing healthz is public..."
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/healthz")
echo "Healthz response: $HTTP_CODE"
[ "$HTTP_CODE" = "200" ] || { echo "expected 200, got $HTTP_CODE"; exit 1; }

echo "Testing auth discovery is public..."
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/api/v1/auth/discovery")
echo "Discovery response: $HTTP_CODE"
# 200 if OIDC configured, 503 if not — both are acceptable (endpoint exists)
[ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "503" ] || { echo "unexpected: $HTTP_CODE"; exit 1; }

echo "Auth enforcement verified."

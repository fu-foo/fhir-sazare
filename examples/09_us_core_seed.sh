#!/bin/bash
# US Core seed — load a US Core compliant transaction Bundle into the server.
# Designed to give Inferno US Core Test Kit something to test against.
#
# Usage:
#   # Local server (no auth):
#   bash examples/09_us_core_seed.sh
#
#   # Remote server with Basic auth:
#   SAZARE_URL=https://servicerequest-demo1.fly.dev \
#   SAZARE_USER=admin SAZARE_PASS=yourpass \
#   bash examples/09_us_core_seed.sh
#
# After loading, point Inferno at the server and use Patient ID: patient-example

set -e

BASE="${SAZARE_URL:-http://localhost:8080}"
SEED_FILE="$(dirname "$0")/us-core-seed.json"

AUTH_ARGS=()
if [[ -n "$SAZARE_USER" && -n "$SAZARE_PASS" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:$SAZARE_PASS")
fi

echo "=== Posting US Core seed Bundle to $BASE ==="
echo "Seed file: $SEED_FILE"
echo

HTTP_CODE=$(curl -s -o /tmp/us-core-seed-response.json -w "%{http_code}" \
  -X POST "$BASE/" \
  "${AUTH_ARGS[@]}" \
  -H "Content-Type: application/json" \
  --data-binary "@$SEED_FILE")

echo "HTTP $HTTP_CODE"
echo

if [[ "$HTTP_CODE" =~ ^2 ]]; then
  echo "=== Per-entry response status ==="
  python3 -c "
import json
with open('/tmp/us-core-seed-response.json') as f:
    bundle = json.load(f)
for i, entry in enumerate(bundle.get('entry', [])):
    resp = entry.get('response', {})
    print(f\"  [{i+1}] {resp.get('status', '?')} {resp.get('location', '')}\")
"
  echo
  echo "=== Verify: fetch Patient/patient-example ==="
  curl -s "${AUTH_ARGS[@]}" "$BASE/Patient/patient-example" | python3 -m json.tool | head -20
else
  echo "=== Error response ==="
  cat /tmp/us-core-seed-response.json | python3 -m json.tool || cat /tmp/us-core-seed-response.json
  exit 1
fi

echo
echo "=== Done. Now point Inferno at $BASE with Patient ID: patient-example ==="

#!/bin/bash
# US Core v8 seed — load a US Core v8 compliant transaction Bundle into the server.
# Designed to give the Inferno US Core Test Kit (suite: us_core_v800) something to test against.
#
# This is the v8 sibling of 09_us_core_seed.sh. The v7 seed (us-core-seed.json) is FROZEN;
# all v8-specific edits live in us-core-v8-seed.json so v7 conformance never regresses.
#
# v8 deltas tracked here (confirmed against the live us_core_v800 run, not assumed):
#   - us-core-interpreter-needed: new must-support; SHALL appear on US Core Patient OR Encounter
#     (at least one). value[x] is valueCoding, required-bound to the Yes/No/Unknown answer set.
#   - us-core-sex: deprecated in STU8.0.1 (still valueCode); replaced by us-core-individual-sex.
#   - TLS / data-absent-reason: same posture as v7 (TLS terminated by Caddy recommended).
# NOTE: the seed currently starts as a verbatim copy of the v7 seed. Apply v8 fixes here only
# after a clean baseline run identifies the real failures (avoid baking in guessed values).
#
# Usage:
#   # Local server (no auth):
#   bash examples/11_us_core_v8_seed.sh
#
#   # Remote server with Basic auth:
#   SAZARE_URL=https://servicerequest-demo1.fly.dev \
#   SAZARE_USER=admin SAZARE_PASS=yourpass \
#   bash examples/11_us_core_v8_seed.sh
#
# After loading, point Inferno (us_core_v800) at the server with Patient ID: patient-example

set -e

BASE="${SAZARE_URL:-http://localhost:8080}"
SEED_FILE="$(dirname "$0")/us-core-v8-seed.json"

AUTH_ARGS=()
if [[ -n "$SAZARE_USER" && -n "$SAZARE_PASS" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:$SAZARE_PASS")
fi

echo "=== Posting US Core v8 seed Bundle to $BASE ==="
echo "Seed file: $SEED_FILE"
echo

HTTP_CODE=$(curl -s -o /tmp/us-core-v8-seed-response.json -w "%{http_code}" \
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
with open('/tmp/us-core-v8-seed-response.json') as f:
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
  cat /tmp/us-core-v8-seed-response.json | python3 -m json.tool || cat /tmp/us-core-v8-seed-response.json
  exit 1
fi

echo
echo "=== Done. Now point Inferno (us_core_v800) at $BASE with Patient ID: patient-example ==="

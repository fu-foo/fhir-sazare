#!/bin/bash
# Load the sazare demo cohort — five legible patients (diabetic, hypertensive
# smoker, prenatal, well-child vaccines, oncology work-up) — so that search,
# _has, _include and $everything visibly *do something* on a fresh server.
#
# This is the curated "hero" cohort. For a larger, realistic population see
# examples/demo/synthea/README.md (Synthea-generated US Core data).
#
# Usage:
#   # Local server (no auth):
#   bash examples/demo/load_demo.sh
#
#   # Remote server with Basic auth:
#   SAZARE_URL=https://your-server SAZARE_USER=admin SAZARE_PASS=secret \
#     bash examples/demo/load_demo.sh
set -e

BASE="${SAZARE_URL:-http://localhost:8080}"
SEED_FILE="$(dirname "$0")/cohort.json"

AUTH_ARGS=()
if [[ -n "$SAZARE_USER" && -n "$SAZARE_PASS" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:$SAZARE_PASS")
fi

echo "=== Loading demo cohort into $BASE ==="
HTTP_CODE=$(curl -s -o /tmp/sazare-demo-response.json -w "%{http_code}" \
  -X POST "$BASE/" \
  "${AUTH_ARGS[@]}" \
  -H "Content-Type: application/json" \
  --data-binary "@$SEED_FILE")
echo "HTTP $HTTP_CODE"

if [[ ! "$HTTP_CODE" =~ ^2 ]]; then
  echo "=== Error response ==="
  python3 -m json.tool < /tmp/sazare-demo-response.json || cat /tmp/sazare-demo-response.json
  exit 1
fi

python3 -c "
import json
b = json.load(open('/tmp/sazare-demo-response.json'))
ok = sum(1 for e in b.get('entry', []) if str(e.get('response', {}).get('status', '')).startswith('2'))
print(f'Loaded {ok}/{len(b.get(\"entry\", []))} resources.')
"

cat <<EOF

=== Now try these (the data makes them visibly do something) ===
  # Everyone in the population
  curl -s "$BASE/Patient" | python3 -m json.tool

  # Every patient who has an HbA1c result (reverse chain, _has)
  curl -s "$BASE/Patient?_has:Observation:patient:code=4548-4"

  # Ann Davis's entire chart in one call
  curl -s "$BASE/Patient/demo-ann-davis/\\\$everything"

  # All Type 2 diabetics
  curl -s "$BASE/Condition?code=http://snomed.info/sct|44054006"

  # Prescriptions, with each patient pulled in (_include)
  curl -s "$BASE/MedicationRequest?_include=MedicationRequest:subject"
EOF

#!/bin/bash
# JP Core seed — load a JP Core compliant transaction Bundle into the server.
# Demonstrates JP Core profile validation and Japanese (kana/kanji) name search.
#
# Usage:
#   bash examples/10_jp_core_seed.sh            # local server (no auth)
#   SAZARE_URL=https://your-host bash examples/10_jp_core_seed.sh
#
# After loading, try:
#   curl "http://localhost:8080/Patient?name-kana=ヤマダ"
#   curl "http://localhost:8080/Patient?name-kanji=山田"

set -e
BASE="${SAZARE_URL:-http://localhost:8080}"
SEED_FILE="$(dirname "$0")/jp-core-seed.json"

AUTH_ARGS=()
if [[ -n "$SAZARE_USER" && -n "$SAZARE_PASS" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:$SAZARE_PASS")
fi

echo "=== Posting JP Core seed Bundle to $BASE ==="
HTTP_CODE=$(curl -s -o /tmp/jp-core-seed-response.json -w "%{http_code}" \
  -X POST "$BASE/" "${AUTH_ARGS[@]}" \
  -H "Content-Type: application/json" \
  --data-binary "@$SEED_FILE")
echo "HTTP $HTTP_CODE"
echo

if [[ "$HTTP_CODE" =~ ^2 ]]; then
  echo "=== Per-entry response status ==="
  python3 -c "
import json
b=json.load(open('/tmp/jp-core-seed-response.json'))
for i,e in enumerate(b.get('entry',[])):
    r=e.get('response',{})
    print(f\"  [{i+1}] {r.get('status','?')} {r.get('location','')}\")
"
  echo
  echo "=== Try kana / kanji name search ==="
  echo "  name-kana=ヤマダ :"; curl -s "${AUTH_ARGS[@]}" "$BASE/Patient?name-kana=ヤマダ" | python3 -c "import json,sys; print('   total =', json.load(sys.stdin).get('total'))"
  echo "  name-kanji=山田  :"; curl -s "${AUTH_ARGS[@]}" "$BASE/Patient?name-kanji=山田" | python3 -c "import json,sys; print('   total =', json.load(sys.stdin).get('total'))"
else
  echo "=== Error response ==="; cat /tmp/jp-core-seed-response.json | python3 -m json.tool || cat /tmp/jp-core-seed-response.json
  exit 1
fi
echo
echo "=== Done. ==="

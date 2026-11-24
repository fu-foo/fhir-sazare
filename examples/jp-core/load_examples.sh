#!/bin/bash
# Load the JP Core package's example resources into a running sazare and run
# a few JP Core searches against them. Run setup.sh first.
#
# Usage:
#   bash examples/jp-core/load_examples.sh            # package under ./.jp-core
#   bash examples/jp-core/load_examples.sh /srv/sazare
#   SAZARE_URL=http://localhost:8080 SAZARE_USER=admin SAZARE_PASS=... bash examples/jp-core/load_examples.sh

set -euo pipefail

DIR="${1:-.}"
BASE="${SAZARE_URL:-http://localhost:8080}"
EXAMPLES="$DIR/.jp-core/package/example"

AUTH_ARGS=()
if [[ -n "${SAZARE_USER:-}" && -n "${SAZARE_PASS:-}" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:$SAZARE_PASS")
fi

if [[ ! -d "$EXAMPLES" ]]; then
  echo "No examples at $EXAMPLES — run examples/jp-core/setup.sh $DIR first." >&2
  exit 1
fi

echo "=== Loading JP Core examples into $BASE ==="
ok=0; failed=0
for f in "$EXAMPLES"/*.json; do
  # PUT to Type/id so the example ids (and the references between them) are kept.
  target=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["resourceType"]+"/"+d["id"])' "$f")
  code=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH_ARGS[@]}" -X PUT \
    -H 'Content-Type: application/fhir+json' --data-binary @"$f" "$BASE/$target")
  if [[ "$code" -lt 300 ]]; then
    ok=$((ok + 1))
  else
    failed=$((failed + 1))
    echo "  $code $target"
  fi
done
echo "loaded: $ok, failed: $failed"
echo

search() {
  local path="$1" query="$2"
  local total
  total=$(curl -s -G "${AUTH_ARGS[@]}" "$BASE/$path" --data-urlencode "$query" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("total"))')
  printf '%-4s %s?%s\n' "$total" "$path" "$query"
}

echo "=== JP Core searches (hits, query) ==="
search Patient 'jp-kana-sort=ヤマダ'
search Coverage 'jp-insured-personnumber=１８７'
search Coverage 'jp-insured-personsymbol=あいう'
search Organization 'jp-prefectureno=13'
search Organization 'jp-insurance-organizationno=http://jpfhir.jp/fhir/core/mhlw/IdSystem/MedicalOrganizationID|9876543'
search MedicationRequest 'jp-medication-start=ge2020-01-01'
search Patient '_has:MedicationRequest:subject:jp-medication-start=ge2020-01-01'

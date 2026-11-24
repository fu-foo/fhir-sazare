#!/bin/bash
# Load the JP Core package's example resources into a running sazare, then
# check JP Core searches and validation against expected results. Exits
# non-zero if anything differs (CI runs this). Run setup.sh first.
#
# Usage:
#   bash examples/jp-core/load_examples.sh            # package under ./.jp-core
#   bash examples/jp-core/load_examples.sh /srv/sazare
#   SAZARE_URL=http://localhost:8080 SAZARE_USER=admin SAZARE_PASS=... bash examples/jp-core/load_examples.sh
#
# The terminology checks run when <dir>/terminology exists (setup.sh --terminology).

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

failures=0

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
[[ "$failed" -eq 0 ]] || failures=$((failures + 1))
echo

# search <expected hits> <type> <query>
search() {
  local expected="$1" path="$2" query="$3" total mark="ok"
  total=$(curl -s -G "${AUTH_ARGS[@]}" "$BASE/$path" --data-urlencode "$query" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("total"))')
  if [[ "$total" != "$expected" ]]; then
    mark="FAIL (expected $expected)"
    failures=$((failures + 1))
  fi
  printf '%-4s %s?%s  %s\n' "$total" "$path" "$query" "$mark"
}

echo "=== JP Core searches (hits, query) ==="
search 1 Patient 'jp-kana-sort=ヤマダ'
search 1 Patient 'jp-kana-sort:exact=ヤマダ タロウ'
search 1 Coverage 'jp-insured-personnumber=１８７'
search 1 Coverage 'jp-insured-personsymbol=あいう'
search 1 Coverage 'jp-insured-personsubnumber=05'
search 2 Organization 'jp-prefectureno=13'
search 2 Organization 'jp-insurance-organizationcategory=1'
search 1 Organization 'jp-insurance-organizationno=http://jpfhir.jp/fhir/core/mhlw/IdSystem/MedicalOrganizationID|9876543'
search 2 MedicationRequest 'jp-medication-start=ge2020-01-01'
search 0 MedicationRequest 'jp-medication-start=lt2020-01-01'
search 1 Patient '_has:MedicationRequest:subject:jp-medication-start=ge2020-01-01'
search 1 Coverage 'beneficiary:Patient.jp-kana-sort=ヤマダ'
echo

# validate <expected: accept|reject> <label> <type> <json>
validate() {
  local expected="$1" label="$2" type="$3" body="$4" got mark="ok"
  # $validate answers 200 either way; an error/fatal issue means rejected.
  got=$(curl -s "${AUTH_ARGS[@]}" -X POST -H 'Content-Type: application/fhir+json' \
    -d "$body" "$BASE/$type/\$validate" \
    | python3 -c 'import json,sys; o=json.load(sys.stdin); print("reject" if any(i.get("severity") in ("error","fatal") for i in o.get("issue",[])) else "accept")')
  if [[ "$got" != "$expected" ]]; then
    mark="FAIL (expected $expected)"
    failures=$((failures + 1))
  fi
  printf '%-7s %s  %s\n' "$got" "$label" "$mark"
}

PROC='{"resourceType":"Procedure","meta":{"profile":["http://jpfhir.jp/fhir/core/StructureDefinition/JP_Procedure"]},"status":"completed","subject":{"reference":"Patient/jp-patient-example-1"},"code":{"coding":[{"system":"http://jpfhir.jp/fhir/core/CodeSystem/JP_ProcedureCodesMedical_CS","code":"CODE"}]}}'

echo "=== JP Core validation ==="
validate reject 'JP_Patient without identifier' Patient \
  '{"resourceType":"Patient","meta":{"profile":["http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"]},"gender":"male"}'
validate accept 'JP_Procedure with a JP procedure code' Procedure "${PROC/CODE/140000610}"
if [[ -d "$DIR/terminology" ]]; then
  validate reject 'JP_Procedure with an unknown JP procedure code' Procedure "${PROC/CODE/NOPE}"
fi
echo

if [[ "$failures" -gt 0 ]]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "all checks passed"

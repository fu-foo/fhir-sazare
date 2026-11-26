#!/usr/bin/env bash
# CI: JP Core end to end. With the JP Core package laid out by
# examples/ig/setup.sh (as package 1) and its examples loaded, check every JP Core
# search parameter plus profile and code-system validation.
#
# The values searched for (names, insurance numbers) are read from the package's
# own examples at run time, so none is written here.
#
# usage: SAZARE_URL=http://localhost:18080 bash ci/jp_core_check.sh <dir>
set -euo pipefail

DIR="$1"
BASE="${SAZARE_URL:-http://localhost:8080}"
EX="$DIR/.ig/1/package/example"
failures=0

# value <kind>: a value taken from the examples
value() {
  python3 - "$EX" "$1" <<'PY'
import glob, json, sys
ex, kind = sys.argv[1], sys.argv[2]
res = [json.load(open(f)) for f in glob.glob(ex + "/*.json")]
def ext_value(r, part):
    for e in r.get("extension", []):
        if part in e.get("url", ""):
            return e.get("valueString")
if kind in ("kana", "kana-prefix"):
    for r in res:
        if r["resourceType"] != "Patient":
            continue
        for n in r.get("name", []):
            reps = [e.get("valueCode") for e in n.get("extension", [])]
            if "SYL" in reps and n.get("text"):
                print(n["text"] if kind == "kana" else n["text"].split()[0]); sys.exit()
else:
    part = {"insured-number": "InsuredPersonNumber", "insured-symbol": "InsuredPersonSymbol",
            "insured-subnumber": "InsuredPersonSubNumber"}[kind]
    for r in res:
        if r["resourceType"] == "Coverage" and ext_value(r, part):
            print(ext_value(r, part)); sys.exit()
sys.exit(f"no {kind} in the examples")
PY
}

# search <expected hits> <type> <query>
search() {
  local expected="$1" path="$2" query="$3" total mark="ok"
  total=$(curl -s -G "$BASE/$path" --data-urlencode "$query" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("total"))')
  if [[ "$total" != "$expected" ]]; then
    mark="FAIL (expected $expected)"
    failures=$((failures + 1))
  fi
  printf '%-4s %s?%s  %s\n' "$total" "$path" "${query%%=*}=…" "$mark"
}

KANA=$(value kana); KANA_PREFIX=$(value kana-prefix)
echo "=== JP Core searches (hits, query) ==="
search 1 Patient "jp-kana-sort=$KANA_PREFIX"
search 1 Patient "jp-kana-sort:exact=$KANA"
search 1 Coverage "jp-insured-personnumber=$(value insured-number)"
search 1 Coverage "jp-insured-personsymbol=$(value insured-symbol)"
search 1 Coverage "jp-insured-personsubnumber=$(value insured-subnumber)"
search 2 Organization 'jp-prefectureno=13'
search 2 Organization 'jp-insurance-organizationcategory=1'
search 1 Organization 'jp-insurance-organizationno=http://jpfhir.jp/fhir/core/mhlw/IdSystem/MedicalOrganizationID|9876543'
search 2 MedicationRequest 'jp-medication-start=ge2020-01-01'
search 0 MedicationRequest 'jp-medication-start=lt2020-01-01'
search 1 Patient '_has:MedicationRequest:subject:jp-medication-start=ge2020-01-01'
search 1 Coverage "beneficiary:Patient.jp-kana-sort=$KANA_PREFIX"
echo

# validate <expected: accept|reject> <label> <type> <json>
validate() {
  local expected="$1" label="$2" type="$3" body="$4" got mark="ok"
  # $validate answers 200 either way; an error/fatal issue means rejected.
  got=$(curl -s -X POST -H 'Content-Type: application/fhir+json' \
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

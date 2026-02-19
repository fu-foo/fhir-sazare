#!/bin/bash
# Bulk NDJSON Import and Export
# Usage: bash examples/07_bulk_ndjson.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== 1. Import: Load multiple resources via NDJSON ==="
curl -s -X POST "$BASE/\$import" \
  -H "Content-Type: application/x-ndjson" \
  -d '{"resourceType":"Patient","name":[{"family":"Import-Test-1","given":["Alpha"]}],"gender":"male"}
{"resourceType":"Patient","name":[{"family":"Import-Test-2","given":["Beta"]}],"gender":"female"}
{"resourceType":"Observation","status":"final","code":{"coding":[{"system":"http://loinc.org","code":"8302-2","display":"Body Height"}]},"valueQuantity":{"value":170,"unit":"cm"}}' \
  | python3 -m json.tool
echo

echo "=== 2. Export: All resources as NDJSON ==="
echo "(First 10 lines)"
curl -s "$BASE/\$export" | head -10
echo
echo

echo "=== 3. Export: Filtered by type ==="
echo "(Patient only)"
curl -s "$BASE/\$export?_type=Patient" | head -5
echo
echo

echo "=== 4. Import from file ==="
# Create a temp NDJSON file
TMPFILE=$(mktemp /tmp/sazare-import-XXXXXX.ndjson)
cat > "$TMPFILE" << 'NDJSON'
{"resourceType":"AllergyIntolerance","clinicalStatus":{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/allergyintolerance-clinical","code":"active"}]},"code":{"coding":[{"system":"http://snomed.info/sct","code":"91935009","display":"Penicillin allergy"}]}}
{"resourceType":"Immunization","status":"completed","vaccineCode":{"coding":[{"system":"http://hl7.org/fhir/sid/cvx","code":"208","display":"COVID-19 mRNA vaccine"}]},"occurrenceDateTime":"2026-01-15"}
NDJSON

echo "Importing from $TMPFILE ..."
curl -s -X POST "$BASE/\$import" \
  -H "Content-Type: application/x-ndjson" \
  --data-binary "@$TMPFILE" \
  | python3 -m json.tool

rm -f "$TMPFILE"
echo

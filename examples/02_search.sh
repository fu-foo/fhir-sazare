#!/bin/bash
# Search operations
# Usage: bash examples/02_search.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== Setup: Create test data ==="
curl -s -X POST "$BASE/Patient" -H "Content-Type: application/json" \
  -d '{"resourceType":"Patient","name":[{"family":"Smith","given":["Alice"]}],"gender":"female","birthDate":"1985-03-15"}' > /dev/null
curl -s -X POST "$BASE/Patient" -H "Content-Type: application/json" \
  -d '{"resourceType":"Patient","name":[{"family":"Johnson","given":["Bob"]}],"gender":"male","birthDate":"1978-07-20"}' > /dev/null
curl -s -X POST "$BASE/Patient" -H "Content-Type: application/json" \
  -d '{"resourceType":"Patient","name":[{"family":"Smith","given":["Charlie"]}],"gender":"male","birthDate":"2000-12-01"}' > /dev/null

# Create Observations linked to a Patient
PATIENT=$(curl -s -X POST "$BASE/Patient" -H "Content-Type: application/json" \
  -d '{"resourceType":"Patient","name":[{"family":"Doe","given":["Jane"]}],"gender":"female"}')
PID=$(echo "$PATIENT" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])" 2>/dev/null)

curl -s -X POST "$BASE/Observation" -H "Content-Type: application/json" \
  -d "{\"resourceType\":\"Observation\",\"status\":\"final\",\"code\":{\"coding\":[{\"system\":\"http://loinc.org\",\"code\":\"29463-7\",\"display\":\"Body Weight\"}]},\"subject\":{\"reference\":\"Patient/$PID\"},\"valueQuantity\":{\"value\":60,\"unit\":\"kg\"}}" > /dev/null

echo "Done."
echo

echo "=== 1. Search by name ==="
curl -s "$BASE/Patient?name=Smith" | python3 -m json.tool
echo

echo "=== 2. Search by gender ==="
curl -s "$BASE/Patient?gender=female" | python3 -m json.tool
echo

echo "=== 3. Search with pagination ==="
curl -s "$BASE/Patient?_count=2&_offset=0" | python3 -m json.tool
echo

echo "=== 4. Chain search: Observations for Patient named Doe ==="
curl -s "$BASE/Observation?subject:Patient.name=Doe" | python3 -m json.tool
echo

echo "=== 5. Search with _include ==="
curl -s "$BASE/Observation?subject:Patient.name=Doe&_include=Observation:subject" | python3 -m json.tool
echo

#!/bin/bash
# Basic CRUD operations
# Usage: bash examples/01_basic_crud.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== 1. CREATE (POST) ==="
RESPONSE=$(curl -s -w "\n%{http_code}" -X POST "$BASE/Patient" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Patient",
    "name": [{"family": "Doe", "given": ["Jane"]}],
    "gender": "male",
    "birthDate": "1990-01-01"
  }')

BODY=$(echo "$RESPONSE" | sed '$d')
HTTP_CODE=$(echo "$RESPONSE" | tail -1)
echo "Status: $HTTP_CODE"
echo "$BODY" | python3 -m json.tool 2>/dev/null || echo "$BODY"

# Extract the ID for subsequent operations
PATIENT_ID=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])" 2>/dev/null)
echo "Created Patient ID: $PATIENT_ID"
echo

echo "=== 2. READ (GET) ==="
curl -s "$BASE/Patient/$PATIENT_ID" | python3 -m json.tool
echo

echo "=== 3. UPDATE (PUT) ==="
curl -s -w "\nStatus: %{http_code}\n" -X PUT "$BASE/Patient/$PATIENT_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"resourceType\": \"Patient\",
    \"id\": \"$PATIENT_ID\",
    \"name\": [{\"family\": \"Doe\", \"given\": [\"Jane\", \"M\"]}],
    \"gender\": \"male\",
    \"birthDate\": \"1990-01-01\",
    \"telecom\": [{\"system\": \"phone\", \"value\": \"03-1234-5678\"}]
  }" | python3 -m json.tool 2>/dev/null
echo

echo "=== 4. READ updated resource ==="
curl -s "$BASE/Patient/$PATIENT_ID" | python3 -m json.tool
echo

echo "=== 5. VERSION HISTORY ==="
curl -s "$BASE/Patient/$PATIENT_ID/_history" | python3 -m json.tool
echo

echo "=== 6. READ specific version (vread) ==="
curl -s "$BASE/Patient/$PATIENT_ID/_history/1" | python3 -m json.tool
echo

echo "=== 7. DELETE ==="
curl -s -w "Status: %{http_code}\n" -X DELETE "$BASE/Patient/$PATIENT_ID"
echo

echo "=== 8. Verify DELETE (should be 404/gone) ==="
curl -s -w "\nStatus: %{http_code}\n" "$BASE/Patient/$PATIENT_ID"
echo

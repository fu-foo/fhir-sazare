#!/bin/bash
# Resource Validation
# Usage: bash examples/08_validation.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== 1. Valid Patient ==="
curl -s -X POST "$BASE/Patient/\$validate" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Patient",
    "name": [{"family": "Valid", "given": ["Test"]}],
    "gender": "male"
  }' | python3 -m json.tool
echo

echo "=== 2. Invalid: Missing resourceType ==="
curl -s -X POST "$BASE/Patient/\$validate" \
  -H "Content-Type: application/json" \
  -d '{
    "name": [{"family": "NoType"}]
  }' | python3 -m json.tool
echo

echo "=== 3. Invalid: Wrong resourceType for endpoint ==="
curl -s -X POST "$BASE/Patient/\$validate" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Observation",
    "status": "final"
  }' | python3 -m json.tool
echo

echo "=== 4. Invalid Observation: Missing required fields ==="
curl -s -X POST "$BASE/Observation/\$validate" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Observation"
  }' | python3 -m json.tool
echo

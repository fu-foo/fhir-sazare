#!/bin/bash
# Conditional Create (If-None-Exist)
# Prevents duplicate resources by checking search criteria before creation
# Usage: bash examples/05_conditional_create.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== 1. First create: Should succeed (201 Created) ==="
curl -s -w "\nStatus: %{http_code}\n" -X POST "$BASE/Patient" \
  -H "Content-Type: application/json" \
  -H "If-None-Exist: identifier=http://hospital.example.org/mrn|MRN-001" \
  -d '{
    "resourceType": "Patient",
    "identifier": [{"system": "http://hospital.example.org/mrn", "value": "MRN-001"}],
    "name": [{"family": "Ito", "given": ["Miki"]}]
  }' | python3 -m json.tool 2>/dev/null
echo

echo "=== 2. Duplicate create: Should return existing (200 OK, not 201) ==="
curl -s -w "\nStatus: %{http_code}\n" -X POST "$BASE/Patient" \
  -H "Content-Type: application/json" \
  -H "If-None-Exist: identifier=http://hospital.example.org/mrn|MRN-001" \
  -d '{
    "resourceType": "Patient",
    "identifier": [{"system": "http://hospital.example.org/mrn", "value": "MRN-001"}],
    "name": [{"family": "Ito", "given": ["Miki"]}]
  }' | python3 -m json.tool 2>/dev/null
echo

echo "=== 3. Conditional create in Bundle (ifNoneExist) ==="
curl -s -X POST "$BASE/" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Bundle",
    "type": "transaction",
    "entry": [
      {
        "fullUrl": "urn:uuid:patient-cond",
        "resource": {
          "resourceType": "Patient",
          "identifier": [{"system": "http://hospital.example.org/mrn", "value": "MRN-002"}],
          "name": [{"family": "Nakamura", "given": ["Yuto"]}]
        },
        "request": {
          "method": "POST",
          "url": "Patient",
          "ifNoneExist": "identifier=http://hospital.example.org/mrn|MRN-002"
        }
      }
    ]
  }' | python3 -m json.tool
echo
echo "Run this script again to see that MRN-002 is not duplicated."

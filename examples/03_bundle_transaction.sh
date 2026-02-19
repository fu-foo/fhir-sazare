#!/bin/bash
# Bundle Transaction (all-or-nothing)
# Demonstrates urn:uuid reference resolution between entries
# Usage: bash examples/03_bundle_transaction.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== Transaction Bundle: Patient + Observation with reference ==="
echo "The Observation references the Patient via urn:uuid:patient-1."
echo "The server resolves this to the actual Patient ID after creation."
echo

curl -s -X POST "$BASE/" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Bundle",
    "type": "transaction",
    "entry": [
      {
        "fullUrl": "urn:uuid:patient-1",
        "resource": {
          "resourceType": "Patient",
          "name": [{"family": "Kimura", "given": ["Sakura"]}],
          "gender": "female",
          "birthDate": "1995-04-01"
        },
        "request": {"method": "POST", "url": "Patient"}
      },
      {
        "fullUrl": "urn:uuid:obs-bp",
        "resource": {
          "resourceType": "Observation",
          "status": "final",
          "code": {
            "coding": [{
              "system": "http://loinc.org",
              "code": "85354-9",
              "display": "Blood pressure panel"
            }]
          },
          "subject": {"reference": "urn:uuid:patient-1"},
          "component": [
            {
              "code": {"coding": [{"system": "http://loinc.org", "code": "8480-6", "display": "Systolic"}]},
              "valueQuantity": {"value": 120, "unit": "mmHg"}
            },
            {
              "code": {"coding": [{"system": "http://loinc.org", "code": "8462-4", "display": "Diastolic"}]},
              "valueQuantity": {"value": 80, "unit": "mmHg"}
            }
          ]
        },
        "request": {"method": "POST", "url": "Observation"}
      },
      {
        "fullUrl": "urn:uuid:enc-1",
        "resource": {
          "resourceType": "Encounter",
          "status": "finished",
          "class": {"system": "http://terminology.hl7.org/CodeSystem/v3-ActCode", "code": "AMB"},
          "subject": {"reference": "urn:uuid:patient-1"},
          "period": {"start": "2026-02-07T09:00:00+09:00", "end": "2026-02-07T09:30:00+09:00"}
        },
        "request": {"method": "POST", "url": "Encounter"}
      }
    ]
  }' | python3 -m json.tool

echo
echo "=== Check: The urn:uuid references should be resolved to actual IDs ==="
echo "Look at the Observation's subject.reference and Encounter's subject.reference in the response."

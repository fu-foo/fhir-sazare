#!/bin/bash
# Bundle Batch (independent entries)
# Each entry is processed independently; failures don't affect others
# Usage: bash examples/04_bundle_batch.sh
#
# Prerequisites: Server running at http://localhost:8080

BASE=http://localhost:8080

echo "=== Batch Bundle: Multiple independent operations ==="
echo

curl -s -X POST "$BASE/" \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Bundle",
    "type": "batch",
    "entry": [
      {
        "resource": {
          "resourceType": "Practitioner",
          "name": [{"family": "Watanabe", "given": ["Kenji"]}],
          "qualification": [{
            "code": {"coding": [{"system": "http://jpfhir.jp/fhir/CodeSystem/physician-category", "code": "medical"}]},
            "period": {"start": "2010-04-01"}
          }]
        },
        "request": {"method": "POST", "url": "Practitioner"}
      },
      {
        "resource": {
          "resourceType": "Organization",
          "name": "Tokyo General Hospital",
          "type": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/organization-type", "code": "prov"}]}],
          "telecom": [{"system": "phone", "value": "03-0000-0000"}],
          "address": [{"city": "Tokyo", "country": "JP"}]
        },
        "request": {"method": "POST", "url": "Organization"}
      },
      {
        "resource": {
          "resourceType": "Condition",
          "clinicalStatus": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-clinical", "code": "active"}]},
          "code": {"coding": [{"system": "http://hl7.org/fhir/sid/icd-10", "code": "I10", "display": "Essential hypertension"}]},
          "recordedDate": "2026-01-15"
        },
        "request": {"method": "POST", "url": "Condition"}
      },
      {
        "resource": {
          "resourceType": "MedicationRequest",
          "status": "active",
          "intent": "order",
          "medicationCodeableConcept": {
            "coding": [{"system": "http://jpfhir.jp/fhir/CodeSystem/YJ", "code": "2149023F1025", "display": "Amlodipine 5mg"}]
          },
          "dosageInstruction": [{
            "timing": {"code": {"text": "Once daily after breakfast"}},
            "doseAndRate": [{"doseQuantity": {"value": 1, "unit": "tablet"}}]
          }]
        },
        "request": {"method": "POST", "url": "MedicationRequest"}
      }
    ]
  }' | python3 -m json.tool

echo
echo "=== Each entry has its own response status (201, 200, or error) ==="

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
          "name": [{"family": "Lee", "given": ["Jordan"]}],
          "qualification": [{
            "code": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/v2-0360", "code": "MD", "display": "Doctor of Medicine"}]},
            "period": {"start": "2010-04-01"}
          }]
        },
        "request": {"method": "POST", "url": "Practitioner"}
      },
      {
        "resource": {
          "resourceType": "Organization",
          "name": "Riverside General Hospital",
          "type": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/organization-type", "code": "prov"}]}],
          "telecom": [{"system": "phone", "value": "555-0100"}],
          "address": [{"city": "Boston", "state": "MA", "country": "US"}]
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
            "coding": [{"system": "http://www.nlm.nih.gov/research/umls/rxnorm", "code": "197361", "display": "Amlodipine 5 MG Oral Tablet"}]
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

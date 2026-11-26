#!/usr/bin/env python3
"""Generate a deterministic synthetic NDJSON dataset for the search benchmark.

usage: gen_data.py <patients> <observations-per-patient> > data.ndjson

Patients get one of 1000 family names (the first ten common Japanese surnames),
a gender, birthDate and an MRN identifier; Observations get one of eight LOINC
codes, a vital-signs category, a subject reference and an effectiveDateTime.
"""
import json
import random
import sys

random.seed(1)
npat, obs_per = int(sys.argv[1]), int(sys.argv[2])
families = ["Smith", "Garcia", "Chen", "Okafor", "Muller", "Rossi",
            "Kowalski", "Silva", "Novak", "Haddad"] + [f"Fam{i}" for i in range(990)]
codes = ["8867-4", "8310-5", "29463-7", "8302-2", "85354-9", "2708-6", "9279-1", "39156-5"]
out = sys.stdout

for i in range(npat):
    out.write(json.dumps({
        "resourceType": "Patient", "id": f"p{i}",
        "gender": random.choice(["male", "female"]),
        "birthDate": f"{random.randint(1930, 2020)}-{random.randint(1, 12):02d}-{random.randint(1, 28):02d}",
        "name": [{"family": random.choice(families), "given": [f"G{i % 500}"]}],
        "identifier": [{"system": "urn:bench", "value": f"MRN{i}"}],
    }) + "\n")

for i in range(npat):
    for j in range(obs_per):
        out.write(json.dumps({
            "resourceType": "Observation", "id": f"o{i}-{j}", "status": "final",
            "category": [{"coding": [{
                "system": "http://terminology.hl7.org/CodeSystem/observation-category",
                "code": "vital-signs"}]}],
            "code": {"coding": [{"system": "http://loinc.org", "code": random.choice(codes)}]},
            "subject": {"reference": f"Patient/p{i}"},
            "effectiveDateTime": f"20{random.randint(15, 25)}-{random.randint(1, 12):02d}-{random.randint(1, 28):02d}T10:00:00Z",
            "valueQuantity": {"value": random.randint(40, 200), "unit": "/min",
                              "system": "http://unitsofmeasure.org", "code": "/min"},
        }) + "\n")

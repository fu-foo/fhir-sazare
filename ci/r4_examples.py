#!/usr/bin/env python3
"""The R4 examples (examples-json.zip) as NDJSON, one resource per line, for
`r4_examples_fit` (sazare-core/src/validation/structure.rs):

  curl -sfLO https://hl7.org/fhir/R4/examples-json.zip
  python3 ci/r4_examples.py examples-json.zip > r4-examples.ndjson
  R4_EXAMPLES=r4-examples.ndjson cargo test -p sazare-core -- --ignored r4_examples
"""
import json
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1]) as z:
    for name in sorted(z.namelist()):
        if not name.endswith(".json"):
            continue
        try:
            v = json.loads(z.read(name))
        except ValueError:
            continue
        if isinstance(v, dict) and "resourceType" in v:
            print(json.dumps(v, ensure_ascii=False))

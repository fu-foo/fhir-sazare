#!/usr/bin/env python3
"""Generate r4.tsv: every element of the R4 resources and complex data types.

  curl -sfLO https://hl7.org/fhir/R4/profiles-resources.json
  curl -sfLO https://hl7.org/fhir/R4/profiles-types.json
  python3 sazare-core/structure/generate.py profiles-resources.json profiles-types.json > sazare-core/structure/r4.tsv

One line per element (from the snapshots, so inherited ones are there too):
path, max cardinality, and its types (comma-separated), or `@path` for an
element defined as another's content (`Questionnaire.item.item` →
`@Questionnaire.item`). The validator checks a resource's elements against it:
known names, JSON types, a list or a single value.

Source: HL7 FHIR R4 (4.0.1), CC0-1.0 (see NOTICE.md).
"""
import json
import sys

SYSTEM = {"http://hl7.org/fhirpath/System.String": "string"}


def main(*srcs):
    for src in srcs:
        for entry in json.load(open(src))["entry"]:
            sd = entry["resource"]
            if sd.get("resourceType") != "StructureDefinition":
                continue
            if sd["kind"] not in ("resource", "complex-type") or sd.get("derivation") == "constraint":
                continue
            for el in sd["snapshot"]["element"]:
                path = el["path"]
                if "." not in path:
                    continue
                if "contentReference" in el:
                    types = "@" + el["contentReference"].lstrip("#")
                else:
                    types = ",".join(SYSTEM.get(t["code"], t["code"]) for t in el.get("type", []))
                print(f"{path}\t{el.get('max', '*')}\t{types}")


main(*sys.argv[1:])

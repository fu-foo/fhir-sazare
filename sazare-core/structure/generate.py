#!/usr/bin/env python3
"""Generate r4.tsv: every element of the R4 resources and complex data types.

  curl -sfLO https://hl7.org/fhir/R4/profiles-resources.json
  curl -sfLO https://hl7.org/fhir/R4/profiles-types.json
  python3 sazare-core/structure/generate.py profiles-resources.json profiles-types.json > sazare-core/structure/r4.tsv

One line per element (from the snapshots, so inherited ones are there too):
path, max cardinality, its types (comma-separated) or `@path` for an element
defined as another's content (`Questionnaire.item.item` →
`@Questionnaire.item`), and min cardinality. The validator checks a
resource's elements against it: known names, JSON types, a list or a single
value, required elements.

With `--primitives`, the primitive types instead: name and the regex their
values match (from profiles-types.json), into r4-primitives.tsv:

  python3 sazare-core/structure/generate.py --primitives profiles-types.json > sazare-core/structure/r4-primitives.tsv

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
                print(f"{path}\t{el.get('max', '*')}\t{types}\t{el.get('min', 0)}")


def primitives(src):
    for entry in json.load(open(src))["entry"]:
        sd = entry["resource"]
        if sd.get("resourceType") != "StructureDefinition" or sd.get("kind") != "primitive-type":
            continue
        for el in sd["snapshot"]["element"]:
            if not el["path"].endswith(".value"):
                continue
            for t in el.get("type", []):
                for x in t.get("extension", []):
                    if x["url"].endswith("/regex"):
                        print(f"{sd['id']}\t{x['valueString']}")


if sys.argv[1] == "--primitives":
    primitives(sys.argv[2])
else:
    main(*sys.argv[1:])

#!/usr/bin/env python3
"""Generate r4.tsv: the top-level summary elements of each R4 resource type.

  curl -sfLO https://hl7.org/fhir/R4/profiles-resources.json
  python3 sazare-core/summary-elements/generate.py profiles-resources.json > sazare-core/summary-elements/r4.tsv

One line per resource type: the type, then its top-level elements with
isSummary = true (as JSON keys: a choice `value[x]` is every `valueQuantity`,
`valueString`, ... it allows), tab-separated. `_summary=true` keeps these
(plus id, meta and the inherited summary elements, which are in every line).

Source: HL7 FHIR R4 (4.0.1), CC0-1.0 (see NOTICE.md).
"""
import json
import sys


def keys(path, types):
    name = path.split(".", 1)[1]
    if not name.endswith("[x]"):
        return [name]
    base = name[:-3]
    return [base + t["code"][0].upper() + t["code"][1:] for t in types]


def main(src):
    bundle = json.load(open(src))
    for entry in bundle["entry"]:
        sd = entry["resource"]
        if sd.get("resourceType") != "StructureDefinition" or sd.get("kind") != "resource" or sd.get("abstract"):
            continue
        rt = sd["type"]
        out = []
        for el in sd["snapshot"]["element"]:
            path = el["path"]
            if path.count(".") != 1 or not el.get("isSummary"):
                continue
            for k in keys(path, el.get("type", [])):
                if k not in out:
                    out.append(k)
        print("\t".join([rt] + out))


main(sys.argv[1])

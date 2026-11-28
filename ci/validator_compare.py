#!/usr/bin/env python3
"""Compare sazare's write checks with the HL7 FHIR validator, accept or refuse,
on the R4 examples and on copies broken on purpose.

  python3 ci/validator_compare.py corpus <r4-examples.ndjson> <dir>
      write the corpus: every example once, and for every 4th one copies
      with one thing broken (a date that doesn't exist, a dateTime without a
      zone, a required element left out, an empty string, an empty object,
      a uri with a space, a 64-bit integer, a code with a double space, a
      second type for a choice)
  java -jar validator_cli.jar <dir> -version 4.0.1 -tx n/a -locale en -output validator.json
  python3 ci/validator_compare.py compare <dir> validator.json <sazare base URL>
      PUT every file to sazare, compare, print a report; exit 1 when

  - sazare refuses what the validator accepts (sazare is wrong), but for
    KNOWN_SAZARE_ONLY below, each with its reason; or
  - a broken copy is accepted by sazare though the validator refuses it: a
    rule sazare says it checks was missed.

Examples the validator refuses and sazare accepts are counted and grouped by
the validator's message: what sazare doesn't check (FHIRPath invariants,
terminology, references) — reported, not failed.
"""
import collections
import copy
import json
import os
import re
import sys
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "..", "sazare-core", "structure", "r4.tsv")

# Definitional resources are left out: the validator loads them from the
# directory it validates as definitions of its own (a second
# StructureDefinition for a type makes it stop), and clients don't write them.
CONFORMANCE = {"StructureDefinition", "CodeSystem", "ValueSet", "ConceptMap", "SearchParameter",
               "OperationDefinition", "CapabilityStatement", "ImplementationGuide", "NamingSystem",
               "CompartmentDefinition", "StructureMap", "GraphDefinition", "MessageDefinition",
               "TerminologyCapabilities", "ExampleScenario"}

# Files sazare refuses and the validator accepts, on purpose: name → reason.
KNOWN_SAZARE_ONLY = {}


def load_table():
    t = {}
    for line in open(TABLE):
        cols = line.rstrip("\n").split("\t")
        path, mx, types = cols[0], cols[1], cols[2]
        t[path] = (mx, types, cols[3] if len(cols) > 3 else "0")
    return t


T = load_table()
DEFINED = {p.split(".")[0] for p in T}


def resolve(base, key):
    """(path, [types]) of element `key` under `base`, as sazare resolves it."""
    path = f"{base}.{key}"
    if path in T:
        types = T[path][1]
        if types.startswith("@"):
            target = types[1:]
            return target, T[target][1].split(",")
        return path, [x for x in types.split(",") if x]
    for i, c in enumerate(key):
        if i and c.isupper():
            stem, ty = key[:i], key[i:]
            choice = f"{base}.{stem}[x]"
            if choice in T:
                for t in T[choice][1].split(","):
                    if t[:1].upper() + t[1:] == ty:
                        return choice, [t]
    return None


PRIMITIVE = {"boolean", "integer", "positiveInt", "unsignedInt", "decimal", "string", "code", "id", "uri", "url",
             "canonical", "oid", "uuid", "markdown", "base64Binary", "date", "dateTime", "instant", "time", "xhtml"}


def walk(obj, base, root=True):
    """(holder, key, index, element path, type) for every value under `obj`
    (but the resource's id, which the corpus sets)."""
    for key, value in list(obj.items()):
        if key == "resourceType" or key.startswith("_") or (root and key == "id"):
            continue
        r = resolve(base, key)
        if not r:
            continue
        path, types = r
        items = list(enumerate(value)) if isinstance(value, list) else [(None, value)]
        for i, v in items:
            ty = types[0] if len(types) == 1 else None
            yield obj, key, i, path, ty
            if isinstance(v, dict) and ty and ty not in PRIMITIVE:
                if ty in ("BackboneElement", "Element"):
                    yield from walk(v, path, False)
                elif ty == "Resource":
                    rt = v.get("resourceType")
                    if rt in DEFINED:
                        yield from walk(v, rt)
                elif ty in DEFINED:
                    yield from walk(v, ty, False)


def get(holder, key, i):
    return holder[key] if i is None else holder[key][i]


def put(holder, key, i, value):
    if i is None:
        holder[key] = value
    else:
        holder[key][i] = value


def first(resource, pred):
    for holder, key, i, path, ty in walk(resource, resource["resourceType"]):
        if pred(get(holder, key, i), path, ty):
            return holder, key, i, path, ty
    return None


def mutations(resource):
    """(kind, broken copy) for what can be broken in `resource`."""
    out = []

    def apply(kind, pred, change):
        r = copy.deepcopy(resource)
        hit = first(r, pred)
        if hit:
            holder, key, i, path, ty = hit
            change(holder, key, i, path, ty, r)
            out.append((kind, r))

    s = lambda v: isinstance(v, str)
    apply("date-feb-30",
          lambda v, p, t: t in ("date", "dateTime") and s(v) and re.match(r"\d{4}-\d{2}-\d{2}", v),
          lambda h, k, i, p, t, r: put(h, k, i, get(h, k, i)[:4] + "-02-30" + get(h, k, i)[10:]))
    apply("dateTime-no-zone",
          lambda v, p, t: t == "dateTime" and s(v) and re.search(r"T\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$", v),
          lambda h, k, i, p, t, r: put(h, k, i, re.sub(r"(Z|[+-]\d\d:\d\d)$", "", get(h, k, i))))
    apply("empty-string",
          lambda v, p, t: t in ("string", "code", "uri") and s(v),
          lambda h, k, i, p, t, r: put(h, k, i, ""))
    apply("empty-object",
          lambda v, p, t: isinstance(v, dict) and t and t not in PRIMITIVE and t != "Resource",
          lambda h, k, i, p, t, r: put(h, k, i, {}))
    apply("uri-space",
          lambda v, p, t: t in ("uri", "url", "canonical") and s(v) and v,
          lambda h, k, i, p, t, r: put(h, k, i, get(h, k, i) + " x"))
    apply("integer-64-bit",
          lambda v, p, t: t in ("integer", "unsignedInt", "positiveInt") and isinstance(v, int),
          lambda h, k, i, p, t, r: put(h, k, i, 5000000000))
    apply("code-double-space",
          lambda v, p, t: t == "code" and s(v) and v,
          lambda h, k, i, p, t, r: put(h, k, i, get(h, k, i) + "  x"))

    # A required element of the resource left out.
    rt = resource["resourceType"]
    for path, (mx, types, mn) in T.items():
        if mn != "0" and path.startswith(rt + ".") and path.count(".") == 1:
            name = path.split(".")[1]
            if not name.endswith("[x]") and name in resource:
                r = copy.deepcopy(resource)
                del r[name]
                r.pop("_" + name, None)
                out.append(("required-left-out", r))
                break

    # A second type for a choice.
    for key in resource:
        res = resolve(rt, key)
        if res and res[0].endswith("[x]"):
            stem = res[0].rsplit(".", 1)[1][:-3]
            for alt, val in (("string", "x"), ("boolean", True), ("dateTime", "2026-01-01")):
                name = stem + alt[0].upper() + alt[1:]
                if alt in T[res[0]][1].split(",") and name not in resource:
                    r = copy.deepcopy(resource)
                    r[name] = val
                    out.append(("choice-two-types", r))
                    break
            break
    return out


def corpus(src, dst):
    os.makedirs(dst, exist_ok=True)
    seen, n, m = set(), 0, 0
    for line in open(src):
        r = json.loads(line)
        key = (r["resourceType"], r.get("id"))
        if key in seen or r["resourceType"] not in DEFINED or r["resourceType"] in CONFORMANCE:
            continue
        seen.add(key)
        rid = r.get("id") or f"x{n}"
        r["id"] = rid
        n += 1
        name = f"{n:05d}"
        json.dump(r, open(os.path.join(dst, name + ".json"), "w"), ensure_ascii=False)
        if n % 4:
            continue
        for kind, broken in mutations(r):
            broken["id"] = f"m{n}-{kind}"[:64]
            json.dump(broken, open(os.path.join(dst, f"{name}~{kind}.json"), "w"), ensure_ascii=False)
            m += 1
    print(f"{n} examples, {m} broken copies in {dst}")


def validator_results(path):
    out = {}
    b = json.load(open(path))
    entries = b.get("entry", [{"resource": b}])
    for e in entries:
        r = e.get("resource", e)
        f = next((x["valueString"] for x in r.get("extension", []) if x["url"].endswith("operationoutcome-file")), None)
        if not f:
            continue
        errors = [i for i in r.get("issue", []) if i["severity"] in ("error", "fatal")]
        out[os.path.basename(f)] = [(i.get("details", {}).get("text") or i.get("diagnostics") or "") for i in errors]
    return out


def sazare_put(base, resource):
    url = f"{base}/{resource['resourceType']}/{resource['id']}"
    req = urllib.request.Request(url, data=json.dumps(resource).encode(), method="PUT",
                                 headers={"Content-Type": "application/fhir+json"})
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, None
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        try:
            msg = json.loads(body)["issue"][0].get("diagnostics", "")
        except (ValueError, KeyError, IndexError):
            msg = body[:200]
        return e.code, msg


def bucket(msg):
    """A validator message without its particulars, for grouping."""
    m = re.sub(r"'[^']*'|\"[^\"]*\"|\([^)]*\)|\b\d[\d.:T+-]*\b", "…", msg)
    return m[:110]


def compare(dirname, vpath, base):
    v = validator_results(vpath)
    sazare_only, missed, gaps = [], [], collections.Counter()
    tally = collections.Counter()
    accepted = set()  # examples both accept: their broken copies are compared
    for name in sorted(os.listdir(dirname)):
        if not name.endswith(".json") or name not in v:
            continue
        broken = "~" in name
        if broken and name.split("~")[0] + ".json" not in accepted:
            continue  # refused already as it was: nothing to learn from breaking it
        resource = json.load(open(os.path.join(dirname, name)))
        status, msg = sazare_put(base, resource)
        s_ok, v_ok = 200 <= status < 300, not v[name]
        if not broken and s_ok and v_ok:
            accepted.add(name)
        kind = name.split("~")[1][:-5] if broken else "example"
        tally[(kind, s_ok, v_ok)] += 1
        if v_ok and not s_ok and name not in KNOWN_SAZARE_ONLY:
            sazare_only.append(f"{name} ({resource['resourceType']}/{resource['id']}): {status} {msg}")
        elif not v_ok and s_ok:
            if broken:
                missed.append(f"{name}: validator: {v[name][0]}")
            else:
                for m in set(map(bucket, v[name])):
                    gaps[m] += 1
    print("| kind | both accept | both refuse | only sazare refuses | only the validator refuses |")
    print("|---|---:|---:|---:|---:|")
    for kind in sorted({k for k, _, _ in tally}):
        row = [tally[(kind, True, True)], tally[(kind, False, False)], tally[(kind, False, True)], tally[(kind, True, False)]]
        print(f"| {kind} | " + " | ".join(map(str, row)) + " |")
    if gaps:
        print("\nExamples only the validator refuses, by message (what sazare doesn't check):\n")
        for m, n in gaps.most_common(25):
            print(f"- {n} × {m}")
    if sazare_only:
        print(f"\nsazare refuses what the validator accepts ({len(sazare_only)}):\n")
        print("\n".join(f"- {x}" for x in sazare_only[:200]))
    if missed:
        print(f"\nBroken copies sazare accepts ({len(missed)}):\n")
        print("\n".join(f"- {x}" for x in missed[:200]))
    return 1 if sazare_only or missed else 0


if __name__ == "__main__":
    if sys.argv[1] == "corpus":
        corpus(sys.argv[2], sys.argv[3])
    elif sys.argv[1] == "compare":
        sys.exit(compare(sys.argv[2], sys.argv[3], sys.argv[4].rstrip("/")))
    else:
        sys.exit(__doc__)

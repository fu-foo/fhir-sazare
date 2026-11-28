#!/usr/bin/env python3
"""Compare sazare's write checks with the HL7 FHIR validator, accept or refuse,
on the R4 examples and on copies broken on purpose, through every write path.

  python3 ci/validator_compare.py corpus <r4-examples.ndjson> <profiles-resources.json> <profiles-types.json> <dir>
      write the corpus: every example once (definitional resources left out),
      and for every 2nd one up to 16 KB copies with one thing broken each
  java -jar validator_cli.jar <dir> -version 4.0.1 -tx n/a -locale en -output validator.json
  python3 ci/validator_compare.py compare <dir> validator.json <sazare base URL> [baseline.json]
      send every file to sazare by all ten write paths — POST, PUT,
      conditional create and update, a transaction and a batch (a POST and a
      PUT entry each), PATCH, $import —; compare; print a report; exit 1 when

  - sazare refuses what the validator accepts (KNOWN_SAZARE_ONLY aside);
  - a broken copy is accepted by sazare though the validator refuses it: a
    rule sazare says it checks was missed;
  - the write paths disagree on a file (one stores what another refuses);
  - more examples than the baseline are refused by the validator alone (a
    ratchet: sazare checks less than it did). Fewer: the report says to lower
    the baseline.

Where to break and what type an element has come from the R4
StructureDefinitions, read here — not from sazare's own table, whose mistakes
would otherwise never be tried. The one judgment shared with sazare's
generator: a resource's `id` is an `id` (the R4 snapshots say `string`, the
spec says id).

What the validator can't judge is guarded by sazare's own tests instead:
conditional references (resolved on write; the validator sees one file) and
decimals kept as written (a round trip, not a verdict).
"""
import collections
import copy
import json
import os
import re
import sys
import urllib.error
import urllib.request

# Definitional resources are left out: the validator loads them from the
# directory it validates as definitions of its own (a second
# StructureDefinition for a type makes it stop), and clients don't write them.
CONFORMANCE = {"StructureDefinition", "CodeSystem", "ValueSet", "ConceptMap", "SearchParameter",
               "OperationDefinition", "CapabilityStatement", "ImplementationGuide", "NamingSystem",
               "CompartmentDefinition", "StructureMap", "GraphDefinition", "MessageDefinition",
               "TerminologyCapabilities", "ExampleScenario"}

# Not broken on purpose: a Parameters with a parameter left without its name
# makes the validator (7.0.8) stop on a NullPointerException, ending the run.
NOT_BROKEN = {"Parameters"}
# Broken copies are made of examples up to this size: the validator's time
# goes with the bytes (the large examples, broken 13 ways, took an hour).
BREAK_UP_TO = 16 * 1024

# Files sazare refuses and the validator accepts, on purpose: name → reason.
KNOWN_SAZARE_ONLY = {}

FHIR_TYPE = "http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type"
PRIMITIVE = {"boolean", "integer", "positiveInt", "unsignedInt", "decimal", "string", "code", "id", "uri", "url",
             "canonical", "oid", "uuid", "markdown", "base64Binary", "date", "dateTime", "instant", "time", "xhtml"}

T = {}  # path → (min, max, [types] or "@target")
DEFINED = set()


def load_definitions(*paths):
    """Every element of the R4 resources and data types, from the snapshots."""
    for p in paths:
        for entry in json.load(open(p))["entry"]:
            sd = entry["resource"]
            if sd.get("resourceType") != "StructureDefinition" or sd.get("derivation") == "constraint":
                continue
            if sd.get("kind") not in ("resource", "complex-type"):
                continue
            for el in sd["snapshot"]["element"]:
                path = el["path"]
                if "." not in path:
                    continue
                if "contentReference" in el:
                    types = "@" + el["contentReference"].lstrip("#")
                elif sd["kind"] == "resource" and path == sd["type"] + ".id":
                    types = ["id"]  # R4: a resource's id is an id
                else:
                    types = []
                    for t in el.get("type", []):
                        ext = next((x.get("valueUrl") for x in t.get("extension", []) if x.get("url") == FHIR_TYPE), None)
                        code = t["code"]
                        if code.startswith("http://hl7.org/fhirpath/System."):
                            code = code.rsplit(".", 1)[-1].lower()  # System.String → string
                        types.append(ext or code)
                T[path] = (int(el.get("min", 0)), el.get("max", "*"), types)
    DEFINED.update(p.split(".")[0] for p in T)


def resolve(base, key):
    """(path, [types]) of element `key` under `base`."""
    path = f"{base}.{key}"
    if path in T:
        types = T[path][2]
        if isinstance(types, str):
            target = types[1:]
            return target, T[target][2]
        return path, types
    for i, c in enumerate(key):
        if i and c.isupper():
            stem, ty = key[:i], key[i:]
            choice = f"{base}.{stem}[x]"
            if choice in T:
                for t in T[choice][2]:
                    if t[:1].upper() + t[1:] == ty:
                        return choice, [t]
    return None


def walk(obj, base, root=True):
    """(holder, key, index, element path, type) for every value under `obj`
    (but the resource's own id, which the corpus sets)."""
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
                    if v.get("resourceType") in DEFINED:
                        yield from walk(v, v["resourceType"], False)
                elif ty in DEFINED:
                    yield from walk(v, ty, False)


def get(holder, key, i):
    return holder[key] if i is None else holder[key][i]


def put(holder, key, i, value):
    if i is None:
        holder[key] = value
    else:
        holder[key][i] = value


def mutations(resource):
    """(kind, broken copy) for what can be broken in `resource`."""
    out = []

    def apply(kind, pred, change):
        r = copy.deepcopy(resource)
        for holder, key, i, path, ty in walk(r, r["resourceType"]):
            if pred(get(holder, key, i), path, ty):
                change(holder, key, i)
                out.append((kind, r))
                return

    s = lambda v: isinstance(v, str)
    apply("date-feb-30", lambda v, p, t: t in ("date", "dateTime") and s(v) and re.match(r"\d{4}-\d{2}-\d{2}", v),
          lambda h, k, i: put(h, k, i, get(h, k, i)[:4] + "-02-30" + get(h, k, i)[10:]))
    apply("dateTime-no-zone", lambda v, p, t: t == "dateTime" and s(v) and re.search(r"T\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$", v),
          lambda h, k, i: put(h, k, i, re.sub(r"(Z|[+-]\d\d:\d\d)$", "", get(h, k, i))))
    apply("empty-string", lambda v, p, t: t in ("string", "code", "uri") and s(v),
          lambda h, k, i: put(h, k, i, ""))
    apply("empty-object", lambda v, p, t: isinstance(v, dict) and t and t not in PRIMITIVE and t != "Resource",
          lambda h, k, i: put(h, k, i, {}))
    apply("uri-space", lambda v, p, t: t in ("uri", "url", "canonical") and s(v) and v,
          lambda h, k, i: put(h, k, i, get(h, k, i) + " x"))
    apply("extension-url-space", lambda v, p, t: p == "Extension.url" and s(v),
          lambda h, k, i: put(h, k, i, get(h, k, i) + " x"))
    apply("contained-id", lambda v, p, t: p.endswith(".id") and t == "id" and s(v),
          lambda h, k, i: put(h, k, i, "a b"))
    apply("integer-64-bit", lambda v, p, t: t in ("integer", "unsignedInt", "positiveInt") and isinstance(v, int),
          lambda h, k, i: put(h, k, i, 5000000000))
    apply("code-double-space", lambda v, p, t: t == "code" and s(v) and v,
          lambda h, k, i: put(h, k, i, get(h, k, i) + "  x"))
    # A narrative's div: what txt-1 refuses, as written to slip past a
    # string search.
    for kind, inner in (("narrative-script", "<scriptx>a</scriptx><script>alert(1)</script>"),
                        ("narrative-event-attribute", "<p\tonclick=\"x()\">a</p>"),
                        ("narrative-javascript-link", "<a href=\"javascript&#58;alert(1)\">a</a>"),
                        ("narrative-unknown-element", "<video>a</video>"),
                        ("narrative-not-well-formed", "<p>a</b>"),
                        # The validator's structural rules, and what it may
                        # accept that a narrow reading would refuse: which is
                        # stricter, the comparison decides.
                        ("narrative-block-in-paragraph", "<p><div>a</div></p>"),
                        ("narrative-li-outside-list", "<li>a</li>"),
                        ("narrative-link-in-link", "<a href=\"#x\"><a href=\"#y\">a</a></a>"),
                        ("narrative-duplicate-id", "<p id=\"d1\">a</p><p id=\"d1\">b</p>"),
                        ("narrative-pre-xml-space", "<pre xml:space=\"preserve\">a</pre>"),
                        ("narrative-html-entity", "<p>a&ensp;b&hearts;c&thetasym;</p>"),
                        ("narrative-self-nesting", "<b>a<span><b>b</b></span></b>"),
                        ("narrative-content-in-br", "<p>a<br>b</br></p>"),
                        ("narrative-colgroup-p", "<table><colgroup><p>a</p></colgroup><tr><td>b</td></tr></table>"),
                        ("narrative-thead-td", "<table><thead><td>a</td></thead></table>")):
        div = resource.get("text", {}).get("div") if isinstance(resource.get("text"), dict) else None
        if isinstance(div, str) and ">" in div:
            r = copy.deepcopy(resource)
            at = div.index(">") + 1
            r["text"]["div"] = div[:at] + inner + div[at:]
            out.append((kind, r))
    apply("extension-value-and-extensions",
          lambda v, p, t: t == "Extension" and isinstance(v, dict) and any(k.startswith("value") for k in v),
          lambda h, k, i: get(h, k, i).setdefault("extension", []).append({"url": "http://example.org/x", "valueString": "y"}))

    # A required element left out: of the resource, and of something inside it.
    for kind in ("required-left-out", "required-left-out-nested"):
        r = copy.deepcopy(resource)
        if kind == "required-left-out":
            targets = [(r, r["resourceType"])]
        else:
            targets = [(get(h, k, i), p if t in ("BackboneElement", "Element") else t)
                       for h, k, i, p, t in walk(r, r["resourceType"])
                       if isinstance(get(h, k, i), dict) and t and t not in PRIMITIVE and t != "Resource"]
        found = False
        for holder, base in targets:
            for key in list(holder):
                path = f"{base}.{key}"
                if path in T and T[path][0] >= 1:
                    del holder[key]
                    holder.pop("_" + key, None)
                    out.append((kind, r))
                    found = True
                    break
            if found:
                break

    # A second type for a choice.
    rt = resource["resourceType"]
    for key in resource:
        res = resolve(rt, key)
        if res and res[0].endswith("[x]"):
            stem = res[0].rsplit(".", 1)[1][:-3]
            for alt, val in (("string", "x"), ("boolean", True), ("dateTime", "2026-01-01")):
                name = stem + alt[0].upper() + alt[1:]
                if alt in T[res[0]][2] and name not in resource:
                    r = copy.deepcopy(resource)
                    r[name] = val
                    out.append(("choice-two-types", r))
                    break
            break
    return out


# The txt-1 elements (the validator's list): every one inside every one, one
# level, as narratives of their own — so the comparison covers the structure
# rules for every pair, not the pairs someone thought of.
NARRATIVE_ELEMENTS = [
    "p", "br", "div", "h1", "h2", "h3", "h4", "h5", "h6", "a", "span", "b", "em", "i", "strong", "small", "big", "tt",
    "dfn", "q", "var", "abbr", "acronym", "cite", "blockquote", "hr", "address", "bdo", "kbd", "sub", "sup", "ul", "ol",
    "li", "dl", "dt", "dd", "pre", "table", "caption", "colgroup", "col", "thead", "tr", "tfoot", "tbody", "th", "td",
    "code", "samp", "img", "map", "area",
]


def element(name, inner=""):
    attrs = {"img": ' src="x.png" alt="x"', "area": ' alt="x"', "map": ' name="m"', "a": ' name="n"'}.get(name, "")
    return f"<{name}{attrs}>{inner}</{name}>"


def element_pairs(dst, start):
    """A Basic for each (outer, inner) pair of narrative elements."""
    n = start
    for outer in NARRATIVE_ELEMENTS:
        for inner in NARRATIVE_ELEMENTS:
            n += 1
            div = f'<div xmlns="http://www.w3.org/1999/xhtml"><p>text</p>{element(outer, "a" + element(inner, "b"))}</div>'
            r = {"resourceType": "Basic", "id": f"pair{n}", "code": {"text": "x"}, "text": {"status": "generated", "div": div}}
            json.dump(r, open(os.path.join(dst, f"pairs-{outer}-{inner}.json"), "w"))
    # And each one empty (`<ul></ul>`, `<table></table>`, `<br/>`).
    for name in NARRATIVE_ELEMENTS:
        n += 1
        div = f'<div xmlns="http://www.w3.org/1999/xhtml"><p>text</p>{element(name)}</div>'
        r = {"resourceType": "Basic", "id": f"pair{n}", "code": {"text": "x"}, "text": {"status": "generated", "div": div}}
        json.dump(r, open(os.path.join(dst, f"pairs-{name}-empty.json"), "w"))
    return n - start


def corpus(src, resources, types, dst):
    load_definitions(resources, types)
    os.makedirs(dst, exist_ok=True)
    seen, n, m = set(), 0, 0
    for line in open(src):
        r = json.loads(line)
        key = (r["resourceType"], r.get("id"))
        if key in seen or r["resourceType"] not in DEFINED or r["resourceType"] in CONFORMANCE:
            continue
        seen.add(key)
        n += 1
        r["id"] = f"e{n}"
        name = f"{n:05d}"
        json.dump(r, open(os.path.join(dst, name + ".json"), "w"), ensure_ascii=False)
        if n % 2 or r["resourceType"] in NOT_BROKEN or len(line) > BREAK_UP_TO:
            continue
        for kind, broken in mutations(r):
            json.dump(broken, open(os.path.join(dst, f"{name}~{kind}.json"), "w"), ensure_ascii=False)
            m += 1
    pairs = element_pairs(dst, n)
    print(f"{n} examples, {m} broken copies, {pairs} element pairs in {dst}")


def validator_results(path):
    out = {}
    b = json.load(open(path))
    for e in b.get("entry", [{"resource": b}]):
        r = e.get("resource", e)
        f = next((x["valueString"] for x in r.get("extension", []) if x["url"].endswith("operationoutcome-file")), None)
        if f:
            errors = [i for i in r.get("issue", []) if i["severity"] in ("error", "fatal")]
            out[os.path.basename(f)] = [(i.get("details", {}).get("text") or i.get("diagnostics") or "") for i in errors]
    return out


def request(base, method, path, body, content_type="application/fhir+json", extra=None, raw=None):
    data = raw.encode() if raw is not None else json.dumps(body).encode()
    req = urllib.request.Request(f"{base}{path}", data=data, method=method,
                                 headers={"Content-Type": content_type, **(extra or {})})
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read())
        except ValueError:
            return e.code, None


def message(body):
    try:
        return body["issue"][0].get("diagnostics", "")
    except (TypeError, KeyError, IndexError):
        return ""


def write_paths(base, resource, original, tag):
    """Each write path's verdict: (stored, status and message)."""
    rt = resource["resourceType"]
    out = {}

    def with_id(r, suffix):
        r = copy.deepcopy(r)
        r["id"] = f"{tag}-{suffix}"[:64]
        return r

    def verdict(s, b):
        return (200 <= s < 300, f"{s} {message(b)}")

    out["POST"] = verdict(*request(base, "POST", f"/{rt}", resource))
    r = with_id(resource, "pu")
    out["PUT"] = verdict(*request(base, "PUT", f"/{rt}/{r['id']}", r))
    # Conditional create (nothing matches) and conditional update (creates).
    r = with_id(resource, "cc")
    out["conditional create"] = verdict(*request(base, "POST", f"/{rt}", r, extra={"If-None-Exist": f"_id={r['id']}"}))
    r = with_id(resource, "cu")
    out["conditional update"] = verdict(*request(base, "PUT", f"/{rt}?_id={r['id']}", r))
    for kind, short in (("transaction", "tx"), ("batch", "ba")):
        for method in ("POST", "PUT"):
            r = with_id(resource, f"{short}{method[1].lower()}")
            url = rt if method == "POST" else f"{rt}/{r['id']}"
            bundle = {"resourceType": "Bundle", "type": kind, "entry": [{"resource": r, "request": {"method": method, "url": url}}]}
            s, b = request(base, "POST", "", bundle)
            if kind == "batch" and s == 200:
                response = b["entry"][0]["response"]
                out[f"batch {method}"] = (response["status"].startswith("2"), f"{response['status']} {message(response.get('outcome'))}")
            else:
                out[f"{kind} {method}"] = verdict(s, b)
    # PATCH: the original stored, then replaced by this content.
    r = with_id(resource, "pa")
    s0, _ = request(base, "PUT", f"/{rt}/{r['id']}", with_id(original, "pa"))
    if 200 <= s0 < 300:
        out["PATCH"] = verdict(*request(base, "PATCH", f"/{rt}/{r['id']}", [{"op": "replace", "path": "", "value": r}],
                                        "application/json-patch+json"))
    # $import: one line; its outcome counts what was imported.
    r = with_id(resource, "im")
    s, b = request(base, "POST", "/$import", None, "application/fhir+ndjson", raw=json.dumps(r) + "\n")
    imported = 0
    if isinstance(b, dict):
        for ext in b.get("extension", []):
            for x in ext.get("extension", []):
                if x.get("url") == "created":
                    imported = x.get("valueInteger", 0)
    out["$import"] = (s == 200 and imported == 1, f"{s} {next((i.get('diagnostics') for i in (b or {}).get('issue', [])[1:2]), '')}")
    return out


def bucket(msg):
    """A validator message without its particulars, for grouping."""
    return re.sub(r"'[^']*'|\"[^\"]*\"|\([^)]*\)|\b\d[\d.:T+-]*\b", "…", msg)[:110]


def compare(dirname, vpath, base, baseline_path=None):
    v = validator_results(vpath)
    sazare_only, missed, disagree, gaps = [], [], [], collections.Counter()
    tally = collections.Counter()
    accepted = {}  # examples every path and the validator accept: their broken copies are compared
    only_validator_examples = 0
    for name in sorted(os.listdir(dirname)):
        if not name.endswith(".json") or name not in v:
            continue
        pair = name.startswith("pairs-")
        broken = "~" in name
        original_name = name.split("~")[0] + ".json"
        if broken and original_name not in accepted:
            continue  # refused already as it was: nothing to learn from breaking it
        resource = json.load(open(os.path.join(dirname, name)))
        paths = write_paths(base, resource, accepted.get(original_name, resource), name[:-5].replace("~", "-"))
        verdicts = {ok for ok, _ in paths.values()}
        if len(verdicts) > 1:
            disagree.append(f"{name}: " + "; ".join(f"{p} {'stored' if ok else 'refused ' + m}" for p, (ok, m) in paths.items()))
            continue
        s_ok, v_ok = verdicts.pop(), not v[name]
        refusal = next((m for ok, m in paths.values() if not ok), "")
        if not broken and not pair and s_ok and v_ok:
            accepted[name] = resource
        kind = "element pairs" if pair else name.split("~")[1][:-5] if broken else "example"
        tally[(kind, s_ok, v_ok)] += 1
        if v_ok and not s_ok and name not in KNOWN_SAZARE_ONLY:
            sazare_only.append(f"{name} ({resource['resourceType']}): {refusal}")
        elif not v_ok and s_ok:
            if broken or pair:
                missed.append(f"{name}: validator: {v[name][0]}")
            else:
                only_validator_examples += 1
                for m in set(map(bucket, v[name])):
                    gaps[m] += 1
    print("Each file was sent by all ten write paths (POST, PUT, conditional create and update, transaction and batch")
    print("POST and PUT entries, PATCH, $import); they agreed on each verdict below.\n")
    print("| kind | both accept | both refuse | only sazare refuses | only the validator refuses |")
    print("|---|---:|---:|---:|---:|")
    for kind in sorted({k for k, _, _ in tally}):
        row = [tally[(kind, True, True)], tally[(kind, False, False)], tally[(kind, False, True)], tally[(kind, True, False)]]
        print(f"| {kind} | " + " | ".join(map(str, row)) + " |")
    failed = bool(sazare_only or missed or disagree)
    if baseline_path:
        baseline = json.load(open(baseline_path))["only_validator_examples"]
        if only_validator_examples > baseline:
            print(f"\nThe validator alone refuses {only_validator_examples} examples, more than the baseline {baseline}: sazare checks less than it did.")
            failed = True
        elif only_validator_examples < baseline:
            print(f"\nThe validator alone refuses {only_validator_examples} examples, fewer than the baseline {baseline}: lower it in {baseline_path}.")
    if gaps:
        print("\nExamples only the validator refuses, by message (what sazare doesn't check):\n")
        for m, n in gaps.most_common(25):
            print(f"- {n} × {m}")
    for title, items in (("sazare refuses what the validator accepts", sazare_only),
                         ("Broken copies sazare accepts", missed),
                         ("The write paths disagree", disagree)):
        if items:
            print(f"\n{title} ({len(items)}):\n")
            print("\n".join(f"- {x}" for x in items[:200]))
    return 1 if failed else 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["corpus"] and len(sys.argv) == 6:
        corpus(*sys.argv[2:6])
    elif sys.argv[1:2] == ["compare"] and len(sys.argv) in (5, 6):
        sys.exit(compare(sys.argv[2], sys.argv[3], sys.argv[4].rstrip("/"), sys.argv[5] if len(sys.argv) == 6 else None))
    else:
        sys.exit(__doc__)

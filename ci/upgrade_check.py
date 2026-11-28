#!/usr/bin/env python3
"""Upgrade check: a data dir written by an older release, opened by this one.

  ci/upgrade_check.py OLD_BIN NEW_BIN [patients]
  OLD_CLI=legacy for a release before 0.15 (no `run` command).

1. OLD_BIN: import a synthetic dataset, record the totals and id sets of a set
   of searches.
2. NEW_BIN on the same data dir: the old index is rebuilt in the background
   (a new index generation). Searches answered meanwhile — by the old index —
   must match step 1; a resource written meanwhile must be searchable after
   the switch; after the switch every search must still match, and the old
   index files must be gone.
3. Before NEW_BIN starts: its `check` reads the old store without moving its
   schema (read-only, no migration). After: the per-type counts it keeps
   (store v4) match the old searches' totals, and the history shows a version
   the old release made by PUT as inferred (POST) and one this release makes
   by PUT as recorded (PUT).
"""
import atexit
import json
import sqlite3
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

OLD_BIN, NEW_BIN = sys.argv[1], sys.argv[2]
PATIENTS = int(sys.argv[3]) if len(sys.argv) > 3 else 2000
PORT = int(os.environ.get("PORT", "18090"))
BASE = f"http://127.0.0.1:{PORT}"
HERE = os.path.dirname(os.path.abspath(__file__))

QUERIES = [
    "Patient",
    "Patient?family=Smith",
    "Patient?family:exact=Smith",
    "Patient?gender=female&birthdate=ge1980",
    "Patient?identifier=urn:bench|MRN42",
    "Observation?subject=Patient/p42",
    "Observation?patient=p42&code=http://loinc.org|8867-4",
    "Observation?code=8867-4&date=ge2020-01-01",
    "Observation?category=vital-signs&status=final",
    "Observation?subject:Patient.family=Smith",
    "Patient?_has:Observation:subject:code=8867-4",
    "Observation?_content=8867-4",
    "Patient?gender:not=female",
    "Observation?date=lt2019",
]


def get(path):
    with urllib.request.urlopen(f"{BASE}/{path}", timeout=600) as r:
        return json.load(r)


def index_state():
    return get("health").get("index", {}).get("state")


def read_all(q):
    sep = "&" if "?" in q else "?"
    ids, url = set(), f"{q}{sep}_count=1000"
    total = None
    while url:
        b = get(url)
        total = b.get("total") if total is None else total
        ids.update(e["resource"]["id"] for e in b.get("entry", []) if e.get("search", {}).get("mode", "match") == "match")
        nxt = [l["url"] for l in b.get("link", []) if l["relation"] == "next"]
        if nxt:
            u = urllib.parse.urlsplit(nxt[0])
            url = u.path.lstrip("/") + "?" + u.query
        else:
            url = None
    return total, ids


def snapshot():
    out = {}
    for q in QUERIES:
        # Pages read across the switch to the new index come from two indexes
        # whose order of equal results differs: read such a search again.
        while True:
            state = index_state()
            out[q] = read_all(q)
            if index_state() == state:
                break
            print(f"  {q}: the index switched while paging; read again")
    return out


RUNNING = []
atexit.register(lambda: [p.kill() for p in RUNNING if p.poll() is None])


def start(binary, cfg, log):
    try:
        get("health")
        sys.exit(f"something already answers on port {PORT}")
    except OSError:
        pass
    # Before 0.15 the server ran with no command: `sazare-server --config <file>`.
    legacy = binary == OLD_BIN and os.environ.get("OLD_CLI") == "legacy"
    args = [binary, "--config", cfg] if legacy else [binary, "run", "--config", cfg]
    p = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT)
    RUNNING.append(p)
    for _ in range(300):
        if p.poll() is not None:
            sys.exit(f"{binary} exited; see {log.name}")
        try:
            get("health")
            return p
        except OSError:
            time.sleep(0.1)
    sys.exit(f"{binary} did not start")


def stop(p):
    p.terminate()
    p.wait(timeout=120)


# Searches the old index may answer differently during the rebuild (its
# format predates them): e.g. from 0.14 or before, `:exact` (case-sensitive
# from 0.15, the old index stored strings lower-cased). Comma-separated.
TRANSIENT = [q for q in os.environ.get("DURING_REBUILD_MAY_DIFFER", "").split(",") if q]


def compare(label, want, got, allowed=()):
    bad = [q for q in QUERIES if want[q] != got[q] and q not in allowed]
    for q in allowed:
        if want[q] != got[q]:
            print(f"  {label}: {q}: differs, as expected ({want[q][0]} -> {got[q][0]})")
    for q in bad:
        (wt, wi), (gt, gi) = want[q], got[q]
        print(f"  {label}: {q}: total {wt} -> {gt}; missing {sorted(wi - gi)[:5]}, extra {sorted(gi - wi)[:5]}")
    if bad:
        sys.exit(f"{label}: {len(bad)} searches differ")
    print(f"{label}: {len(QUERIES)} searches match")


work = tempfile.mkdtemp()
cfg = os.path.join(work, "config.yaml")
with open(cfg, "w") as f:
    f.write(f"server:\n  host: 127.0.0.1\n  port: {PORT}\nauth:\n  enabled: false\n"
            f"storage:\n  data_dir: {work}/data\nlog:\n  level: info\n")
log = open(os.path.join(work, "server.log"), "a")

old = start(OLD_BIN, cfg, log)
data = subprocess.run([sys.executable, os.path.join(HERE, "..", "bench", "gen_data.py"), str(PATIENTS), "5"],
                      capture_output=True, check=True).stdout
lines = data.splitlines(keepends=True)
for i in range(0, len(lines), 5000):
    req = urllib.request.Request(f"{BASE}/$import", data=b"".join(lines[i:i + 5000]), method="POST",
                                 headers={"Content-Type": "application/fhir+ndjson"})
    urllib.request.urlopen(req, timeout=600).read()
# A resource the old release creates by PUT: its history names no method.
put_req = urllib.request.Request(f"{BASE}/Patient/old-put", method="PUT",
                                 data=json.dumps({"resourceType": "Patient", "id": "old-put"}).encode(),
                                 headers={"Content-Type": "application/fhir+json"})
urllib.request.urlopen(put_req, timeout=60).read()
before = snapshot()
stop(old)
print(f"old release: {len(lines)} resources imported")


def store_schema():
    db = sqlite3.connect(f"file:{work}/data/resources.sqlite?mode=ro", uri=True)
    try:
        return db.execute("PRAGMA user_version").fetchone()[0]
    finally:
        db.close()


# `check` with the new binary on the old store: read-only, no migration.
schema_before = store_schema()
checked = subprocess.run([NEW_BIN, "check", "--config", cfg], capture_output=True, text=True)
print(f"check on the old store: exit {checked.returncode}; {checked.stderr.strip().splitlines()[-1:]}")
if store_schema() != schema_before:
    sys.exit(f"check moved the store schema: {schema_before} -> {store_schema()}")

new = start(NEW_BIN, cfg, log)
state = get("health").get("index", {}).get("state")
print(f"new release up; index state: {state}")
# From a release with the same index format there is nothing to rebuild.
rebuilt = state == "rebuilding"
during = snapshot()
compare("during the rebuild", before, during, TRANSIENT)


def status(path):
    try:
        with urllib.request.urlopen(f"{BASE}/{path}", timeout=600) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


# Quantities: an old index without them (before 0.17) answers 503 while it
# serves; one with them (OLD_HAS_QUANTITIES=1) answers.
want_quantity = 200 if os.environ.get("OLD_HAS_QUANTITIES") == "1" else 503
if get("health").get("index", {}).get("state") == "rebuilding":
    for q in ("Observation?value-quantity=gt0", "Observation?_sort=-value-quantity"):
        code = status(q)
        if code != want_quantity and get("health").get("index", {}).get("state") == "rebuilding":
            sys.exit(f"{q} during the rebuild answered {code}, not {want_quantity}")
        print(f"{q} during the rebuild: {code}")
# A write while the rebuild runs: replayed into the new index.
req = urllib.request.Request(f"{BASE}/Patient/upgrade-check", method="PUT",
                             data=json.dumps({"resourceType": "Patient", "id": "upgrade-check",
                                              "name": [{"family": "Upgradecheck"}]}).encode(),
                             headers={"Content-Type": "application/fhir+json"})
urllib.request.urlopen(req, timeout=60).read()
for _ in range(6000):
    if get("health").get("index", {}).get("state") == "ready":
        break
    time.sleep(0.1)
else:
    sys.exit("index never became ready")
after = snapshot()
# The resource written during the rebuild aside (it matches some searches).
after = {q: (t - (1 if "upgrade-check" in ids else 0), ids - {"upgrade-check"}) for q, (t, ids) in after.items()}
compare("after the switch", before, after)
if status("Observation?value-quantity=gt0") != 200:
    sys.exit("a quantity search after the switch failed")
if get("Patient?family=Upgradecheck").get("total") != 1:
    sys.exit("the write made during the rebuild is not searchable")
# The counts kept per type (store v4) against the old release's totals (the
# two resources written around the upgrade are Patients).
counts = {c["type"]: c["count"] for c in get("$status")["resourceCounts"]}
want_patients = before["Patient"][0] + 1  # upgrade-check
if counts.get("Patient") != want_patients:
    sys.exit(f"kept Patient count {counts.get('Patient')}, searched {want_patients}")
observations = get("Observation?_summary=count").get("total")
if counts.get("Observation") != observations:
    sys.exit(f"kept Observation count {counts.get('Observation')}, searched {observations}")
print(f"kept counts match the searches: {counts}")
# History methods: inferred for the old release's PUT, recorded for this one's.
old_method = get("Patient/old-put/_history")["entry"][-1]["request"]["method"]
urllib.request.urlopen(urllib.request.Request(f"{BASE}/Patient/new-put", method="PUT",
                       data=json.dumps({"resourceType": "Patient", "id": "new-put"}).encode(),
                       headers={"Content-Type": "application/fhir+json"}), timeout=60).read()
new_method = get("Patient/new-put/_history")["entry"][-1]["request"]["method"]
if (old_method, new_method) != ("POST", "PUT"):
    sys.exit(f"history methods: old release's PUT shows {old_method} (want POST, inferred), this one's {new_method} (want PUT)")
print("history: the old release's PUT inferred as POST, this release's recorded as PUT")
files = sorted(os.listdir(os.path.join(work, "data")))
if rebuilt and "search_index.sqlite" in files:
    sys.exit(f"old index files still there: {files}")
stop(new)
print(f"index files: {[f for f in files if f.startswith('search_index')]}")
print("upgrade check OK")

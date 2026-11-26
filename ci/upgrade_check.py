#!/usr/bin/env python3
"""Upgrade check: a data dir written by an older release, opened by this one.

  ci/upgrade_check.py OLD_BIN NEW_BIN [patients]

1. OLD_BIN: import a synthetic dataset, record the totals and id sets of a set
   of searches.
2. NEW_BIN on the same data dir: the old index is rebuilt in the background
   (a new index generation). Searches answered meanwhile — by the old index —
   must match step 1; a resource written meanwhile must be searchable after
   the switch; after the switch every search must still match, and the old
   index files must be gone.
"""
import atexit
import json
import os
import subprocess
import sys
import tempfile
import time
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


def snapshot():
    out = {}
    for q in QUERIES:
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
        out[q] = (total, ids)
    return out


RUNNING = []
atexit.register(lambda: [p.kill() for p in RUNNING if p.poll() is None])


def start(binary, cfg, log):
    try:
        get("health")
        sys.exit(f"something already answers on port {PORT}")
    except OSError:
        pass
    p = subprocess.Popen([binary, "run", "--config", cfg], stdout=log, stderr=subprocess.STDOUT)
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


def compare(label, want, got):
    bad = [q for q in QUERIES if want[q] != got[q]]
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
before = snapshot()
stop(old)
print(f"old release: {len(lines)} resources imported")

new = start(NEW_BIN, cfg, log)
state = get("health").get("index", {}).get("state")
print(f"new release up; index state: {state}")
during = snapshot()
compare("during the rebuild", before, during)
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
if get("Patient?family=Upgradecheck").get("total") != 1:
    sys.exit("the write made during the rebuild is not searchable")
files = sorted(os.listdir(os.path.join(work, "data")))
if "search_index.sqlite" in files:
    sys.exit(f"old index files still there: {files}")
stop(new)
print(f"index files: {[f for f in files if f.startswith('search_index')]}")
print("upgrade check OK")

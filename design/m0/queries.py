#!/usr/bin/env python3
"""Milestone 0: the target queries as hand-written SQL on the v2 prototype.

Each query is what the v2 executor would generate (design §4.3): a driving set,
EXISTS probes, a streamed page, and an exact total. Prints the median of 3 runs and
the query plans.

usage: python3 -I queries.py <v2.sqlite>
"""
import sqlite3
import statistics
import sys
import time

db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
db.execute("PRAGMA cache_size = -1000000")
q1 = lambda sql, *a: db.execute(sql, a).fetchone()[0]

tid = {n: t for t, n in db.execute("SELECT tid, name FROM rtype")}
pid = {(tid_, n): p for p, tid_, n in db.execute("SELECT pid, tid, name FROM param")}
P = lambda t, n: pid[(tid[t], n)]
sid = {u: s for s, u in db.execute("SELECT sid, uri FROM csystem")}
LOINC = sid["http://loinc.org"]
CAT = sid["http://terminology.hl7.org/CodeSystem/observation-category"]
rid_of = lambda t, i: q1("SELECT rid FROM rkey WHERE tid = ? AND id = ?", tid[t], i)
P42 = rid_of("Patient", "p42")
OBS, PAT = tid["Observation"], tid["Patient"]
# 2020-01-01T00:00:00Z in µs
D2020 = 1577836800 * 1_000_000
D1980 = 315532800 * 1_000_000


def page(cur, n=20, offset=0):
    """Distinct rids in order of first occurrence, stopping at the page."""
    seen, out = set(), []
    for (rid,) in cur:
        if rid in seen:
            continue
        seen.add(rid)
        if len(seen) > offset:
            out.append(rid)
            if len(out) == n:
                break
    return out


def run(label, fn):
    times, result = [], None
    for _ in range(3):
        t = time.perf_counter()
        result = fn()
        times.append((time.perf_counter() - t) * 1000)
    print(f"{statistics.median(times):10.1f} ms  {str(result)[:60]:<60}  {label}", flush=True)


def plan(sql, *a):
    for row in db.execute("EXPLAIN QUERY PLAN " + sql, a):
        print("        plan:", row[3])


# ---- Q: Observation?_count=20 (unfiltered list) ---------------------------------
LIST = "SELECT rid FROM rkey INDEXED BY rkey_live WHERE tid = ? AND present = 1 ORDER BY rid"
run("Observation?_count=20", lambda: (page(db.execute(LIST, (OBS,))),
                                      q1("SELECT n FROM type_count WHERE tid = ?", OBS)))

# ---- Q: Observation?_sort=-date&_count=20 ----------------------------------------
SORT = "SELECT rid FROM v_date WHERE pid = ? ORDER BY lo DESC"
run("Observation?_sort=-date&_count=20",
    lambda: (page(db.execute(SORT, (P("Observation", "date"),))),
             q1("SELECT n FROM type_count WHERE tid = ?", OBS)))
plan(SORT, P("Observation", "date"))

# ---- Q: Observation?code=8867-4&_sort=-date&_count=20 -----------------------------
CODE_SORT = """SELECT d.rid FROM v_date d WHERE d.pid = ? AND EXISTS
                 (SELECT 1 FROM v_token t INDEXED BY v_token_r WHERE t.rid = d.rid AND t.pid = ? AND t.code = ?)
               ORDER BY d.lo DESC"""
CODE_COUNT = "SELECT count(DISTINCT rid) FROM v_token WHERE pid = ? AND code = ?"
run("Observation?code=8867-4&_sort=-date  (page)",
    lambda: page(db.execute(CODE_SORT, (P("Observation", "date"), P("Observation", "code"), "8867-4"))))
run("   … total", lambda: q1(CODE_COUNT, P("Observation", "code"), "8867-4"))
plan(CODE_SORT, P("Observation", "date"), P("Observation", "code"), "8867-4")

# ---- Q: Patient?_has:Observation:subject:code=8867-4&_count=20 ---------------------
HAS_PAGE = """SELECT k.rid FROM rkey k INDEXED BY rkey_live WHERE k.tid = ? AND k.present = 1
               AND EXISTS (SELECT 1 FROM v_ref r WHERE r.target = k.rid AND r.pid = ?
                             AND EXISTS (SELECT 1 FROM v_token t INDEXED BY v_token_r WHERE t.rid = r.rid AND t.pid = ? AND t.code = ?))
              ORDER BY k.rid"""
HAS_COUNT = """SELECT count(DISTINCT r.target) FROM v_token t
               CROSS JOIN v_ref r INDEXED BY v_ref_r ON r.rid = t.rid AND r.pid = ?
               CROSS JOIN rkey k ON k.rid = r.target AND k.tid = ? AND k.present = 1
               WHERE t.pid = ? AND t.code = ?"""
a = (PAT, P("Observation", "subject"), P("Observation", "code"), "8867-4")
run("Patient?_has:Observation:subject:code=8867-4  (page)", lambda: page(db.execute(HAS_PAGE, a)))
run("   … total", lambda: q1(HAS_COUNT, P("Observation", "subject"), PAT, P("Observation", "code"), "8867-4"))
plan(HAS_COUNT, P("Observation", "subject"), PAT, P("Observation", "code"), "8867-4")

# ---- Q: Observation?category=vital-signs&status=final&_count=20 --------------------
CS_PAGE = """SELECT t.rid FROM v_token t WHERE t.pid = ? AND t.code = ? AND t.sid = ?
               AND EXISTS (SELECT 1 FROM v_token s INDEXED BY v_token_r WHERE s.rid = t.rid AND s.pid = ? AND s.code = ?)
             ORDER BY t.rid"""
CS_COUNT = """SELECT count(*) FROM (SELECT rid FROM v_token WHERE pid = ? AND code = ? AND sid = ?
                                     INTERSECT
                                     SELECT rid FROM v_token WHERE pid = ? AND code = ?)"""
b = (P("Observation", "category"), "vital-signs", CAT, P("Observation", "status"), "final")
run("Observation?category=vital-signs&status=final  (page)", lambda: page(db.execute(CS_PAGE, b)))
run("   … total (INTERSECT)", lambda: q1(CS_COUNT, *b))
CS_COUNT2 = """SELECT count(*) FROM v_token t WHERE t.pid = ? AND t.code = ? AND t.sid = ?
                 AND EXISTS (SELECT 1 FROM v_token s INDEXED BY v_token_r WHERE s.rid = t.rid AND s.pid = ? AND s.code = ?)"""
run("   … total (EXISTS)", lambda: q1(CS_COUNT2, *b))

# ---- Q: Observation?patient=p42&code=…  (+ date) -----------------------------------
PC = """SELECT r.rid FROM v_ref r WHERE r.pid = ? AND r.target = ?
          AND EXISTS (SELECT 1 FROM v_token t INDEXED BY v_token_r WHERE t.rid = r.rid AND t.pid = ? AND t.code = ? AND t.sid = ?)
        ORDER BY r.rid"""
c = (P("Observation", "patient"), P42, P("Observation", "code"), "8867-4", LOINC)
run("Observation?patient=p42&code=loinc|8867-4", lambda: len(page(db.execute(PC, c), 100)))
PCD = PC.replace("ORDER BY", "AND EXISTS (SELECT 1 FROM v_date d INDEXED BY v_date_r WHERE d.rid = r.rid AND d.pid = ? AND d.hi > ?) ORDER BY")
run("   … &date=ge2020-01-01", lambda: len(page(db.execute(PCD, c + (P("Observation", "date"), D2020)), 100)))

# ---- Q: Observation?code=8867-4&date=ge2020-01-01&_count=20 (both large) ------------
CD_PAGE = """SELECT t.rid FROM v_token t WHERE t.pid = ? AND t.code = ?
               AND EXISTS (SELECT 1 FROM v_date d INDEXED BY v_date_r WHERE d.rid = t.rid AND d.pid = ? AND d.hi > ?)
             ORDER BY t.rid"""
d = (P("Observation", "code"), "8867-4", P("Observation", "date"), D2020)
run("Observation?code=8867-4&date=ge2020  (page)", lambda: page(db.execute(CD_PAGE, d)))
run("   … total", lambda: q1(CD_PAGE.replace("SELECT t.rid", "SELECT count(DISTINCT t.rid)").replace("ORDER BY t.rid", ""), *d))
DATE_GE = "SELECT count(DISTINCT rid) FROM v_date INDEXED BY v_date_h WHERE pid = ? AND hi > ?"
run("date=ge2020 alone (count, via v_date_h)", lambda: q1(DATE_GE, P("Observation", "date"), D2020))
plan(DATE_GE, P("Observation", "date"), D2020)

# ---- Q: Patient?gender=female&birthdate=ge1980&_count=20 ----------------------------
GB = """SELECT t.rid FROM v_token t WHERE t.pid = ? AND t.code = 'female'
          AND EXISTS (SELECT 1 FROM v_date d INDEXED BY v_date_r WHERE d.rid = t.rid AND d.pid = ? AND d.hi > ?)
        ORDER BY t.rid"""
e = (P("Patient", "gender"), P("Patient", "birthdate"), D1980)
run("Patient?gender=female&birthdate=ge1980  (page)", lambda: page(db.execute(GB, e)))
run("   … total", lambda: q1(GB.replace("SELECT t.rid", "SELECT count(*)").replace("ORDER BY t.rid", ""), *e))

# ---- Q: Patient?family=Sato&_revinclude=Observation:subject ------------------------
FAM = "SELECT rid FROM v_string WHERE pid = ? AND norm >= 'sato' AND norm < 'sato\U0010ffff' ORDER BY rid"
def revinc():
    pats = page(db.execute(FAM, (P("Patient", "family"),)))
    qs = ",".join("?" * len(pats))
    obs = db.execute(f"SELECT rid FROM v_ref INDEXED BY v_ref_t WHERE target IN ({qs}) AND pid = ?",
                     (*pats, P("Observation", "subject"))).fetchall()
    return len(pats), len(obs)
run("Patient?family=Sato&_revinclude=Observation:subject", revinc)

#!/usr/bin/env python3
"""Milestone 0: build the search-index v2 tables from an existing v1 index.

Throw-away prototype (design/search-index-v2.md §6). It re-shapes the rows a v1
index already holds — the extraction logic is unchanged — into the v2 schema,
entirely in SQL, so the sizes and query plans of v2 can be measured on real data
before any Rust is written.

usage: SQLITE_TMPDIR=<disk dir> python3 -I build_v2.py <v1 data dir> <out.sqlite>
"""
import sqlite3
import sys
import time

data, out = sys.argv[1], sys.argv[2]
db = sqlite3.connect(out, isolation_level=None)
db.executescript(f"""
PRAGMA journal_mode = OFF;
PRAGMA synchronous = OFF;
PRAGMA cache_size = -2000000;
PRAGMA temp_store = FILE;
ATTACH 'file:{data}/search_index.sqlite?mode=ro' AS v1;
ATTACH 'file:{data}/resources.sqlite?mode=ro' AS st;
""")


def step(name, sql):
    t = time.time()
    db.executescript("BEGIN;" + sql + "COMMIT;")
    print(f"{name:<28} {time.time() - t:8.1f} s", flush=True)


step("dictionaries", """
CREATE TABLE rtype (tid INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL);
INSERT INTO rtype (name) SELECT DISTINCT resource_type FROM st.resources;
CREATE TABLE param (pid INTEGER PRIMARY KEY, tid INT NOT NULL, name TEXT NOT NULL,
                    kind TEXT NOT NULL, UNIQUE (tid, name));
-- one row per (type, param): the index's distinct triples (an index-only scan)
INSERT OR IGNORE INTO param (tid, name, kind)
  SELECT t.tid, x.param_name, x.param_type
  FROM (SELECT DISTINCT resource_type, param_name, param_type FROM v1.search_index) x
  JOIN rtype t ON t.name = x.resource_type;
CREATE TABLE csystem (sid INTEGER PRIMARY KEY, uri TEXT UNIQUE NOT NULL);
INSERT INTO csystem (uri)
  SELECT DISTINCT value_system FROM v1.search_index WHERE value_system IS NOT NULL;
""")

step("rkey", """
CREATE TABLE rkey (rid INTEGER PRIMARY KEY, tid INT NOT NULL, id TEXT NOT NULL,
                   present INT NOT NULL, UNIQUE (tid, id));
INSERT INTO rkey (tid, id, present)
  SELECT t.tid, r.id, 1 FROM st.resources r JOIN rtype t ON t.name = r.resource_type
  ORDER BY r.resource_type, r.id;
""")

# Unpacked staging of v1 rows with integer keys (one pass over v1).
step("stage (join v1 → keys)", """
CREATE TEMP TABLE s AS
  SELECT p.pid, p.kind, k.rid, x.value_string AS vs, x.value_string_lower AS vl,
         x.value_system AS sys, x.value_date_start AS lo, x.value_date_end AS hi,
         x.value_number AS num
  FROM v1.search_index x
  JOIN rtype t ON t.name = x.resource_type
  JOIN param p ON p.tid = t.tid AND p.name = x.param_name
  JOIN rkey k ON k.tid = t.tid AND k.id = x.resource_id;
""")

step("v_token", """
CREATE TABLE v_token (pid INT, code TEXT, sid INT, rid INT,
                      PRIMARY KEY (pid, code, sid, rid)) WITHOUT ROWID;
INSERT OR IGNORE INTO v_token
  SELECT s.pid, s.vs, coalesce(c.sid, 0), s.rid FROM s LEFT JOIN csystem c ON c.uri = s.sys
  WHERE s.kind = 'token';
""")

step("v_string", """
CREATE TABLE v_string (pid INT, norm TEXT, rid INT, exact TEXT,
                       PRIMARY KEY (pid, norm, rid, exact)) WITHOUT ROWID;
INSERT OR IGNORE INTO v_string SELECT pid, vl, rid, vs FROM s WHERE kind = 'string';
""")

step("v_date", """
CREATE TABLE v_date (pid INT, lo INT, hi INT, rid INT,
                     PRIMARY KEY (pid, lo, hi, rid)) WITHOUT ROWID;
INSERT OR IGNORE INTO v_date SELECT pid, lo, hi, rid FROM s WHERE kind = 'date' AND lo IS NOT NULL;
""")

step("v_number", """
CREATE TABLE v_number (pid INT, val REAL, rid INT, PRIMARY KEY (pid, val, rid)) WITHOUT ROWID;
INSERT OR IGNORE INTO v_number SELECT pid, num, rid FROM s WHERE num IS NOT NULL;
""")

# References: keep the "Type/id[/_history/n]" form only (the bare-id duplicate rows
# disappear); targets that don't exist get a key with present = 0.
step("v_ref", """
CREATE TEMP TABLE r AS
  SELECT pid, rid,
         substr(vs, 1, instr(vs, '/') - 1) AS ttype,
         CASE WHEN instr(substr(vs, instr(vs, '/') + 1), '/') > 0
              THEN substr(substr(vs, instr(vs, '/') + 1), 1, instr(substr(vs, instr(vs, '/') + 1), '/') - 1)
              ELSE substr(vs, instr(vs, '/') + 1) END AS tid_text
  FROM s WHERE kind = 'reference' AND instr(vs, '/') > 0 AND vs NOT LIKE '%://%';
INSERT OR IGNORE INTO rtype (name) SELECT DISTINCT ttype FROM r;
INSERT OR IGNORE INTO rkey (tid, id, present)
  SELECT DISTINCT t.tid, r.tid_text, 0 FROM r JOIN rtype t ON t.name = r.ttype;
CREATE TABLE v_ref (pid INT, target INT, rid INT, PRIMARY KEY (pid, target, rid)) WITHOUT ROWID;
INSERT OR IGNORE INTO v_ref
  SELECT r.pid, k.rid, r.rid FROM r JOIN rtype t ON t.name = r.ttype
  JOIN rkey k ON k.tid = t.tid AND k.id = r.tid_text;
CREATE TABLE v_ref_text (pid INT, value TEXT, rid INT, PRIMARY KEY (pid, value, rid)) WITHOUT ROWID;
INSERT OR IGNORE INTO v_ref_text SELECT pid, vs, rid FROM s
  WHERE kind = 'reference' AND (vs LIKE '%://%' OR vs LIKE 'urn:%' OR vs LIKE '#%');
""")

step("has_param, type_count", """
CREATE TABLE has_param (pid INT, rid INT, PRIMARY KEY (pid, rid)) WITHOUT ROWID;
INSERT OR IGNORE INTO has_param SELECT pid, rid FROM s;
CREATE TABLE type_count (tid INTEGER PRIMARY KEY, n INT NOT NULL);
INSERT INTO type_count SELECT tid, count(*) FROM rkey WHERE present = 1 GROUP BY tid;
""")

step("secondary indexes", """
CREATE INDEX v_token_r ON v_token (rid, pid, code, sid);
CREATE INDEX v_string_r ON v_string (rid, pid);
CREATE INDEX v_date_h ON v_date (pid, hi, lo);
CREATE INDEX v_date_r ON v_date (rid, pid);
CREATE INDEX v_number_r ON v_number (rid, pid);
CREATE INDEX v_ref_r ON v_ref (rid, pid);
CREATE INDEX v_ref_t ON v_ref (target, pid, rid);
CREATE INDEX v_ref_text_r ON v_ref_text (rid, pid);
CREATE INDEX rkey_live ON rkey (tid, rid) WHERE present = 1;
""")

db.execute("DROP TABLE temp.s")
db.execute("DROP TABLE temp.r")
for t in ["rkey", "v_token", "v_string", "v_date", "v_number", "v_ref", "v_ref_text", "has_param"]:
    print(f"rows {t:<12} {db.execute(f'SELECT max(rowid) FROM {t}' if t == 'rkey' else f'SELECT count(*) FROM {t}').fetchone()[0]}", flush=True)
db.execute("DETACH v1")
db.execute("DETACH st")
t = time.time()
db.execute("VACUUM")
print(f"{'vacuum':<28} {time.time() - t:8.1f} s")

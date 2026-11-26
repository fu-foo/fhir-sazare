# Search index v2

Status: **draft for review** · Target: 0.15 (before 1.0) · Author: Claude Code, with the maintainer

## 1. Why

A benchmark at hospital scale (300k Patients, 9.6M Observations, 9.9M resources; data dir
68 GB) showed that searches touching large sets are far too slow, and that the index is
4× the size of the data it indexes:

| query | time |
|---|---:|
| `Observation?_sort=-date&_count=20` | 249 s |
| `Patient?_has:Observation:subject:code=8867-4&_count=20` | 160 s |
| `Observation?category=vital-signs&status=final&_count=20` | 21–29 s |
| `Observation?_count=20` | 1.5 s |
| `Observation?patient=p42&code=…` (after #25) | 1 ms |

| file | size |
|---|---:|
| `resources.sqlite` (current + history JSON) | 12.6 GB |
| `search_index.sqlite` | **50.7 GB** |
| `search_index.fts.sqlite` | 9.4 GB |

These are not four bugs. They share one cause, and fixing them one at a time would
produce four special cases on top of it.

### 1.1 Root cause

**Set algebra happens outside SQLite, on string ids.** For every search parameter the
executor asks SQLite for *all* matching `resource_id` strings, collects them into a
`Vec<String>`, and intersects / sorts / pages / counts them in Rust:

- an unfiltered list materializes 9.6M strings to return 20;
- `category&status` builds two 9.6M-entry `HashSet<String>`s to intersect them;
- `_sort` reads every value of the type into memory and sorts it;
- `_has` materializes 1.2M source ids, then resolves their references.

**The index stores everything as repeated text.** 108M rows, ~470 bytes each. Every row
repeats `resource_type`, `resource_id`, `param_name`, `param_type` as strings, keeps both
`value_string` and `value_string_lower`, and is stored in eight B-trees (the table, its
UNIQUE autoindex, and six secondary indexes), most repeating those strings. A reference
is stored twice (`Patient/p0` and `p0`) under each of `subject` and `patient`: four rows.

#25 (scoping later parameters to an earlier parameter's candidates through a temp
table) is a real improvement for selective searches, but it is a tactic within this
structure, not a fix for it.

## 2. Goals and non-goals

Goals
- Every search that returns a page does work proportional to the page (plus the cost of
  the most selective criterion), not to the size of the type.
- Counting (`total`) is done inside SQLite on integers.
- The index shrinks by roughly an order of magnitude.
- New search parameter kinds (quantity, composite, uri) are **added as data, not as new
  executor code paths**.
- Changing the index format — now and later — never blocks startup: the new index is built
  in the background while the server serves from the old one.
- Search behavior is unchanged (§3), proven by differential tests against the current
  executor.

Non-goals (this design)
- `resources.sqlite` (resource JSON, history) is unchanged.
- No other database engine; SQLite stays (see the scaling discussion: single writer is
  fine for human-entered data).
- No new search features beyond what the structure makes possible; quantity/composite/uri
  come later, on top of it.

## 3. Behavior to preserve

The v2 executor must return the same results as today for all of these (the differential
test suite enumerates them):

- Parameter types: token, string, date, reference, number; `_id`, `_lastUpdated`, `_tag`,
  `_profile`, `_security`; custom `SearchParameter`s compiled by the FHIRPath subset.
- Token forms `code`, `system|code`, `|code`, `system|`.
- String: prefix (default, case-insensitive), `:exact`, `:contains`.
- Date: half-open ranges by precision, prefixes `eq ne gt ge lt le sa eb ap`; Periods span
  their range.
- Number: precision-aware `eq`/`ne`, `ap` ±10%, other prefixes.
- Reference: `Type/id`, bare `id`, versioned references, absolute URLs as stored today.
- Modifiers `:missing`, `:not` (a resource without the parameter matches `:not`).
- Comma = OR within a parameter; repeated parameters = AND.
- Chains up to 4 hops (`subject:Patient.organization.name=…`); `_has` one level.
- `_include` / `_revinclude` (validated targets), patient-compartment filtering of
  matches and included resources.
- `_sort` with several keys, `-` for descending; a resource with several values sorts by
  its lowest (asc) / highest (desc); resources without a value last; ties by id.
- `_count` (default 100, max 1000), `_offset`, `total` (accurate), `_total=none`,
  `_summary=count`.
- `_text` / `_content` (FTS5 trigram, LIKE fallback for terms under 3 characters).
- Subscription criteria evaluated against a single changed resource.

## 4. Design

### 4.1 Integer keys

All in the index database (the index stays derived data that can be rebuilt from
`resources.sqlite`):

```sql
CREATE TABLE rtype  (tid INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL);      -- Patient, Observation, …
CREATE TABLE param  (pid INTEGER PRIMARY KEY, tid INTEGER NOT NULL, name TEXT NOT NULL,
                     kind INTEGER NOT NULL, UNIQUE (tid, name));               -- (Observation, code, token)
CREATE TABLE csystem(sid INTEGER PRIMARY KEY, uri TEXT UNIQUE NOT NULL);       -- http://loinc.org …
CREATE TABLE rkey   (rid INTEGER PRIMARY KEY, tid INTEGER NOT NULL, id TEXT NOT NULL,
                     present INTEGER NOT NULL, UNIQUE (tid, id));              -- Observation/o1 → 4711
```

- `rkey` gives every resource *and every reference target* an integer `rid`. A target that
  doesn't exist (yet) gets a key with `present = 0`, so a reference can always be stored
  as an integer and becomes resolvable when the target arrives. Keys are never reused.
- The dictionaries are small and cached in memory by `SearchIndex`.

### 4.2 Value tables

One table per value kind, each `WITHOUT ROWID` with two covering B-trees: one ordered for
searching by value, one ordered by resource for updates and per-resource probes.

```sql
CREATE TABLE v_token (pid INT, code TEXT, sid INT, rid INT,            -- sid 0 = no system
                      PRIMARY KEY (pid, code, sid, rid)) WITHOUT ROWID;
CREATE INDEX v_token_r ON v_token (rid, pid, code, sid);

CREATE TABLE v_string (pid INT, norm TEXT, rid INT, exact TEXT,       -- norm = lower-cased
                       PRIMARY KEY (pid, norm, rid, exact)) WITHOUT ROWID;
CREATE INDEX v_string_r ON v_string (rid, pid);

CREATE TABLE v_date (pid INT, lo INT, hi INT, rid INT,                -- [lo, hi) µs
                     PRIMARY KEY (pid, lo, hi, rid)) WITHOUT ROWID;
CREATE INDEX v_date_r ON v_date (rid, pid);

CREATE TABLE v_number (pid INT, val REAL, rid INT,
                       PRIMARY KEY (pid, val, rid)) WITHOUT ROWID;
CREATE INDEX v_number_r ON v_number (rid, pid);

CREATE TABLE v_ref (pid INT, target INT, rid INT,                     -- target = rkey.rid
                    PRIMARY KEY (pid, target, rid)) WITHOUT ROWID;
CREATE INDEX v_ref_r ON v_ref (rid, pid);
CREATE INDEX v_ref_t ON v_ref (target, pid, rid);                     -- _revinclude, _has

CREATE TABLE v_ref_text (pid INT, value TEXT, rid INT,                -- urn:, #contained, foreign URLs
                         PRIMARY KEY (pid, value, rid)) WITHOUT ROWID;

CREATE TABLE has_param (pid INT, rid INT, PRIMARY KEY (pid, rid)) WITHOUT ROWID;  -- :missing, sort "missing last"
CREATE TABLE type_count (tid INTEGER PRIMARY KEY, n INTEGER NOT NULL);           -- unfiltered totals
```

- A bare-id reference search (`patient=p0`) resolves `p0` against the parameter's
  target types (`Patient`, or `Patient|Group|Device` for `subject`) to their `rid`s; the
  duplicate "bare id" rows disappear.
- `exact` is kept only where `:exact` needs it (and could be omitted when equal to `norm`
  in a later refinement).
- Full text keeps its own file; its FTS5 `rowid` becomes the `rid`
  (`resource_text_key` disappears).

**Size estimate** (to be verified in milestone 0): a token row is ~20 bytes per B-tree
instead of ~470 in total over seven; reference rows drop from four per reference to two.
Expect the index around **5 GB** instead of 50 GB at 10M resources.

### 4.3 Query execution

A `SearchQuery` is compiled into a plan of rid-set nodes, executed in SQLite:

```
Plan := Leaf(param, values…)        -- one value table, OR over comma values
      | Not(Leaf) | Missing(pid, bool)
      | Chain(ref pid, target plan)  -- rid IN (SELECT rid FROM v_ref WHERE pid=? AND target IN (target plan))
      | Has(source plan, ref pid)    -- rid IN (SELECT target FROM v_ref WHERE pid=? AND rid IN (source plan))
      | Text(fts query)
      | And(Plan…)                   -- driven by the smallest, others probed per rid
```

- **Driving set.** For `And`, each child gets a bounded cardinality probe
  (`SELECT count(*) FROM (… LIMIT 10001)`, integer index reads, well under a millisecond).
  The smallest child drives; the others are checked per candidate through the `…_r`
  (by-resource) indexes as `EXISTS` sub-queries. This replaces both the HashSet
  intersection and #25's temp table, and the decision no longer relies on a fixed ranking
  or on SQLite statistics (which we don't keep: ANALYZE takes minutes at this scale).
- **Paging** streams the plan in order and stops after `_offset + _count` rows.
- **Total** is `SELECT count(*)` over the same plan, on integers. An unfiltered search
  reads `type_count`. `_total=none` skips it.
- **Sorting** walks the sort parameter's value index in order (`v_date (pid, lo, …)`
  ascending, or descending), keeps rows whose `rid` passes the plan, and emits each rid on
  its first occurrence — exactly "lowest value ascending / highest descending". Equal
  keys are ordered by id; further sort keys break ties within such a group. Rids without a
  value (`has_param`) follow, by id. Only `_offset + _count` rows are produced.
  When the plan's driving set is small, the opposite order is cheaper (fetch the few
  candidates' values and sort them); the executor picks by the probe count.
- **Compartment** filtering for patient-scoped access becomes one more `And` child:
  `rid IN (SELECT rid FROM v_ref WHERE pid IN (compartment params) AND target = patient)`
  — instead of loading candidates as JSON and filtering them.
- **`_revinclude`**: `v_ref_t (target, pid, rid)` with the page's rids.
  `_include` still reads references from the page's resources.
- **Subscriptions** keep evaluating criteria on a one-resource in-memory index (same
  schema).

### 4.4 Writes

`replace_resource(rid, entries)` deletes the resource's rows through the `…_r` indexes and
inserts the new ones, in the existing batched transactions (`replace_many`). `type_count`
is adjusted when `rkey.present` changes. Dictionary inserts are cached.

### 4.5 Rollout: building a new index without blocking

The index format changes (`INDEX_FORMAT = 2`), so existing installations need a full
rebuild — at 10M resources this must not stop the server. The same mechanism serves later
parameter changes.

1. On start, if the index format is old, the server **serves from the old index** and
   starts building `search_index.v2.sqlite` in the background, reading
   `resources.sqlite` in pages (bounded memory) in batches.
2. Writes during the build go to the old index as now, and their `(type, id)` is appended
   to a `pending` table.
3. When the build reaches the end, `pending` is replayed into the new index under the
   index lock, the new index is switched in, and the old files are deleted.
4. Progress is visible in `/health` and `/metrics`; a crash mid-build restarts the build
   (it is derived data).
5. Config `index.rebuild: background | blocking | manual` (default `background`):
   `manual` builds only on `POST /$reindex`.

Downgrade after the switch is by restoring the backup (as today: migrations are
forward-only).

## 5. Verification

- **Differential tests** (written first, against the current executor): generated
  datasets (several types, references, multi-valued fields, missing values, periods,
  numbers, unicode strings) × generated queries covering §3; v1 and v2 must return the same
  ids, order, and totals. Deterministic seeds; failures shrink to a minimal case.
- Existing unit / e2e / security tests, Inferno US Core v7 + v8 and JP Core in CI.
- Benchmarks: the 60k CI gate as today; 1M and 10M runs locally, with the targets below.

| query (10M) | today | target |
|---|---:|---:|
| `Observation?_count=20` | 1.5 s | < 10 ms |
| `Observation?_sort=-date&_count=20` | 249 s | < 50 ms |
| `Observation?code=X&_sort=-date&_count=20` | 195 s | < 1 s |
| `Patient?_has:Observation:subject:code=X&_count=20` | 160 s | < 2 s |
| `Observation?category=A&status=B&_count=20` (matches all) | 21–29 s | < 5 s (the accurate total dominates; `_total=none` < 50 ms) |
| `Observation?patient=X&code=Y` | 1 ms | ≤ 1 ms |
| index size | 50.7 GB | ~5 GB |
| first start after upgrade | — | serves immediately; rebuild in background |

## 6. Milestones

0. **Prototype and measure** (throw-away script): build the v2 tables from the 10M dataset
   and run the target queries as hand-written SQL. Confirms the size estimate and the plan
   shapes before any Rust is written. *Go / no-go point for this design.*
1. Differential test harness against the current executor.
2. Background index build and switch (§4.5), still with the v1 format — useful on its own
   (startup never blocks on a reindex).
3. v2 schema, writes, and the query compiler behind `INDEX_FORMAT = 2`; differential tests
   green.
4. 1M / 10M benchmarks against the targets; docs (README limitations, UPGRADING).
5. Release 0.15. Then the stability policy and 1.0.

## 7. Risks and open questions

- **Planner choices without statistics.** We control join order explicitly (driving set
  from probes, `EXISTS` probes, `CROSS JOIN` where needed) instead of relying on ANALYZE.
  Milestone 0 checks each plan with `EXPLAIN QUERY PLAN`.
- **Accurate totals on unselective queries** stay O(matches). Integer counting should
  bring 9.6M to a few seconds; if not, the default could become `_total=estimate`
  (FHIR allows it) — a behavior change to decide before 1.0. *Open.*
- **Sort tie-break by id** within equal keys needs the ids of a key group; groups are
  usually small, but a sort on a low-cardinality key (e.g. `_sort=status`) has huge
  groups. Option: define ties as "by rid" (insertion order) instead — also a behavior
  change. *Open.*
- **Reference forms** (versioned, absolute same-server URLs, foreign URLs, `urn:`,
  contained) must map exactly as today; the differential tests include each.
- **Memory**: plans stream; no step materializes a type-sized set in Rust.
- **Effort**: the executor (~900 lines) and index (~1,400 lines) are largely rewritten;
  the parsers, registry, extraction (`index_builder`) and handlers stay.

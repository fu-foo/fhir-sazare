# Search index v2

Status: **milestone 0 done — go; revision 4** (after two external reviews and the prototype) · Target: 0.15 (before 1.0) · Author: Claude Code, with the maintainer

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
  its lowest (asc) / highest (desc); resources without a value last. **Changed (decided):**
  the order of resources with equal sort keys is *unspecified* — a client that needs an
  order asks for it with another sort key. In practice it is insertion order (`rid`).
  Today it is by id; the differential tests compare tie groups as sets.
- `_count` (default 100, max 1000), `_offset`, `_total=none`, `_summary=count`.
- **`total` is always accurate** (decided): even when a search barely narrows, the count is
  exact; the time it takes is an accepted cost, as in other databases.
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
- `rtype`, `param` and `csystem` are small and cached in memory. `rkey` is not (10M+
  rows); it is read through its indexes.
- `CREATE INDEX rkey_live ON rkey (tid, rid) WHERE present = 1` serves unfiltered lists
  and `:missing=true` without touching deleted or not-yet-arrived keys.

### 4.2 Value tables

One table per value kind, each `WITHOUT ROWID` with two covering B-trees: one ordered for
searching by value, one ordered by resource for updates and per-resource probes.

```sql
CREATE TABLE v_token (pid INT, code TEXT, sid INT, rid INT,            -- sid 0 = no system
                      PRIMARY KEY (pid, code, sid, rid)) WITHOUT ROWID;
CREATE INDEX v_token_r ON v_token (rid, pid, code, sid);
-- `system|` (any code in a system) scans the pid's rows; an index (pid, sid) is added
-- only if milestone 0 shows it is needed.

CREATE TABLE v_string (pid INT, norm TEXT, rid INT, exact TEXT,       -- norm = lower-cased
                       PRIMARY KEY (pid, norm, rid, exact)) WITHOUT ROWID;
CREATE INDEX v_string_r ON v_string (rid, pid);

CREATE TABLE v_date (pid INT, lo INT, hi INT, rid INT,                -- [lo, hi) µs
                     PRIMARY KEY (pid, lo, hi, rid)) WITHOUT ROWID;
CREATE INDEX v_date_h ON v_date (pid, hi, lo);                        -- ge/gt: seek on hi
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
CREATE INDEX v_ref_text_r ON v_ref_text (rid, pid);

CREATE TABLE type_count (tid INTEGER PRIMARY KEY, n INTEGER NOT NULL);           -- unfiltered totals
```

- **Date ranges.** A resource range `[lo, hi)` matches `ge X` when `hi > X`, which the
  primary key (ordered by `lo`) can't seek; `v_date_h` can. For `eq`/`ap`
  (`lo < qhi AND hi > qlo`) the pid's longest span (`param.max_span`, kept up to date on
  write) bounds the scan: `lo >= qlo - max_span AND lo < qhi`. Instants and dates have
  short spans, so this stays a narrow range; a parameter holding long Periods gets a wider
  one. (v1 has the same weakness: `code=…&date=ge…` took 6 s.) Open-ended Periods
  (`hi = i64::MAX`) are left out of `max_span` and matched by a separate seek on
  `v_date_h` (`hi = MAX AND lo < qhi`), so one open Period can't turn every `eq` into a
  full scan.
- **Compartment membership** is indexed for every compartment type, including types with
  no search parameters (`Consent`, `Basic`, …): extraction writes the compartment's
  reference fields under an internal pid `_compartment` into `v_ref`. Compartment
  filtering never depends on which search parameters happen to exist.
- `WITHOUT ROWID` secondary indexes carry the primary-key columns implicitly
  (`v_string_r (rid, pid)` effectively stores `norm` and `exact` too); the size estimate
  below is checked against real files in milestone 0.
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

- **Driving set.** For `And`, each *seekable* child gets a bounded cardinality probe
  (`SELECT count(*) FROM (… LIMIT 10001)`, integer index reads, well under a millisecond).
  The smallest drives; the others are checked per candidate through the `…_r`
  (by-resource) indexes as `EXISTS` sub-queries. This replaces both the HashSet
  intersection and #25's temp table, without SQLite statistics (which we don't keep:
  ANALYZE takes minutes at this scale).
  - *Seekable* = an equality or a range the index can seek: token, string prefix/exact,
    reference, number, date via the right index (§4.2), `_id`. Nodes whose probe could
    itself scan millions of rows are **never probed and never drive** unless nothing else
    can: `:contains`, `:not`, `:missing`, `Chain`, `Has`, full text. They run as `EXISTS`
    checks on the driving candidates.
  - **Saturated probes** (several children at the 10,001 cap) are ordered by the static
    rank from #25 (`_id` > reference > identifier > token > string > date/number), so a
    `code` drives rather than `status`.
  - `Chain` / `Has` that must drive (nothing else in the query) are estimated by their
    inner plan's probe, without `DISTINCT`.
- **Duplicates.** A resource can match one criterion several times (several names, codes
  in two systems, a comma OR, many sources in `_has`). Paging and counting use distinct
  rids: driving rows are de-duplicated in order (the value index yields each rid's rows
  together only for equality criteria; ranges and ORs go through `DISTINCT`), and `total`
  is `count(DISTINCT rid)`. Milestone 0 measured it (§8): a parameter that never has more than one value per resource skips the `DISTINCT`.
- **`Has` and `Chain` results** are restricted to existing resources of the searched type:
  `target` joins `rkey` with `tid = <type> AND present = 1` (a `subject` may point at a
  Group, or at a deleted or never-created Patient).
- **Paging** streams the plan in order and stops after `_offset + _count` rows.
- **Total** is `SELECT count(*)` over the same plan, on integers. An unfiltered search
  reads `type_count`. `_total=none` skips it.
- **Sorting** walks the sort parameter's value index in order (`v_date (pid, lo, …)`
  ascending, or descending), keeps rows whose `rid` passes the plan, and emits each rid on
  its first occurrence — exactly "lowest value ascending / highest descending". Further
  sort keys order resources with an equal key; beyond the last key the order is
  unspecified (rid order, i.e. insertion order, comes for free from the index). Rids
  without a value follow, in rid order (found by probing the value table's `…_r` index). Only `_offset + _count` rows are
  produced.
  When the plan's driving set is small, the opposite order is cheaper (fetch the few
  candidates' values and sort them); the executor picks by the probe count. Because the
  sort key and the filter can be correlated (all matches near the end of the sort order),
  the sort-driven walk is bounded: after scanning N rows (e.g. 100× the page) without
  filling the page, it switches to the set-driven order.
- **Compartment** filtering for patient-scoped access becomes one more `And` child:
  `rid IN (SELECT rid FROM v_ref WHERE pid IN (compartment params) AND target = patient)`
  — instead of loading candidates as JSON and filtering them.
- **`_revinclude`**: `v_ref_t (target, pid, rid)` with the page's rids.
  `_include` still reads references from the page's resources.
- **Subscriptions** keep evaluating criteria on a one-resource in-memory index (same
  schema).
- **Concurrency.** Today every search holds the single `Mutex<SearchIndex>`, so one
  seconds-long accurate total would stall all other searches *and* writes. v2 gives the
  index what the resource store has: a pool of read-only WAL connections for searches
  (run in `spawn_blocking`), and one writer connection behind the lock. A slow count then
  delays nobody else. (This part doesn't depend on the v2 schema and can land first.)

### 4.4 Writes

`replace_resource(rid, entries)` deletes the resource's rows through the `…_r` indexes and
inserts the new ones, in the existing batched transactions (`replace_many`). `type_count`
is adjusted when `rkey.present` changes. Dictionary inserts are cached.

### 4.5 Rollout: building a new index without blocking

The index format changes (`INDEX_FORMAT = 2`), so existing installations need a full
rebuild — at 10M resources this must not stop the server. The same mechanism serves later
parameter changes.

**Generations.** Index files carry a generation number
(`search_index.g2.sqlite`, `search_index.g2.fts.sqlite` — the FTS file is built and
switched together, because its `rowid` is the new `rid`). A generation counts only once its
`index_meta` has `ready = 1`. On start the server opens the newest ready generation and
deletes any unfinished one (it restarts the build). A crash at any moment is therefore
recoverable, and nothing is renamed or deleted while open — required on Windows, where an
open SQLite file can't be removed.

**Build.**
1. `pending` change recording is switched on *before* the scan starts.
2. The server keeps serving from the current generation. A background task reads
   `resources.sqlite` with keyset paging (`(resource_type, id) > (?, ?) … LIMIT n`, never
   `OFFSET`) and writes the new generation in batches, with a short pause between batches
   so live traffic keeps the disk.
3. Every write — create, update, patch, delete (single, conditional, in Bundles) and
   `$import` — updates the current index as now and records `(type, id)` in `pending`.

**Drain and switch.**
4. While building, and afterwards, `pending` is drained *without* blocking anyone:
   de-duplicated batches are replayed from the store (a resource that is gone is removed
   from the new generation).
5. When fewer than a few hundred entries remain, the switch takes the **write gate** — a
   `RwLock` that every write handler holds (shared) across *both* its index and store
   writes. With the gate held exclusively no write is between its two steps (the handlers
   write the index first, then the store, so without the gate a resource could be
   recorded, replayed before it reached the store, and then be missing from the new
   index). The rest of `pending` is replayed, `ready = 1` is committed, readers and the
   writer move to the new files, the old connections are closed, then the old files are
   deleted. Only this last step blocks writes, for a moment. Searches still running on
   the old generation keep it open; it is closed and its files deleted when the last of
   them finishes (the generation is reference-counted and marked delete-on-drop at the
   switch) — needed on Windows, harmless elsewhere.

**Operations.** Progress in `/health` and `/metrics`. `POST /$reindex` while a build runs
is a 409. Config `index.rebuild: background | blocking | manual` (default `background`;
`manual` builds only on `POST /$reindex`). UPGRADING states the peak disk use: the old and
new generations coexist during the build (at 10M resources about 12.6 GB resources +
60 GB old index and FTS + ~15 GB new: plan for ~90 GB free).

**Later parameter changes** don't need a new generation: `rid`s and the FTS file stay, and
only the changed parameters are rebuilt, in the background, with the same pending/drain
steps. A changed parameter is written under a **new staging `pid`**; searches keep using
the old one until the rebuild is complete, then `param` switches to the new `pid` and the
old rows are deleted — so a parameter change never makes searches on that parameter
incomplete, even briefly.

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
| `Observation?category=A&status=B&_count=20` (matches all) | 21–29 s | < 5 s (the accurate total dominates and is kept; with `_total=none` < 50 ms) |
| `Observation?patient=X&code=Y` | 1 ms | ≤ 1 ms |
| index size | 50.7 GB | ~5 GB |
| first start after upgrade | — | serves immediately; rebuild in background |

## 6. Milestones

0. **Prototype and measure** (throw-away script): build the v2 tables from the 10M dataset
   and run the target queries as hand-written SQL, checking each plan with
   `EXPLAIN QUERY PLAN`. Measures: the target queries (§5), **file sizes**, **build time**
   of the 10M generation, **single-write latency and `$import` throughput** on the 10M
   index, `count(DISTINCT rid)` cost, date `ge`/`eq` plans. *Go / no-go point for this
   design.*
1. Differential test harness against the current executor.
1b. Read-connection pool for the index and searches in `spawn_blocking` (§4.3
    Concurrency) — independent of the schema, lands early.
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
- **Accurate totals on unselective queries** stay O(matches): integer counting inside
  SQLite, expected a few seconds at 9.6M matches. *Decided:* totals stay accurate; that
  time is an accepted cost (no `_total=estimate` default).
- **Sort ties** — *decided:* unspecified (insertion order in practice). Ties no longer need
  ids, so a sort on a low-cardinality key (`_sort=status`) streams like any other.
  Listed as not covered in the stability policy.
- **`:exact` was case-insensitive** (it compared lower-cased values); FHIR defines it as
  exact. *Decided:* follow the spec — fixed already in v1 (before this redesign), so v1 and
  v2 agree and the differential tests need no exception.
- **Disk and I/O during the build**: see §4.5 (peak space, throttling).
- **Reference forms** (versioned, absolute same-server URLs, foreign URLs, `urn:`,
  contained) must map exactly as today; the differential tests include each.
- **Memory**: plans stream; no step materializes a type-sized set in Rust; `rkey` is not
  cached.
- **Effort**: the executor (~900 lines) and index (~1,400 lines) are largely rewritten;
  the parsers, registry, extraction (`index_builder`) and handlers stay.

## 8. Milestone 0 results (10M resources)

Built from the 10M benchmark's v1 index with `design/m0/build_v2.py` (pure SQL over the
v1 rows, so extraction is unchanged); queries in `design/m0/queries.py`, hand-written as
the executor would generate them.

**Size**: 6.0 GB with `has_param`, **5.1 GB without** (v1: 50.7 GB). Per B-tree: token
0.98 + 0.98, date 0.56 × 3, reference 0.29 × 3, rkey 0.53 in all.

| query (10M) | v1 | v2 page | v2 total |
|---|---:|---:|---:|
| `Observation?_count=20` | 1.5 s | 0.0 ms | `type_count` |
| `Observation?_sort=-date&_count=20` | 249 s | 0.0 ms | `type_count` |
| `Observation?code=8867-4&_sort=-date` | 195 s | 0.1 ms | 178 ms |
| `Patient?_has:Observation:subject:code=8867-4` | 160 s | 0.1 ms | 502 ms |
| `Observation?category=…&status=…` (all match) | 21–29 s | 0.0 ms | 3.6 s |
| `Observation?patient=p42&code=…` | 1 ms | 0.1 ms | — |
| `…&date=ge2020-01-01` | ≈6 s | 0.0 ms | — |
| `Observation?code=8867-4&date=ge2020` | 6.3 s | 523 ms ⚠ | 571 ms |
| `Patient?gender=female&birthdate=ge1980` | 0.16 s | 55 ms | 51 ms |
| `Patient?family=…&_revinclude=Observation:subject` | 21 ms | 0.2 ms | — |
| `date=ge2020` alone, count of 5.2M | — | — | 12.8 s ⚠ |
| `status:missing=true` (no `has_param`) | — | — | 3.5 s |

**Writes** (WAL, `synchronous=NORMAL`, index only): single-resource update median
1.4 ms, p95 3.0 ms; batches of 1,000 new resources 2,900–4,100 / s.

**Go.** Every target in §5 is met or within reach of a change found here:

1. **`has_param` is removed.** It had no by-resource index, so every write scanned its
   88.8M rows (found when the write test didn't finish). Presence is a probe of the value
   table's `…_r` index instead; the 0.9 GB go too.
2. **Every probe and every drive names its index** (`INDEXED BY`, `CROSS JOIN`). Without
   statistics the planner chose a date range scan for a per-candidate probe
   (`patient&code&date`: 1,143 ms → 0.0 ms once forced) and the wrong driving table for
   `_has` (3.6 s → 0.5 s).
3. **Unsorted pages stream in index order**, with no `ORDER BY`: the order of an unsorted
   search is unspecified (§3), and ordering by rid across several `sid`s of a code forced
   a sort (`code&date` page: 523 ms).
4. **Single-valued parameters count without `DISTINCT`**: `param.multi` (kept on write)
   says whether any resource has more than one value for the pid; when not, `count(*)`
   replaces `count(DISTINCT rid)` (the temp B-tree made the 5.2M count 12.8 s).

## Revision history

- r4 — milestone 0 results (§8): go; `has_param` removed, explicit indexes for every
  probe and drive, unsorted pages in index order, `DISTINCT` only for multi-valued
  parameters.

- r3 — implementation notes from the second review: open-ended Periods and `max_span`,
  reference-counted old generation (delete when the last reader finishes), staging `pid`s
  for parameter changes. Reviewer: go for milestone 0.
- r2 — after the first external review: probe rules (seekable nodes only, saturation
  ranking), date `ge`/`eq` indexing, `count(DISTINCT rid)`, `Has`/`Chain` type and
  existence checks, compartment pid for all compartment types, read-connection pool,
  `v_ref_text_r`, `rkey` not cached + live partial index, generation files with `ready`,
  write gate across index+store writes, lock-free iterative drain, deletes in `pending`,
  keyset paging, FTS built with the generation, in-place rebuild for parameter changes,
  `$reindex` 409, peak disk, `:exact` case, sort/filter correlation bound; milestone 0
  measures sizes, build time and write speed too.
- r1 — first draft.

# Changelog

All notable changes to sazare. Versions follow [Semantic Versioning](https://semver.org/);
until 1.0, a minor version (0.x.0) may change behavior — those changes are listed under
**Behavior changes**. See [UPGRADING.md](UPGRADING.md) for how to upgrade.

## Unreleased

### Added
- CI compares sazare's write checks with the HL7 FHIR validator (7.0.8): the R4 examples
  (but definitional resources) and copies broken on purpose (a date that doesn't exist, a
  dateTime without a zone, a required element left out, an empty string or object, a uri
  with a space, a 64-bit integer, a code with a double space, two types of a choice). It
  fails when sazare refuses what the validator accepts, or accepts a broken copy the
  validator refuses. First run: 0 and 0 (771 broken copies, 720 examples).
- `sazare-server check`: lists stored resources that don't meet the R4 rules writes are
  checked with (exit 1 when any). Read-only — safe while the server runs, and before an
  upgrade (no migration).

### Fixed
- **Decimals keep their precision.** `5.40` came back as `5.4` and
  `1.000000000000000000001` as `1.0` (numbers were read as 64-bit floats); in R4 the
  precision is part of the value. They are kept as written (an exponent gains its sign:
  `1.0e2` → `1.0e+2`). A decimal beyond 64 bits (`123456789012345678901234567890`) was
  refused; it is stored. Searches still compare values.
- **Conditional references are resolved**: `"reference": "Patient?identifier=urn:mrn|42"` in a
  transaction, a create or an update becomes the one resource the search finds; none or
  several is a 412. It was stored as written, a reference nothing followed (`_include`,
  `_revinclude` and compartments missed it).
- **`Prefer: return=minimal`** answers a create, update or patch without a body, and
  **`return=OperationOutcome`** with an outcome instead of the resource (the headers stay).
  Both were ignored.
- A resource created by `PUT` shows as `PUT` in its history (it was `POST`): each version
  records the method that made it. Versions from before keep the inferred method. A POST's
  entry names the type (`"url": "Patient"`), as R4 has it; it named the resource.
- `GET /{type}/{id}/_history` of an id that never existed is a 404 (it was an empty 200, which
  told an unused id from another patient's, a 404 since 0.18.2).
- A chain's last parameter takes its modifier: `subject.family:exact=Doe`,
  `:contains`, `:missing`, `:not` found nothing (the modifier stayed in the parameter's
  name). An unknown `_` parameter there (`subject._bogus`) is a 400.
- `sazare-server logs --follow` was refused (only `-f` was listed).
- **`$import` is about 27× faster on a real disk.** Each resource was its own store
  transaction, and each store commit waits for the disk: about 170 resources/s on an NVMe
  SSD (CI's disks hid it). Resources are now written 1,000 to a transaction, like the
  index already was: 200k resources in 43 s instead of about 20 minutes. A line that
  fails is still reported and the others kept; the response still comes after everything
  is committed.
- **The dashboard and `/metrics` no longer count the whole store.** Resources per type were
  counted with a scan of every resource on each dashboard refresh (every 5 s) and each
  metrics scrape — seconds at 100M. They are kept in a small table, updated with each write.
- **The dashboard's resource list reads a page, not the whole type.** It sorted every
  resource of the type by the `lastUpdated` inside its JSON for each page (minutes at
  100M); it now walks the search index's `_lastUpdated` order like
  `GET /{type}?_sort=-_lastUpdated`.

### Behavior changes
- **Writes are checked against more of the R4 definitions** — what was stored before and
  silently misbehaved (an invalid date was left out of the index, so `birthdate=1990-02`
  missed it and `:missing=true` found it):
  - **Required elements** (min 1) of every type, nested ones included
    (`Observation.component.code`, `Extension.url`): from the definitions, instead of a
    hand-written list of 17 types. A value or `_name` with an extension
    (a data-absent-reason) meets it.
  - **Primitive formats**: the R4 regex of each type (`date`, `dateTime` — a time needs a
    time zone —, `instant`, `time`, `uri`, `code`, `id`, `oid`, `uuid`, `base64Binary`,
    `integer`, `decimal`, …), a date that exists (`1990-02-30` is refused), and the
    32-bit range of `integer`, `positiveInt` (≥ 1) and `unsignedInt` (≥ 0).
  - **No empty values** (ele-1): `""`, `{}`, `[{}]`; `_name` holds an Element (id,
    extension) and is checked as one.
  - **One type per choice**: `valueString` with `valueBoolean` is refused.
  The R4 examples pass (`r4_examples_fit`, 11 official examples with errors of their own
  listed). Data already stored is read as before; updating it applies the rules.
- `resources.sqlite` moves to schema version 5: the per-type counts (taken once on the first
  start, a scan of the store), two unused indexes dropped (smaller files, cheaper writes),
  and the method of each history version.

## 0.18.3 — 2026-11-28

### Added
- A tab icon for the dashboard (`/favicon.svg`, also at `/favicon.ico`): さざれ石 grown into
  a rock, with moss. It was empty.
- `--port <n>` for `start` and `run` (over `server.port` and `$SAZARE_PORT`).
- **Chains without types**: `subject.name=Doe` (FHIR's own form) was refused as an unknown
  parameter; only `subject:Patient.name` worked. A hop without a type takes the one type
  it may refer to (R4's targets) that has the next parameter; when several do, the 400
  says which to name.
- `$import` errors read as sentences (`line 3 (Patient): …`, not JSON inside a string); a
  line holding a JSON array says that NDJSON is one resource per line; plurals fixed.
- `$export` with an unknown parameter names it in sazare's words (it listed `_typeFilter`,
  which isn't supported).
- `sazare-server restart`: stop (when running) and start, with `start`'s options — the
  step after changing the configuration.

### Fixed
- **Backups and new data directories are owner-only**: `backup` wrote `resources.sqlite`
  and `audit.sqlite` readable by every local user (the umask's 644); they are 0600 now,
  and directories sazare creates (data, logs, backup, export) 0700. Existing directories
  keep their mode.
- **A request with two `Authorization` headers is refused** (400). The first was used and the
  rest ignored, so a proxy in front checking another one than sazare could be misled.
- A configuration block written as a list (`jwt:` then `- secret: …`, easy beside `api_keys`
  and `basic_auth`, which are lists) or the reverse: the error says to remove or add the `- `.
- A taken port (8080 often is: another server, an editor's port forwarding) made `start`
  print the server's log; it now says plainly that the port is in use and how to choose
  another (`--port`, `SAZARE_PORT`, `server.port`). `run` logs the same.
- `auth new-api-key` prints the key alone on its line (it was indented, and a copied line
  carried the spaces), says that config.yaml takes its hash, and both auth helpers end with
  the next step: restart the server.
- `auth hash-password` says the rule (at least 12 characters) before the password is typed,
  and asks again when it is too short or the two entries differ (it ended after both
  entries); characters are counted, not bytes.
- An unsupported search feature (`_filter`, `:text`, …) is reported as `not-supported`, like
  an unknown parameter (it was `invalid`).
- An argument a command doesn't take is an error with the option probably meant: `start
  demo` started without the demo data, silently (`` `demo` … Did you mean `--demo`? ``).

## 0.18.2 — 2026-11-27

### Security
- **A patient-scoped token reading another patient's resource gets 404**, the answer for
  an id that doesn't exist — it got 403, which told that the id exists. Reads, vreads,
  histories, deleted resources and `$everything` of another patient; in a batch, such a
  read is a 404 entry (it refused the whole Bundle with 403). Writes outside the
  compartment are still a 403.

## 0.18.1 — 2026-11-27

### Security
- **A patient-scoped token could create a Patient outside its compartment**: the
  compartment check saw the body's id (`{"resourceType":"Patient","id":"p1"}`), which a
  create then ignores for a new one. Checks now see the stored id — none for a create,
  the URL's for an update (a PUT of one's own Patient without a body id was refused), the
  match's for a conditional update — and, in a transaction, the body's `urn:uuid:`
  references resolved.
- **Another patient's deleted resource** answered 410 to a read (telling that it
  existed), and a PUT to its id created it again, continuing its history. Both are held
  to the compartment of what it was (the record of its delete): 403.
- SMART Backend Services: a client's JWKS fetched from its `jwks_url` is cached (5
  minutes; a failure 10 s), so assertions no longer each make the server fetch it; the
  assertion's `nbf` is checked.
- `X-Forwarded-Proto` is taken only as `http`/`https`, `X-Forwarded-Host` only as a plain
  host (first of a list); a NUL in a search value is a 400 (full-text search answered
  500); chains and `_has` have the 1000-value cap too.

## 0.18.0 — 2026-11-27

### Security
- **Rest-hook endpoints that reach a private address through IPv6** are refused like the
  private address itself: IPv4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96` and 6to4
  `2002::/16` (each judged by the IPv4 address inside), site-local `fec0::/10`, and the
  IPv4 ranges 192.0.0.0/24, 198.18.0.0/15 and 240.0.0.0/4.
- A JWT whose `nbf` is in the future is refused.
- `/ui` (`SAZARE_UI_DIR`) followed symbolic links out of its directory; it is served by
  the same code as `/apps` now (no `..`, no symlinks, nothing outside).
- SECURITY.md: no built-in rate limiting, bulk operations and their disk use, TLS reload.
- A parameter with many comma-separated values answered 500 (SQLite's limit of 500
  terms in a compound SELECT); up to 1000 now search correctly, more are a 400.
- Dependencies updated for the advisories `cargo audit` reports, among them the bundled
  SQLite (`rusqlite` 0.43, `libsqlite3-sys` 0.40), `rustls`, `rustls-webpki` and the
  `tokio` I/O-safety fix. `rustls-pemfile` (unmaintained) is replaced by the PEM parser
  in `rustls-pki-types`. Two unmaintained dependencies remain, with no known vulnerability:
  `serde_yaml`'s `unsafe-libyaml` (it reads only the operator's own configuration file)
  and `chrono`.

### Fixed
- **The patient compartment follows the R4 elements**: EpisodeOfCare and NutritionOrder
  are linked by `patient` (not `subject`), SupplyRequest by `requester`, Person by
  `link.target`, Basic by `subject`, EnrollmentRequest by `candidate` — those resources
  were in no compartment (a patient-scoped token was refused them, `$everything` and
  Group `$export` left them out). `$everything` and Group `$export` find members through
  the index's compartment rows, so a field whose search parameter has another name
  (`Task.for`) counts too.
- A transaction that writes and then deletes one resource left it in the search index.
- `_format=application/fhir+json` with its `+` unencoded (the R4 spec's own example), with
  `;charset=…` or in capitals, and an `Accept` in capitals, were a 406.
- `PUT /Type/a_b` with no id in the body, and a Bundle entry `Patient/a/b`, stored an id
  that isn't a FHIR id.
- A chain or `_has` with an unknown parameter, an unsupported modifier or a value that
  isn't a date is a 400 (it found nothing, or ignored the modifier), as at the top level.
  A date of the wrong shape (`2024X06`) isn't taken for one.
- `POST /Type/_search` also reads parameters in the URL.
- A chain or `_has` ending in a number or quantity with a value that isn't one is a 400,
  and one ending in a reference takes this server's absolute URL as the local reference.
  A Subscription `payload` is matched without regard to case.
- A reference search by this server's absolute URL (`subject=https://host/fhir/Patient/1`)
  matches like the local reference; it found nothing.
- A Subscription whose rest-hook `payload` isn't JSON (`application/fhir+xml`) is
  refused; it was accepted and then sent empty pings.
- The CapabilityStatement declares `$validate` on every type and `$everything` on Patient.
- `$export` refuses parameters it doesn't support (400; they were ignored) and a `_type`
  that isn't an R4 type (it exported nothing for it, as if there were none).
- **`log.level` in the configuration took no effect**: only `RUST_LOG` set the log level.
  It does now; `RUST_LOG`, when set, still wins.
- `docker compose up` failed on a fresh checkout: the compose file mounted a
  `./config.yaml` that doesn't exist (Docker then mounts an empty directory there). The
  mount is now commented out — the image has a default configuration; uncomment it after
  creating the file.
- Stopping the server waited for every request in flight without limit (a request whose
  body never came, a large synchronous `$export`): they now get 30 s, then the server
  stops without them.
- `sazare-server backup` no longer copies the search index (derived, rebuilt on
  restore — 6 GB at 10M resources — and without its full-text half anyway).
- A conditional update (`PUT /Type?search`) sent no Subscription notifications or
  webhooks (every other write did); its 201 now has a `Location` header too.
- A Bundle entry's `request.ifMatch` was ignored, so a transaction or batch `PUT` /
  `DELETE` replaced whatever version was current. A transaction now fails as a whole
  (412) when an entry's version isn't current; a batch fails that entry (412).
- URLs the server builds follow the request everywhere: the Bulk Data manifest and status
  URL ignored `X-Forwarded-Host`; the CapabilityStatement's `implementation.url` and the
  SMART description used the bind address (`http://0.0.0.0:8080` in Docker), and `http`
  under the built-in TLS. Under TLS, a request without `X-Forwarded-Proto` is taken as
  https. The CapabilityStatement's OAuth endpoints are the identity provider's (or this
  server's `/token`) instead of a placeholder that wasn't a URI.
- **Every R4 resource type is searchable.** Only 28 types had search parameters; the
  others (Subscription, Medication, MedicationStatement, Consent, …) could be stored, but
  every search of them came back empty — `Medication?status=active` found nothing — and an
  unknown parameter wasn't a 400. All 146 R4 types now have their R4 parameters and are in
  the CapabilityStatement. The first start rebuilds the search index once, in the
  background.
- A resource type that isn't R4 (`POST /Foo`) is refused — 404 for the URL, 400 for a
  body (a Bundle entry, `$import`) — instead of being stored where nothing could find it.
- **`$everything` pages**: it returned a patient's whole compartment in one Bundle (all of
  it in memory), with relative full URLs. It now takes `_count` (100 by default, at most
  1000), `_offset` and `_type`, links the next page, and gives absolute full URLs; other
  parameters (`start`, `end`, `_since`) are a 400.
- **Group `$export` finds the members' resources in the index** instead of reading and
  testing every resource of the compartment's types — a group of a few patients on a large
  server read all of them.
- **A delete is a version in the history.** `DELETE` removed the current resource and
  left no trace in `_history`; it now records a version (the store's schema moves to 3),
  listed as a `DELETE` entry, and a vread of it answers 410. A re-create continues after
  it. History entries say `POST` for a version that created the resource (the first, or
  the one after a delete) and `PUT` otherwise — a re-create used to say `PUT`.
- `_summary=true` keeps each type's summary elements as R4 defines them; four types had
  a list, the others a guess (DiagnosticReport lost `issued` and `performer`). Resources
  returned with `_summary` or `_elements` carry the `SUBSETTED` tag.
- Writes no longer look for Subscriptions when there are none (an `$import` of 10M
  resources read the Subscription list 10M times). A Subscription set to `error` gets a
  new `lastUpdated` with its new version.
- A panic's message is logged (with where it happened) instead of going to stderr only.
- `$import` lists the first 100 errors (and how many more); a file of bad lines made a
  response as large as the file.
- The CapabilityStatement declares conditional update and delete (`single`) and the
  `_include` values of each type.
- Docker: a `HEALTHCHECK` on `GET /health`; `docker-compose.yml` gives a stop 45 s.
- Building needs Rust 1.89 (the README said 1.85; the updated SQLite binding needs 1.89);
  `rust-version` is set, and CI builds with it.

### Behavior changes
- **String search ignores accents as well as case**, as FHIR specifies: `family=jose`
  finds José, `given=nandu` Ñandú. `:exact` still compares as written. (Index format 5:
  part of the one rebuild of this release.)
- **Concurrent updates of one resource all succeed** (without `If-Match` the last one
  wins, each a version): 29 of 30 simultaneous PUTs to one id answered 409. Updates and
  patches of a resource now go one at a time; a patch is applied to the version current
  when it runs. With `If-Match`, one that names a version no longer current is a 412.
- A PATCH that isn't a JSON Patch (a FHIRPath Patch) is a 415 saying so; it was "invalid
  JSON Patch".
- **A create (`POST /Type`) ignores an id in the body** and assigns one, as R4 specifies
  (transaction and batch entries always did). It used to keep the body's id, or answer
  409 when it existed. To choose the id, `PUT /Type/{id}`. `$import` keeps the ids in the
  file.
- **An id is a FHIR id** (1–64 letters, digits, `-`, `.`) on every write; `a b`, `a_b`
  or a 70-character id were stored. Ids already stored are still read.
- Paths that are neither a resource type nor the server's own are a 404 that says so:
  `/_history` answered an empty search, `/Patient/_history` "Patient/_history not found".
- **Resources are checked against the R4 definitions of their shape.** An element the
  type doesn't define (`"bogusField"`, a misspelled `nmae`, `deceasedString`), a value of
  the wrong JSON type (`"gender": 5`), a list where the element has one value or one
  value where it repeats — all were stored as sent. They are now a 400 naming each (the
  first 20), on every write: REST, Bundles, `$import`. All 2912 R4 examples pass. Not
  checked: minimum cardinality, value formats beyond the JSON type, invariants. Writes
  are about 5% slower.
- **JSON only, said so**: a request for another format (`_format=xml`, an `Accept` that
  takes no JSON) is a 406 — it got JSON back — and a body in another (`Content-Type:
  text/plain`, XML) a 415 — it was read as JSON whatever it claimed. A form body is
  accepted for `_search`.
- A date search value that isn't a date (`date=notadate`, `_lastUpdated=gt20xx`) is a 400;
  it matched nothing.
- **Unsupported search syntax is a 400 instead of being ignored.** `_filter`, `_list`,
  `_query`, `_contained`, `_include:iterate`, a malformed or nested `_has`, and unsupported
  modifiers (`:text`, `:below`, `:in`, `:of-type`, `:identifier`, `:exact` on a token, …)
  were dropped, so the search matched more than asked — `Patient?_filter=…` returned
  every patient. Invalid `_count`, `_offset`, `_summary` and `_total` values are a 400
  too. `_format` and `_pretty` are accepted (JSON is the one format).

### Added
- **Searches inside a batch** (`GET Patient?gender=female` as an entry) answer the
  searchset Bundle of that search; they were refused ("must include id"). A conditional
  `PUT`/`DELETE` inside a Bundle is refused as such (it was taken for a resource type).
- `audit.retention_days`: audit entries older than that are deleted (at startup and
  hourly, a batch at a time). Unset, every entry is kept, as before.

### Changed
- The audit log's commits are no longer each synced to disk (one per request, reads
  included): a crash of the server loses nothing, a power loss the last entries.
- A reference's type modifier is applied: `subject:Patient=123` means
  `subject=Patient/123` (the type used to be dropped). `_tag:not=…` and the other `_`
  search parameters keep their modifier (one used to drop the whole parameter).
- **The Linux binaries run on glibc 2.28 or newer** (RHEL 8, Ubuntu 20.04, Debian 10, …).
  They needed glibc 2.39 (Ubuntu 24.04 or newer), so RHEL 8/9, Ubuntu 20.04/22.04 and
  Debian 12 couldn't run them. They are built against glibc 2.28 with zig as the linker —
  not as static musl binaries, which measured ~1.7x slower on large searches.
- **The Docker image is Debian (glibc) instead of Alpine (musl)**, for the same reason:
  large searches ~1.7x faster, imports ~1.3x. It is larger (Debian slim).
  The data volume and the `sazare` user (uid 10001) are unchanged.

### Tests
- The upgrade check in CI starts from 0.17.0, and from 0.16.0 and 0.12.0 as before.

## 0.17.0 — 2026-11-27

### Fixed
- Stopping the server while it rebuilt the search index in the background
  (`sazare-server stop`, SIGTERM) waited for the rebuild to finish — up to an hour at 10M
  resources — after logging that it had shut down. The rebuild now stops at once; its
  partial index is deleted and built again on the next start or `$reindex`.
- The `latest` Docker image follows releases only. Every push to `main` used to rebuild
  it, so `latest` (used by `docker-compose.yml`) could be an unreleased build; the newest
  `main` is now the `edge` tag.
- FHIR answers are `application/fhir+json`. `/metadata` and every error (400, 401, 403,
  404, …) were `application/json`, which strict clients refuse. What isn't FHIR keeps
  `application/json`: the Bulk Data export manifest, `/health`, `/token`,
  `/.well-known/smart-configuration`, the dashboard and static apps.
- The CapabilityStatement lists Appointment (it was stored and searchable, but not
  listed).
- A request handler that panics answers 500 with an OperationOutcome (and is counted in
  `/metrics`) instead of dropping the connection.
- Long-running servers: the SQLite write-ahead logs are cut back to 64 MiB after a
  checkpoint (an `$import` or a rebuild left them at gigabytes for good); a `/ws` client
  that reads nothing holds at most 64 queued notifications, more are dropped (a
  notification only says "search again"); the server pings each `/ws` connection every
  30 s and closes one that answered nothing for a minute.
- **`$export` no longer holds the export in memory.** It read every resource of the
  server at once and kept the output in memory for an hour — 300k resources took 400 MB
  more, and a large server could run out of memory. A job now writes its files under
  `<data_dir>/exports/` (deleted with the job, and on the next start), and the
  synchronous response is streamed as it is read, a page of resources at a time.
- **A crash no longer leaves the search index behind the store.** The two are separate
  databases; a crash or power loss between a write's two commits left a stored resource
  the index didn't find (a transaction Bundle or an `$import` stored first), or index
  rows for a write the store never got — until the next `$reindex`. The store now
  numbers every change to its resources, the index notes every resource it writes, and
  every minute and at shutdown the index records the change it matches. A start after a
  crash indexes again, from the store, only what changed after that point (logged as a
  warning).
- A database written by a newer sazare is refused at startup (migrations are
  forward-only) instead of being opened.
- A background server (`sazare-server start`) turns its log over at 50 MB while it runs
  (`logs/sazare.log.1` holds the one before). It used to do so only when started, so a
  server that ran for months grew one file without end. `SAZARE_LOG_FILE=<path>` gives
  `run` the same (for launchd and Homebrew's service, which keep stdout in one file); the
  Docker examples cap the log Docker keeps.

### Added
- `SECURITY.md`: how to report a vulnerability, and what a deployment must take care of.
- Releases include `SHA256SUMS` for the downloads.
- **Quantity search parameters**: `[prefix]number|system|code`, `number||code` (the code
  or the unit, any system) or the number alone, with FHIR's implicit precision and the
  comparison prefixes, `:missing` and `_sort`. Quantities, Ages, Durations and Money are
  indexed; units are compared as written (no unit conversion). Every R4 quantity
  parameter of the supported types comes with it: `value-quantity`,
  `component-value-quantity`, `combo-value-quantity` on Observation and the rest. The
  first start rebuilds the search index once, in the background. On an index still being
  rebuilt from an older version, a quantity search (or `_sort` by a quantity) answers 503
  until the new one is ready.

### Behavior changes
- `/metadata` and error responses are `application/fhir+json` instead of
  `application/json` (see Fixed).
- The resource store's schema moves to version 2 (a change log for the search index).
  Like every migration it is forward-only: to go back to 0.16, restore the backup taken
  before upgrading (UPGRADING.md).
- A `/ws` client that reads nothing gets at most 64 queued notifications; more are
  dropped.

### Performance
- **Writes wait for one disk sync, not three.** The search index no longer syncs each
  commit to disk (`synchronous=NORMAL`): what a power loss drops from it is indexed again
  from the store at the next start (see above). A single `PUT` on an NVMe disk: 21 ms →
  10 ms.
- **Faster index writes**: a rebuild and `$import` no longer clear rows a new resource
  can't have, and reuse their SQL statements. 300k resources: `$import` 72 s → 50 s, a
  rebuild 49 s → 27 s.
- **Patient-scoped searches narrow in the index.** A token limited to one patient's
  compartment used to read every match of a search and test each resource for the
  compartment before paging — every Observation of the server for a bare
  `GET /Observation`. Each resource's patient compartments are now indexed (computed by
  the same test the access check applies), and the compartment is one more criterion of
  the search: pages and totals come straight from the index. The page is still checked
  against the compartment. The first start rebuilds the search index once, in the
  background; until it switches, patient-scoped searches test each match as before, and
  quantity searches answer 503.

## 0.16.0 — 2026-11-26

### Changed
- **Index rebuilds happen in the background.** When the search index has to be rebuilt —
  new search parameters, a new index format, a missing full-text index — the server
  answers at once from the current index and builds a new one beside it (a new *index
  generation*, `search_index.gN.sqlite`), then switches to it; writes made during the build
  are replayed into it, and the old files are deleted once their last search is done. At
  10M resources the server used to be unreachable for the hours a rebuild took. Progress
  is in `GET /health` (`index.state`, `done`, `total`) and `/metrics`
  (`sazare_index_rebuild_active`, `sazare_index_rebuild_remaining`). `index.rebuild:
  blocking` restores the old wait; `manual` rebuilds only on `POST /$reindex`.
- `POST /$reindex` starts a background rebuild and answers 202 (409 while one runs).
- **Search index v2.** A new index format: integer keys for resource types, parameters,
  code systems and resources, one table per kind of value, and queries compiled to a
  single SQL statement driven by the most selective criterion. At 10M resources the index
  is 6 GB instead of ~60 GB, and searches that took seconds to a minute answer in
  milliseconds (unsorted and sorted pages of 9.6M Observations: 0.1–0.4 ms instead of
  0.7–62 s); totals stay exact. An existing index is rebuilt in the background on first
  start, the old one serving searches until the switch (see UPGRADING.md).
- `_total=none` now skips counting the matches (the next link still appears when there is
  a next page): the cheap way to page through searches matching millions of resources.

### Added
- **Every R4 search parameter of the supported resource types** (of type token, string,
  date, reference and number): the R4 definitions are bundled and fill what the built-in
  lists lacked — 211 parameters, among them Patient `address*`, `telecom`, `phone`,
  `email`, `active`, `deceased`, `language` (#34). They are advertised in
  `GET /metadata`; existing data is indexed for them by the background rebuild.
- `_include` works for every reference parameter, including those defined by an
  expression (e.g. `Appointment:patient`).
- Search parameter expressions (built-in and custom) may name a choice element without its
  type, as several R4 definitions do: `ServiceRequest.occurrence` finds
  `occurrenceDateTime` / `occurrencePeriod`.

### Behavior changes
- Unsorted search results come back in index order (insertion order for an unfiltered
  search) instead of by id. The order of an unsorted search is unspecified; use `_sort`.
  Ties in `_sort` keep insertion order.

### Tests
- Upgrade check in CI: a data dir written by 0.15.1 opened by this build — searches answered
  by the old index during the background rebuild and by the new one after the switch match
  the old release's, and a write made during the rebuild is found.
- Differential search tests: generated datasets (case and accent variants, repeated and
  missing values, open periods, references) × generated queries (tokens in all four forms,
  string modifiers, date prefixes, `:missing`, `:not`, OR lists, chains, `_has`, `_sort`,
  paging) are checked against a reference model of the FHIR search rules — the net under
  the search index v2 rewrite. Verified to catch planted errors in each area.

### Performance
- Searches (including `_summary=count`, `_include` / `_revinclude` and `$everything`) run on
  a pool of read-only index connections, off the async worker threads, waiting for a free
  connection before taking a thread. A long search —
  an exact total over millions of matches — no longer holds up other searches or the
  writer, and a long write (an `$import` batch) no longer holds up searches.

## 0.15.1 — 2026-11-26

### Fixed
- `sazare-server stop` could fall back to terminating the server (SIGTERM / `taskkill`)
  instead of asking it to stop: the control port judged the first network segment of
  the request alone, and the token header could arrive in a later one. It now reads the
  whole request, and `stop` sends it in one write.
- Basic-auth passwords (`password_hash`) are verified off the async worker threads, at most
  four at a time: a burst of wrong passwords — loopback addresses (reverse proxies) are not
  locked out — could otherwise occupy every worker, and each Argon2 check also takes
  ~19 MB of memory.
- A malformed `password_hash` (no salt or hash part) is a configuration error at startup,
  not a login that always fails.
- Empty `SAZARE_CONFIG`, `SAZARE_DATA_DIR`, `SAZARE_APPS_DIR`, `SAZARE_IG_DIR` mean unset
  (as compose files and unit templates often pass them).
- `auth hash-password` / `auth new-api-key` quote the names they print, so any name gives
  valid YAML; a hashing error is reported instead of printing an empty hash.

## 0.15.0 — 2026-11-26

### Changed
- **Commands instead of a bare server** (breaking): `sazare-server start` runs the server
  in the background and returns once it is up; `stop` shuts it down cleanly; `status`
  shows its URL and pid; `run` keeps it in the foreground (Docker, systemd, launchd,
  `brew services`); `backup <dir>` replaces `--backup <dir>`. `sazare-server` alone prints
  the help. A server in the foreground could be killed by an accidental Ctrl+C or a
  closed terminal; a background one can't.
- **One server per data directory**: a lock file keeps a second server off the same
  databases (it used to be possible, and unsafe).
- **Unknown configuration keys are an error** (a typo like `enable: true` left auth off,
  silently). The deprecated `plugins` key is removed.
- **Locations no longer depend on where sazare is started**: the configuration file is
  `--config`, `$SAZARE_CONFIG`, `./config.yaml` or the user configuration file; relative
  paths in it are relative to the file; data defaults to the user data directory; IG
  directories to the configuration file's directory (`ig.dir`, `SAZARE_IG_DIR`). The
  startup log states each. Existing `./data` / IG directories in the current directory
  are still used, with a warning.
- **Hashed credentials**: `key_sha256` for API keys and Argon2id `password_hash` for users,
  generated by `sazare-server auth new-api-key` / `auth hash-password`. Plaintext is
  warned about.
- `sazare-server logs [-f] [-n N]`; README: starting at boot or login (`brew services`,
  systemd, launchd, Windows Task Scheduler).
- Documentation is English only: the Japanese section of the README is gone, and sample
  names in the examples, the sample app, the benchmark data and the tests are no longer
  Japanese.
- **`examples/jp-core/` became `examples/ig/`**: a generic implementation-guide loader
  (`setup.sh <package-url>…`, `--terminology`, `--terminology-package`) and example
  loader, with JP Core as the worked example. JP Core is still checked end to end in CI
  (`ci/jp_core_check.sh` takes its search values from the package's own examples). The
  unused copies of JP Core examples under `sazare-core/tests/fixtures/` are gone.

### Behavior changes
- String `:exact` is case-sensitive, as FHIR defines it: `family:exact=Smith` no longer
  matches `SMITH` or `smith`. (It compared lower-cased values.) Plain and `:contains`
  string searches stay case-insensitive. String values are now indexed as written, so the
  first start after upgrading rebuilds the search index once.

## 0.14.0 — 2026-11-26

### Performance
Measured on 10M resources (300k Patients, 9.6M Observations, 68 GB):
- **Startup no longer scans the database**: it checked whether the index was empty with
  `COUNT(*)` and ran `PRAGMA optimize` (statistics) on every start — together over ten
  minutes at this size. Existence checks and no statistics pass: 0.3 s. The indexes are
  built to plan well without statistics.
- **Multi-parameter searches narrow first**: the most selective kind of parameter is
  evaluated first, and once the candidates are few, the remaining parameters are checked
  only against them. `Observation?patient=X&code=Y` 1,023 ms → 1 ms;
  `…&date=ge2020-01-01` 5,346 ms → 0.7 ms. `:not` / `:missing` complement against the
  candidates instead of the whole type.

## 0.13.0 — 2026-11-26

Security release: a second review (an external model's, verified line by line) found
authorization gaps the 0.11.0 hardening missed.

### Changed
- **Plugins are now "static apps", served under `/apps/<name>/`** instead of at the top
  level, and only when a directory is configured (`apps.dir` / `SAZARE_APPS_DIR`; there
  is no implicit `./plugins` any more). Apps can no longer collide with FHIR or server
  routes, so the reserved-name rules are gone. The listing moved from `/$plugins` to
  `/$apps`. `plugins.dir` / `SAZARE_PLUGIN_DIR` still work, with a deprecation warning.

- **The Docker image runs the server as an unprivileged user** (`sazare`, uid 10001). On
  start, the entrypoint hands the data directory to that user — volumes written by
  earlier images (as root) keep working with no manual step — and then drops root.
  Started with `--user`, it runs as that user and leaves ownership alone.

### Security
- Dashboard: values from stored resources and the audit log (names, ids, resource types)
  are HTML-escaped, and click targets no longer embed them in inline JavaScript. A
  resource with markup in, e.g., `Patient.name.text` could run script in an operator's
  browser.
- Conditional create (`If-None-Exist`, also in Bundles) and conditional update/delete only
  match resources the caller may see: a patient-scoped token could get another patient's
  resource back, or learn that it exists.
- Patient-scoped access can no longer create, change or delete resources outside the
  patient compartment (Practitioner, Organization, Medication, ...). Reading them for
  reference resolution still works.
- SMART scopes are read with their context and per permission:
  - The patient-compartment restriction is decided per resource type and permission, so
    a token mixing `patient/` and `user/` scopes (e.g. `patient/Observation.rs
    user/Observation.c`) no longer escapes it for what it may do only as the patient.
  - Bulk Data (`$export`, `$import`) requires a `system/` scope; `user/*.read`/`write`
    no longer passes.
  - SMART v2 permissions (`.c` `.r` `.u` `.d` `.s` and combinations such as `.rs`,
    `.cruds`) are understood; v2 granular scopes with a query (`?category=...`) grant
    nothing instead of everything of the type.
  - `POST /{type}/_search` and `$validate` need read/search, not write.
- `_include`, `_revinclude` and `$everything` leave out resource types the token's scopes
  don't allow reading.
- `/ws` needs Subscription read access, and patient-scoped tokens can't listen.
- `POST /token`: a backend client without `allowed_scopes` is granted no scopes (it used to
  get whatever it asked for); failed token requests count toward the failed-login lockout.
- A plugin can't be named `metrics` (it would have shadowed `/metrics`).

### Availability
- HTTPS: TLS handshakes run concurrently with a 10 s timeout. A client that opened a
  connection and never finished the handshake used to stop the server from accepting
  anyone else.
- Outbound calls (JWKS for JWT validation, SMART client JWKS, webhooks) have connect and
  total timeouts, and the JWKS fetch no longer holds a lock that every JWT request waits on.
- JWTs naming a `kid` are verified with that key only; an unknown `kid` refetches the JWKS
  once (picking up a key rotation) instead of falling back to the first key. A token
  without `kid` is accepted only when the JWKS has a single key.
- Async `$export` jobs expire an hour after finishing, and at most 8 are held at once
  (429 with `Retry-After` beyond that); the export is built off the async worker threads.
- `$browse` pages are capped at 100; `public_key_file` is read once, not per request.
- Subscription criteria are evaluated against the changed resource alone (an in-memory
  index of just that resource), so a write no longer costs a search over the whole type;
  criteria with chains or `_has` still use the full index.
- The TLS accept task stops — and releases the port — as soon as the server stops.
- Expired export jobs are dropped on any lookup (status/file → 404), not only when the
  next export starts; the synchronous export also runs off the async worker threads.

### Fixed
- Patient-scoped searches narrow to the compartment before paging, so `total`, the page and
  the `next` link are right (another patient's matches on the first page used to hide
  later ones).
- `$everything` covers every Patient-compartment resource type (it had a fixed list of 9)
  and answers with `application/fhir+json`.
- REST create/update keep every `meta` element (`tag`, `security`, `extension`); they were
  dropped, so `_tag` / `_security` missed resources written through REST.
- Re-creating a deleted resource continues its version history instead of overwriting
  version 1 (and the history table now refuses to overwrite any version).
- A create with a client-chosen id is atomic (two racing creates: one gets 409).
- `PUT` and batch/transaction entries refuse a body whose `resourceType` or `id` differs
  from the URL (400).
- `PATCH` can't change `resourceType` / `id` (422), validates a patched Subscription,
  stamps `meta` even when the patch removed it, and is a compare-and-swap (409 when the
  resource changed meanwhile).
- Subscriptions written through batch/transaction, conditional update or `$import` get
  the same validation as through REST.
- A history Bundle without versions omits `entry` instead of sending `[]`; audit entries
  record PATCH as an update.

## 0.12.0 — 2026-11-25

### Added
- `GET /metrics`: Prometheus metrics (requests and latency by route template, resources by
  type, database sizes, version, uptime). Operator-only when auth is enabled.
- Standard R4 reference search parameters the built-in lists lacked (e.g.
  `Observation.performer`, `MedicationRequest.medication`, `Patient.general-practitioner`,
  `Encounter.participant`), so chaining, `_include` and `_revinclude` through them work.
- The search index is rebuilt automatically on start when the set of search parameters
  changed (new built-ins after an upgrade, or an added/edited `searchparameters/` file).
- CI: Inferno US Core v7 runs alongside v8; a backup → restore round trip is tested.

### Behavior changes
- `_include` / `_revinclude` naming a parameter that is not a reference parameter of the
  source type is a 400 (it used to include nothing, silently).
- Chained searches deeper than 4 reference hops are a 400.

## 0.11.0 — 2026-11-25

Security release: authorization hardened throughout.

### Security
- Patient compartment enforced for patient-scoped tokens on every path: history/vread,
  update and PATCH content, conditional update, `_include`/`_revinclude` results.
- Subscriptions: patient-scoped tokens may not create or change them; `channel.endpoint`
  must be an absolute http(s) URL; rest-hook delivery reaches public addresses only, follows
  no redirects, times out after 30 s.
- `$reindex` / `$demo` are operator-only (API key, Basic user, or `system/*.write`).
- Bundles authorize every entry like the equivalent direct request.
- State-changing requests relying on browser-cached Basic credentials from another site are
  refused.
- Failed logins are audit-logged; 10 failures/minute from one address → 429 (loopback
  exempt).
- Plugin names that could shadow API routes are rejected; WebSocket `bind` only to existing
  websocket Subscriptions; the SMART token endpoint refuses to issue tokens when it can't
  verify the assertion audience.

### Behavior changes
- The binary listens on **127.0.0.1** by default (Docker keeps `0.0.0.0`). Set
  `server.host: "0.0.0.0"` — with auth enabled — to serve the network.
- With auth enabled, Subscriptions can't deliver to loopback/private addresses unless
  `subscription.allow_private_endpoints: true`.
- `POST /` (Bundles) accepts API keys and JWTs, not only Basic.

## 0.10.0 — 2026-11-24

### Added
- Full-text search: `_content` (whole resource) and `_text` (narrative). Case-insensitive
  substring matching, so Japanese works. Stored in `search_index.fts.sqlite`, built on the
  first start for existing data.
- `_total=none`.

### Changed
- `$import` writes the search index in batches (63.5k resources: 29.5 s → 16.6 s);
  `$reindex` uses one transaction per resource.
- `_sort` on narrowed result sets reads only those resources' values.

## 0.9.0 — 2026-11-24

### Added
- `validation.default_profiles`: validate resources that declare no `meta.profile` against a
  profile per type (`enforce` or `warn`).
- `_sort` (`_id`, `_lastUpdated`, any search parameter; `-` for descending).

### Behavior changes
- Without `_count`, searches return pages of **100** (they returned every match, although the
  links advertised 100). `_count` is capped at 1000.

## 0.8.0 — 2026-11-24

### Added
- JP Core code validation (opt-in): `examples/jp-core/setup.sh --terminology` loads JP Core
  and jpfhir-terminology; required bindings, including slices, are checked against complete
  code systems.
- Number search for custom SearchParameters of type `number`.
- CI: JP Core end-to-end job.

### Fixed
- `$export`: a malformed `_since` is a 400 (it was ignored, exporting everything).

## 0.7.0 — 2026-11-24

### Added
- JP Core guide and scripts (`examples/jp-core/`): all 8 JP Core search parameters work.
- `--backup <dir>` (online, consistent), `--config <path>`, `server.max_request_mb`.
- Custom search parameters: `where(... and ...)`, `where(resolve() is X)`, `[n]`.
- CI: Inferno US Core v8, search benchmark.

### Changed
- Search 30–600× faster on larger datasets (covering indexes; migrated automatically).

### Fixed
- Request bodies over 2 MB were rejected regardless of configuration.
- Custom search parameters were queried as strings (date prefixes, token `system|code`).
- Chain and `_has` ignored date prefixes; `$export` `_since` with an unencoded `+`.

### Behavior changes
- A `+` in a search value means a space (except in dates, where it stays a literal `+`).

## Earlier releases

0.1.0 – 0.6.6: see [GitHub Releases](https://github.com/fu-foo/fhir-sazare/releases).

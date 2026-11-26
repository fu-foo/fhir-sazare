# sazare

**A lightweight FHIR R4 server built with Rust**

*Lightweight, single-binary FHIR server powered by SQLite. No external database required.*

> **sazare** (細石 / さざれ) refers to small pebbles in Japanese — tiny stones that, as a poem from the *Kokin Wakashū* (905 AD) says, gather over time to form great rocks. This server starts small but aims to grow into something solid and reliable.

---

## Features

- **FHIR R4 (4.0.1)** compliant REST API
- **Zero external dependencies** — SQLite embedded, single binary deployment
- **Full CRUD** — Create, Read, Update, Delete for all resource types
- **Version history** — `vread` and `_history` support
- **Bundle** — Transaction (all-or-nothing) and Batch processing with `urn:uuid:` reference resolution
- **Search** — Parameter-based search, chain search (`subject:Patient.name=...`), reverse chain (`_has:Observation:subject:code=...`), `_include`, `_revinclude`, `_sort`, full text (`_content`, `_text` — Japanese included)
- **Conditional operations** — Conditional create (`If-None-Exist`), update, and delete
- **Resource filtering** — `_summary` (5 modes) and `_elements` support
- **Validation** — Multi-phase validation against US Core profiles built in
- **Any implementation guide** — Load an IG's package (profiles, search parameters, code systems) at startup with one script; tested in CI with JP Core (all 76 package examples validate, all 8 search parameters work) — see [`examples/ig/`](examples/ig/README.md)
- **US Core conformance** — Passes the Inferno US Core v7 & v8 FHIR API test suites (`examples/us-core-seed.json` for v7, `examples/us-core-v8-seed.json` for v8; the TLS test requires an HTTPS deployment)
- **Custom search parameters** — Drop FHIR `SearchParameter` resources into a `searchparameters/` directory; their FHIRPath `expression` is compiled by a bounded evaluator (unsupported expressions are rejected at load, never mis-evaluated)
- **Bulk data** — NDJSON `$import`, and `$export` both synchronous and async (FHIR Bulk Data Access IG: `Prefer: respond-async` kick-off, status poll, manifest, `_type`/`_since`/`_outputFormat`)
- **Static app hosting** — Serve single-page apps that use the API, same-origin, at `/apps/<name>/`
- **Web dashboard** — Built-in console at `/`: browse resources, a search builder that shows the generated FHIR URL, one-click sample data — no build step, served from the binary
- **Audit logging** — All operations recorded to dedicated SQLite database
- **PATCH** — JSON Patch (RFC 6902)
- **$everything** — Patient compartment operation
- **Subscription** — REST-hook and WebSocket (R4 `bind`/`ping` at `/ws`) notifications on resource changes
- **Webhooks** — Lifecycle event hooks (`BundleCreated`, `TaskCompleted`) to configured endpoints
- **Authentication** — API key, Basic auth, JWT (HS256/RS256/JWK URL), SMART on FHIR scopes
- **Compartment access control** — Patient-scoped token restricts access to own data
- **TLS/HTTPS** — Optional TLS via config
- **Graceful shutdown** — Clean shutdown on SIGTERM / Ctrl+C

---

## Where sazare fits

sazare is for **learning FHIR, prototypes and PoCs, local development, and
small deployments** — places where a single binary with an embedded database
beats running a Java application server and an RDBMS. For large production
workloads, other FHIR versions or full terminology services, use a mature
server such as [HAPI FHIR](https://hapifhir.io/).

| | sazare | HAPI FHIR (JPA server) |
|---|---|---|
| Runtime | Single native binary (~15 MB), or Docker | Java application on the JVM, or Docker |
| Database | Embedded SQLite, nothing to set up | External RDBMS (e.g. PostgreSQL); H2 for development |
| FHIR versions | R4 (4.0.1) | DSTU2 – R5 |
| Search | Common parameters, chain, `_has`, `_include`/`_revinclude`, custom SearchParameters (FHIRPath subset) | Full search, including full FHIRPath and more modifiers |
| Validation | Declared profiles: cardinality, types, extensions, required bindings (built-in value sets, plus an IG's complete code systems from `terminology/`) | Full HL7 validator |
| Terminology | No `$expand` / `$validate-code` / `$lookup` | Terminology services |
| Conformance check | Inferno US Core v7/v8 FHIR API suites pass, run in CI | Widely used reference implementation |

### Limitations

Know these before you rely on sazare:

- **FHIR R4 (4.0.1) only.** No R4B / R5.
- **Search:** the common parameter types (string, token, date, reference, number) with
  `:exact`, `:contains`, `:missing`, `:not`; chains up to 4 hops; one level of `_has`.
  Not supported: `quantity`/`uri`/`composite` parameters, `_filter`, `_list`, `_query`,
  nested `_has`, `:above`/`:below`/`:in`/`:text` modifiers. For resource types with a
  built-in parameter list, unknown parameters are a 400, never silently ignored.
- **Custom SearchParameters** use a FHIRPath subset (paths, `|`, `ofType`/`as`,
  `extension()`, `where(a = 'x' and ...)`, `resolve() is`, `[n]`); others are rejected at load.
- **Validation** checks declared (or default) profiles — cardinality, types, fixed values,
  extensions, required bindings to complete code systems — but is not the full HL7
  validator (no FHIRPath invariants, no terminology service, no `$expand` /
  `$validate-code` / `$lookup`).
- **Scale:** one process with SQLite; writes go through a single index lock, so it does
  not scale horizontally or for highly concurrent writes. Benchmarked (in CI) on 60k
  resources, where searches stay well under 100 ms; larger datasets are untested — measure
  with `bench/run.sh <patients> <obs-per-patient>` before relying on it.
- **Subscriptions:** R4 rest-hook and websocket, best-effort delivery (a few retries,
  no persistent queue).
- **Auth:** API keys, Basic, JWT (HS256/RS256/JWKS) with SMART v1 and v2 scopes, SMART
  Backend Services. No built-in OAuth authorization server for user login (use an
  external IdP). SMART v2 granular scopes (`?category=...`) are not evaluated and grant
  nothing.

---

## Quick Start — run a FHIR server in 30 seconds

Download one file, run it, and you're looking at live FHIR data. No Docker, no
JVM, no database, no config.

**On macOS or Linux, Homebrew is the smoothest path** — it also sidesteps the
macOS Gatekeeper warning below, because brew-installed binaries aren't
quarantined:

```bash
brew install fu-foo/tap/sazare
sazare-server start --demo --open
```

**On Windows, [Scoop](https://scoop.sh) is the smoothest path** — it likewise
sidesteps the SmartScreen warning below, because Scoop downloads aren't tagged
with the Mark-of-the-Web:

```powershell
scoop bucket add fu-foo https://github.com/fu-foo/scoop-bucket
scoop install sazare
sazare-server start --demo --open
```

**Otherwise, download the binary for your OS** from the
[latest release](https://github.com/fu-foo/fhir-sazare/releases/latest)
(macOS Intel/Apple Silicon, Linux x86-64/ARM64, Windows x86-64), then unpack and
run it with sample data:

```bash
# macOS / Linux (Apple Silicon shown — pick the asset matching your OS/arch)
tar xzf sazare-server-macos-arm64.tar.gz
./sazare-server start --demo --open
```

```powershell
# Windows: unzip, then
.\sazare-server.exe start --demo --open
```

`start` runs the server in the background and returns once it is up;
`--demo` pre-loads a few sample patients with vitals, a condition, an encounter,
and a prescription; `--open` launches the built-in dashboard in your browser.
That's the whole setup. `sazare-server stop` stops it (cleanly — in-flight writes
finish), `sazare-server status` shows where it runs. See
[Running the server](#running-the-server).

> **macOS first run** (direct download only): the binary is unsigned, so
> Gatekeeper blocks it with an "unidentified developer" dialog. Clear the
> quarantine flag and run:
> ```bash
> xattr -d com.apple.quarantine ./sazare-server && ./sazare-server start --demo --open
> ```
> Or allow it via **System Settings → Privacy & Security → "Open Anyway"** (on
> macOS 15 Sequoia the old right-click → Open shortcut no longer works).
> Installing with `brew` avoids this entirely.

> **Windows first run** (direct download only): because the `.exe` is unsigned,
> Microsoft Defender SmartScreen shows a blue **"Windows protected your PC"**
> box, and the only obvious button is *Don't run*. This is the Windows
> equivalent of the macOS dialog above — it means "downloaded from the internet
> and not yet recognized", not "this is malware". Two ways through:
>
> - **Click through:** in the dialog, click **More info**, then the **Run
>   anyway** button that appears.
> - **Clear it once (PowerShell):** unblock the file so it never prompts again —
>   the counterpart to macOS's `xattr` command:
>   ```powershell
>   Unblock-File .\sazare-server.exe
>   .\sazare-server.exe start --demo --open
>   ```
>   (GUI equivalent: right-click the `.exe` → **Properties** → tick **Unblock**
>   at the bottom → **OK**.)
>
> Installing with `scoop` avoids this entirely. SmartScreen also stops warning
> on its own once a build earns download reputation; signing the Windows and
> macOS binaries is on the road to 1.0.

> **Linux**: no such gate — download, `chmod +x sazare-server` if needed, and run.

The server listens on `http://localhost:8080` (no authentication by default). The
dashboard has a one-click "Load sample data" button and a search builder.

### Try it from the command line

```bash
# CapabilityStatement
curl http://localhost:8080/metadata

# Create your first resource
curl -X POST http://localhost:8080/Patient \
  -H "Content-Type: application/fhir+json" \
  -d '{
    "resourceType": "Patient",
    "name": [{"family": "Doe", "given": ["Jane"]}],
    "gender": "female",
    "birthDate": "1990-01-01"
  }'

# Search for it
curl "http://localhost:8080/Patient?name=Doe"
```

### Sample data — make search *visible*

An empty server is hard to learn from: every query returns nothing. The richer
**demo cohort** is five legible patients (a diabetic, a hypertensive smoker, a
prenatal visit, a well-child vaccine schedule, an oncology work-up) with real
LOINC/SNOMED/RxNorm codes — enough that `_has`, `_include` and `$everything`
visibly *do something*. Load it either way:

```bash
# into a running server, over HTTP
bash examples/demo/load_demo.sh

# or at startup, only if the store is empty (great for Docker)
SAZARE_SEED_ON_EMPTY=examples/demo/cohort.json sazare-server start
```

Then try the queries that an empty server can't show you:

```bash
# every patient who has an HbA1c result (reverse chain)
curl "http://localhost:8080/Patient?_has:Observation:patient:code=4548-4"

# one patient's entire chart in a single call
curl "http://localhost:8080/Patient/demo-ann-davis/\$everything"
```

For a realistic *population* (pagination, `$export`, dashboards), generate one
with Synthea — see [`examples/demo/`](examples/demo/) for both the hero cohort
and the Synthea bulk-load path. (The built-in `--demo` flag and the dashboard's
"Load sample data" button use a tiny embedded two-patient set for a zero-file
taste.)

### Build from source (optional)

Prefer to build it yourself? You'll need **Rust 1.85+** (2024 edition):

```bash
git clone https://github.com/fu-foo/fhir-sazare.git
cd fhir-sazare
cargo build --release
./target/release/sazare-server start --demo --open
```

---

## Running the server

```bash
sazare-server start [--config config.yaml] [--demo] [--open]   # in the background; returns once it is up
sazare-server status                                          # running? URL, pid, log
sazare-server stop                                            # clean shutdown (in-flight writes finish)
sazare-server run [--config config.yaml]                      # in the foreground
sazare-server backup <dir> [--config config.yaml]             # consistent copy of the databases
```

- **One server per data directory.** A second `start` reports the running one; a second
  `run` refuses to start. (`sazare.lock` in the data directory; the OS releases it however
  the process ends.)
- A background server logs to `<data_dir>/logs/sazare.log` (one previous file is kept
  after 50 MB). `stop` asks it to shut down through a loopback control port with a
  per-start token (`<data_dir>/sazare.run.json`, readable only by you); nothing extra is
  exposed on the server's own port.
- **Service managers** (Docker, systemd, launchd, `brew services`) use `run`, which stays
  in the foreground and stops on SIGTERM. The Docker image already does.
- Pass the same `--config` to `status` / `stop` / `backup` that you passed to `start`: it
  tells them which data directory to look at.

## Configuration

Copy the example config and customize:

```bash
cp config.example.yaml config.yaml
```

```yaml
server:
  host: "127.0.0.1"       # default: this machine only; "0.0.0.0" for the network (enable auth!)
  port: 8080
  max_request_mb: 64      # Max request body (Bundles, $import NDJSON); 413 above it

auth:
  enabled: false          # Set true to require authentication
  api_keys:
    - name: "my-client"
      key: "your-secret-key"
  basic_auth:
    - username: "admin"
      password: "secure-password"

storage:
  data_dir: "data"        # SQLite files stored here

log:
  level: "info"           # trace, debug, info, warn, error

webhook:
  enabled: false
  endpoints:
    - url: "https://example.com/webhook"
      events: ["BundleCreated", "TaskCompleted"]

validation:
  default_profiles:       # for resources that declare no meta.profile
    Patient: "http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"
  default_profile_mode: enforce   # or warn: accept, report violations
```

`validation.default_profiles` validates resources that don't declare a
`meta.profile` against a profile per resource type — real-world data often
declares none, so declared-profile validation alone never runs on it. The
check runs on a copy; the stored resource is not modified. `enforce` rejects
violations exactly as if the profile were declared; `warn` accepts the
resource and reports the violations (`$validate` and the server log), which
helps while migrating existing data. A resource that declares its own
profile is validated against that one only.

If no `config.yaml` is found, the server runs with sensible defaults (port 8080, auth disabled).

---

## API Endpoints

### Resource Operations

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/metadata` | CapabilityStatement |
| `POST` | `/{type}` | Create resource |
| `GET` | `/{type}/{id}` | Read resource |
| `PUT` | `/{type}/{id}` | Update resource |
| `DELETE` | `/{type}/{id}` | Delete resource |
| `GET` | `/{type}/{id}/_history` | Version history |
| `GET` | `/{type}/{id}/_history/{vid}` | Read specific version |
| `PATCH` | `/{type}/{id}` | Patch resource (JSON Patch) |
| `GET` | `/{type}?params` | Search |
| `POST` | `/{type}/$validate` | Validate resource |
| `GET` | `/Patient/{id}/$everything` | Patient compartment |

### System Operations

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/` | Bundle (transaction / batch) |
| `GET` | `/$export` | System bulk export — sync NDJSON, or async with `Prefer: respond-async` |
| `GET` | `/Patient/$export` | Patient-compartment bulk export |
| `GET` | `/Group/{id}/$export` | Group-members' bulk export |
| `GET`/`DELETE` | `/$export-status/{job}` | Async export job status / cancel |
| `GET` | `/$export-file/{job}/{type}` | Download an async export NDJSON file |
| `POST` | `/$import` | Bulk import (NDJSON) |

### Dashboard

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Web dashboard (browser) |
| `GET` | `/$status` | Server status JSON API |

### Static apps

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/$apps` | List hosted apps (JSON) |
| `GET` | `/apps/{name}/` | Serve an app's `index.html` |
| `GET` | `/apps/{name}/{path}` | Serve an app's files (SPA fallback) |

---

## Search

### Basic Search

```bash
# Search by parameter
curl "http://localhost:8080/Patient?name=Doe"

# With pagination (default page size 100, max 1000; follow the Bundle's next link)
curl "http://localhost:8080/Patient?_count=10&_offset=0"

# Sorting: comma-separated keys, '-' for descending
curl "http://localhost:8080/Observation?patient=123&_sort=-date"
curl "http://localhost:8080/Patient?_sort=family,birthdate"

# Leave Bundle.total out
curl "http://localhost:8080/Observation?patient=123&_total=none"

# Summary and elements
curl "http://localhost:8080/Patient?_summary=true"
curl "http://localhost:8080/Patient?_elements=name,gender"
```

`_sort` accepts `_id`, `_lastUpdated` and any search parameter of the
resource type (built-in or custom). A resource with several values sorts by
its lowest value ascending and its highest descending; resources without a
value come last; ties fall back to the id, so paging is stable.

### Full-text search

```bash
# _content: anywhere in the resource; _text: the narrative only
curl "http://localhost:8080/Condition?_content=diabetes"
curl "http://localhost:8080/Observation?_content=heart%20rate"        # every term must occur
curl "http://localhost:8080/Observation?_content=%22heart%20rate%22"   # "quoted" = phrase
curl "http://localhost:8080/Patient?_text=Shaw"
```

Matching is case-insensitive **substring** matching (SQLite FTS5 with the
trigram tokenizer), so it also works for languages written without spaces
between words (Japanese, Chinese, …) — and `MRN42` also matches `MRN420`. Whitespace-separated
terms must all occur; commas separate alternatives (OR), as for any
parameter. Terms of three or more characters use the index; shorter ones
(e.g. a two-character surname) are still found, by a scan. An index built by
an older version gets its full text on the first start.

### Chain Search

Search by referenced resource attributes (multi-level up to 4 hops, type required at each hop):

```bash
# Find Observations where the subject (Patient) has name "Doe"
curl "http://localhost:8080/Observation?subject:Patient.name=Doe"

# Multi-level: Conditions whose Encounter's subject (Patient) is named "Doe"
curl "http://localhost:8080/Condition?encounter:Encounter.subject:Patient.name=Doe"
```

### Reverse Chain (`_has`)

The mirror of chain search: filter resources by a property of other resources
that reference *them*. Form: `_has:{Type}:{reference-param}:{search-param}`.

```bash
# Patients that have an Observation with LOINC code 29463-7 (body weight)
curl "http://localhost:8080/Patient?_has:Observation:subject:code=29463-7"

# Composable with ordinary parameters (AND)
curl "http://localhost:8080/Patient?gender=male&_has:Observation:subject:code=29463-7"
```

### Custom search parameters

Define your own search parameters without rebuilding. Drop a FHIR
`SearchParameter` resource into a `searchparameters/` directory next to the
binary; its `expression` (FHIRPath) is compiled at startup by a bounded
evaluator, and the parameter then works like any built-in one. This is also how
an implementation guide's search params are supplied, alongside its profiles in
`profiles/`, with nothing baked into the core.
When the set of parameters changes (a file added or edited, or new built-ins
after an upgrade), the next start rebuilds the index automatically so existing
data is covered; `POST /$reindex` does the same without a restart.

```jsonc
// searchparameters/name-phonetic.json
{
  "resourceType": "SearchParameter",
  "code": "name-phonetic",
  "base": ["Patient"],
  "type": "string",
  "expression": "Patient.name.where(extension('http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation').value.ofType(code)='SYL').text"
}
```

```bash
curl "http://localhost:8080/Patient?name-phonetic=Chen"
```

The evaluator covers path navigation, `|` unions, choice types (`as` /
`ofType`), `extension('url')`, `where(field = 'literal')`, reference type
filters (`where(resolve() is Patient)`) and `[n]` indexers — 1479 of the 1480
expressions in base R4 plus US Core 8.0.0. Results are indexed by type the
same way as built-in parameters (HumanName/Address parts for string, Period
ranges for date, bare ids for reference, booleans for token). Supported types
are `string`, `token`, `date`, `reference` and `number` (`gt`/`lt`/`ap`/… with
FHIR precision: `probability=0.8` matches [0.75, 0.85)). Expressions
outside the subset (e.g. `resolve()` navigation, boolean logic) are rejected at
load — never silently mis-evaluated into wrong results.

**Implementation guides**: `bash examples/ig/setup.sh <package-url>` downloads an
IG's package into `profiles/` and `searchparameters/` (and with `--terminology`,
its code systems into `terminology/`, so required bindings are validated). See
[`examples/ig/README.md`](examples/ig/README.md) for Docker, default profiles and
what is checked.

### Conditional Create

Prevent duplicate creation using search criteria:

```bash
curl -X POST http://localhost:8080/Patient \
  -H "Content-Type: application/json" \
  -H "If-None-Exist: identifier=http://example.org|12345" \
  -d '{"resourceType":"Patient","identifier":[{"system":"http://example.org","value":"12345"}]}'
```

---

## Bundle (Transaction / Batch)

### Transaction (All-or-Nothing)

All entries succeed or all are rolled back. Supports `urn:uuid:` reference resolution between entries.

```bash
curl -X POST http://localhost:8080/ \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Bundle",
    "type": "transaction",
    "entry": [
      {
        "fullUrl": "urn:uuid:patient-1",
        "resource": {
          "resourceType": "Patient",
          "name": [{"family": "Doe"}]
        },
        "request": {"method": "POST", "url": "Patient"}
      },
      {
        "fullUrl": "urn:uuid:obs-1",
        "resource": {
          "resourceType": "Observation",
          "status": "final",
          "code": {"coding": [{"system": "http://loinc.org", "code": "29463-7"}]},
          "subject": {"reference": "urn:uuid:patient-1"}
        },
        "request": {"method": "POST", "url": "Observation"}
      }
    ]
  }'
```

The `urn:uuid:patient-1` reference in the Observation is automatically resolved to the assigned Patient ID.

### Batch (Independent)

Each entry is processed independently. Failures in one entry do not affect others.

```bash
curl -X POST http://localhost:8080/ \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Bundle",
    "type": "batch",
    "entry": [
      {
        "resource": {"resourceType": "Patient", "name": [{"family": "Smith"}]},
        "request": {"method": "POST", "url": "Patient"}
      },
      {
        "resource": {"resourceType": "Patient", "name": [{"family": "Johnson"}]},
        "request": {"method": "POST", "url": "Patient"}
      }
    ]
  }'
```

---

## Bulk Data (NDJSON)

### Export

Synchronous (returns NDJSON directly):

```bash
# Export all resources
curl http://localhost:8080/\$export

# Export specific types
curl "http://localhost:8080/\$export?_type=Patient,Observation"
```

Asynchronous (FHIR Bulk Data Access IG — kick-off / poll / download):

```bash
# 1. Kick-off: returns 202 with a Content-Location status URL
#    System-level, or patient-level (/Patient/$export), or group-level
#    (/Group/{id}/$export — only the group's member patients' compartments)
curl -i "http://localhost:8080/\$export?_since=2024-01-01T00:00:00Z" \
  -H "Prefer: respond-async"

# 2. Poll the status URL: 202 while running, 200 with a manifest when done
curl http://localhost:8080/\$export-status/<job-id>
#    -> { "transactionTime": ..., "output": [{ "type": "Patient", "url": ... }], ... }

# 3. Download each NDJSON file from the manifest's output URLs
curl http://localhost:8080/\$export-file/<job-id>/Patient

# Cancel a job
curl -X DELETE http://localhost:8080/\$export-status/<job-id>
```

Supports `_type`, `_since`, and `_outputFormat` (NDJSON).

### Import

```bash
# Import from NDJSON
curl -X POST http://localhost:8080/\$import \
  -H "Content-Type: application/x-ndjson" \
  -d '{"resourceType":"Patient","name":[{"family":"Doe"}]}
{"resourceType":"Patient","name":[{"family":"Smith"}]}'
```

---

## Static apps

sazare can host static web apps — typically single-page apps that use its FHIR API — at
`/apps/<name>/`. Because they are served from the same origin as the API, they need no
CORS setup. This is file hosting, not a plugin system: an app can't change what the server
does, and it reaches data only through the FHIR API and its authorization.

Nothing is served unless you configure a directory:

```yaml
apps:
  dir: "apps"    # each subdirectory is one app
```

or `SAZARE_APPS_DIR=./apps`. (The Docker image sets it to the bundled `/apps`.)

```
apps/
  my-app/           → served at /apps/my-app/
    index.html      # Entry point
    style.css
    app.js
```

- **SPA fallback** — paths that don't name a file return the app's `index.html`
- **Cache-Control** — `index.html` with `no-cache`; other files with `max-age=604800`
- **No authentication** for the files themselves; the app's API calls are authorized as usual
- **Safety** — app names are `[A-Za-z0-9_-]`; path traversal and symlinks are refused; apps
  live under `/apps/`, so they can never shadow a FHIR or server route
- **Listing** — `GET /$apps`

A sample app (`apps/sample-patient-register/`), a Patient registration form with a list,
is included: run with `SAZARE_APPS_DIR=apps` and open `/apps/sample-patient-register/`.

---

## Architecture

```
fhir-sazare/
  sazare-core/     # FHIR types, validation, search parameter parsing
  sazare-store/    # SQLite persistence (resources, search index, audit)
  sazare-server/   # Axum HTTP server, handlers, middleware
```

### Technology Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (2024 edition) |
| HTTP server | Axum 0.8 |
| Async runtime | Tokio |
| Database | SQLite (rusqlite, bundled) |
| Config | YAML (serde_yaml) |
| JSON Patch | json-patch (RFC 6902) |

### Storage

Three separate SQLite databases for clean separation of concerns:

- **`resources.sqlite`** — Resource data with version history
- **`search_index.sqlite`** — Search parameter index
- **`search_index.fts.sqlite`** — Full text for `_text` / `_content` (derived data, like the index; kept in its own file because FTS writes in the shared file made every write ~10× slower)
- **`audit.sqlite`** — Audit log entries

### Upgrading

See [UPGRADING.md](UPGRADING.md) (back up, replace the binary, watch the first start)
and [CHANGELOG.md](CHANGELOG.md) for what changed in each version, including behavior
changes.

### Monitoring

`GET /metrics` serves Prometheus metrics: requests and latency by method,
route template and status (`sazare_http_requests_total`,
`sazare_http_request_duration_seconds`), stored resources by type
(`sazare_resources`), database file sizes (`sazare_database_size_bytes`),
version and uptime. With auth enabled it needs operator credentials (an API
key, a Basic user or a `system/*.write` token), since it reveals data volumes:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: sazare
    authorization: { credentials: "<api key>" }
    static_configs: [{ targets: ["sazare:8080"] }]
```

`GET /health` remains the unauthenticated liveness check.

### Backup and restore

`sazare-server backup <dir>` writes a consistent copy of all three databases into `<dir>`
and exits. It is safe to run while the server is serving the same `data_dir`
(it uses SQLite `VACUUM INTO`, so the copy includes not-yet-checkpointed WAL
data and never tears). Existing files in `<dir>` are never overwritten.

```bash
sazare-server backup backups/$(date +%Y%m%d-%H%M%S) --config config.yaml
```

To restore, stop the server and copy `resources.sqlite` and `audit.sqlite`
back into `data_dir`. Leave `search_index.sqlite` and `search_index.fts.sqlite`
out (delete the old ones): they are derived data, and the server rebuilds them
from the resources on startup, so the index is guaranteed to match the restored resources even if the backup was
taken mid-write.

---

## Development

```bash
# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run -- run

# Run with custom config
cargo run -- run --config path/to/config.yaml
```

### Benchmark

`bench/run.sh` starts a throwaway server, `$import`s a synthetic dataset and
prints the median latency of a fixed set of searches (`bench/queries.txt`) as a
Markdown table. CI runs it on every push and fails if any query exceeds 500ms.

```bash
cargo build --release
bench/run.sh 10000 5          # 10k Patients, 50k Observations
MAX_MS=500 bench/run.sh       # exit non-zero if a query is slower than 500ms
```

Reference numbers (10k Patients / 50k Observations, one desktop CPU): import
~2,200 resources/s; single-resource lookups and selective searches under 3ms;
the broadest searches (50k matches, `_has` over 5k sources) about 30ms.

---

## Roadmap

- [x] Runtime-loadable profiles (US Core embedded; other IGs from a `profiles/` directory)
- [x] Multi-level chain search
- [x] Reverse chain search (`_has`)
- [x] Subscription via WebSocket

---

## Supporting

If you find this project useful, consider supporting its development:

[![GitHub Sponsors](https://img.shields.io/github/sponsors/fu-foo?style=for-the-badge&logo=github&label=Sponsor)](https://github.com/sponsors/fu-foo)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-Support-ff5e5b?style=for-the-badge&logo=ko-fi)](https://ko-fi.com/fufoo)

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Bundled FHIR profiles (US Core) are third-party artifacts redistributed under
CC0-1.0 — see [NOTICE.md](NOTICE.md) for provenance and attribution.

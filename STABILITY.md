# Stability policy

> **Draft.** This is what sazare will promise from **1.0** on. Until then (0.x), a minor
> release may change behavior; every such change is listed under **Behavior changes** in
> [CHANGELOG.md](CHANGELOG.md).

sazare follows [Semantic Versioning](https://semver.org/). From 1.0:

| Release | May contain | You do |
|---|---|---|
| **Patch** (1.2.**3**) | Bug fixes and security fixes. No schema migration, no new configuration. | Replace the binary. Going back to an earlier patch of the same minor is safe. |
| **Minor** (1.**3**.0) | New features, additive only. Existing requests, configurations and data keep working the same way. May migrate the databases (forward-only). | Back up, replace the binary, watch the first start ([UPGRADING.md](UPGRADING.md)). |
| **Major** (**2**.0.0) | Anything on the list below may change. Each change is listed with what to do instead. | Read the upgrade notes first. |

## What is covered

A change to any of the following requires a **major** version.

### REST API

- Endpoints, HTTP methods and the operations they perform (`/{type}`, `/{type}/{id}`,
  `_search`, `_history`, `$validate`, `$everything`, `$export` / `$status`, `$import`,
  `$reindex` (202: rebuilds in the background), Bundle transaction/batch, `/metadata`,
  `/health`, `/metrics`, `$apps`, SMART `/.well-known/smart-configuration` and `/token`).
- Subscriptions: rest-hook delivery and the `/ws` WebSocket channel.
- Status codes for a given request — what succeeds stays a success, what is a 4xx stays a
  4xx of the same meaning.
- Search semantics: how each parameter type, modifier and prefix matches (comma-separated
  alternatives each with their own prefix; quantities compared in the units as written);
  `+` decoding; chaining, `_has`, `_include` / `_revinclude`; sort order by the requested
  keys (`_sort`, missing values last); `total` is exact, and left out — not counted —
  with `_total=none`.
- Defaults and limits that clients see: page size 100 when `_count` is absent, `_count`
  up to 1000, chain depth 4, the request size default. A minor release may raise a limit
  (a larger `_count`, a deeper chain), never lower one; the default page size stays.
- Authorization: which credentials and scopes may do what, and what a patient-scoped
  token can see. Tightening this for a security fix is the one exception (see below).

### Configuration

- `config.yaml` keys, their meaning and their defaults.
- Where the configuration is found: `--config`, then `$SAZARE_CONFIG`, then
  `./config.yaml`, then the user configuration file; relative paths in it are relative to
  the file; unknown keys are an error.
- Environment variables: `SAZARE_CONFIG`, `SAZARE_HOST`, `SAZARE_PORT`,
  `SAZARE_DATA_DIR`, `SAZARE_APPS_DIR`, `SAZARE_IG_DIR`, `SAZARE_UI_DIR`,
  `SAZARE_SEED_ON_EMPTY`, `SAZARE_LOG_FILE`; `RUST_LOG` (overrides `log.level`).
- A key is never renamed in place: the new name is added, the old one keeps working
  (with a warning in the log) until the next major.

### Command line

- The commands `run`, `start`, `stop`, `status`, `logs`, `backup <dir>`,
  `auth hash-password`, `auth new-api-key`, `version`, and the options `--config`,
  `--demo`, `--open`.
- Exit statuses: 0 on success; `backup` returns 0 only when a complete backup was
  written; `status` returns non-zero when no server is running.
- One server per data directory; `stop`, `status`, `logs` and `backup` find it through
  the same configuration.

### Data on disk

- Any 1.x release opens a `data_dir` written by any earlier 1.x release, and by 0.12 or
  later.
  Migrations run automatically on start.
- Migrations are **forward-only**: once a newer minor has opened a `data_dir`, older
  versions can't. To go back, restore the backup taken before upgrading.
- `resources.sqlite` and `audit.sqlite` hold your data and are migrated, never rebuilt.
  The search index (`search_index*.sqlite`, including its generations
  `search_index.gN.sqlite` and the full text) is derived: any release may rebuild it — in
  the background, the previous index answering searches until the switch — and you may
  always delete it to force a rebuild.
- A backup written by `sazare-server backup` restores into the same or any later 1.x
  release.

### Operations

- Prometheus metric names and label names in `/metrics` (new metrics and new label
  *values*, e.g. a new route, may appear in any minor).
- `/health` returning 200 with `{"status":"ok"}` while the server serves requests, and
  its `index.state` (`ready` / `rebuilding`).
- Static apps: with `apps.dir` set, each subdirectory is served at `/apps/<name>/`, with
  the SPA fallback and cache rules in the README; `GET /$apps` lists them.
- Docker tags: `X.Y.Z` never moves; `X.Y` follows the latest patch; `X` follows the latest
  minor of that major; `latest` follows the newest release. (`edge`, the newest `main`, is
  unreleased and promises nothing.)

## What is not covered

These may change in any release:

- **Additions.** New endpoints, resource types, search parameters (built-in or standard
  R4), config keys whose default keeps the old behavior, CapabilityStatement entries,
  metrics. A new built-in search parameter triggers a one-time index rebuild.
- **The order of resources the requested sort keys don't distinguish** (ties, and the
  order of an unsorted search). Ask for the order you need with `_sort`; in practice it is
  insertion order.
- **Unit conversion for quantities** may be added, as an option that is off by default.
- **While the search index is rebuilt** after an upgrade, searches use the previous index:
  they don't know parameters added by the new release yet, and a search only the new
  index can answer (e.g. quantities, coming from 0.16) answers 503 until the switch.
- **Wording.** Log messages, `OperationOutcome` diagnostics text, error messages,
  `--help` output. Match on status codes and issue codes, not on text.
- **The dashboard** (`GET /`), `$browse` and the `$demo` sample data.
- **The Rust crates** (`sazare-core`, `sazare-store`, `sazare-server`) as libraries. They
  are not published to crates.io; their APIs change freely. The supported interface is the
  binary.
- **The schema inside the SQLite files.** Read your data through the API or `$export`, not
  with SQL.
- **Performance** characteristics, memory use, and the minimum Rust version to build.
- **Behavior that contradicts the FHIR R4 specification**, when fixing it is the only way
  to conform. Such a fix ships in a minor release, listed under **Behavior changes**, and
  never silently.

## Security fixes

A security fix may restrict behavior in a **patch** or **minor** release — for example,
refusing a request that used to succeed — when keeping the old behavior would leave
deployments exposed. The CHANGELOG marks it under **Security** and says how to restore
legitimate uses (usually a configuration key).

## Deprecation

Before anything covered above is removed or changed in a major release, it is
**deprecated** in a minor release first: the CHANGELOG lists it under **Deprecated**, and
the server logs a warning when the deprecated feature is used. Deprecated features keep
working until the next major.

## Supported versions

Fixes, including security fixes, are released for the **latest minor** of the current
major. Upgrading within a major is designed to be safe, so stay on the latest minor.

# Upgrading sazare

## The procedure (any version)

1. **Back up** while the server runs:
   ```bash
   sazare-server backup backups/$(date +%Y%m%d-%H%M%S) --config config.yaml
   ```
2. **Replace the binary** — download the release, `brew upgrade sazare`, or pull the new
   Docker image — and restart.
3. **Watch the first start.** Schema migrations run automatically. When the search index
   has to be rebuilt (new search parameters, a new index format), the server **answers at
   once** and builds a new index in the background, switching to it when it is complete;
   until then searches use the previous index and may miss what the new one adds.
   `GET /health` shows the progress (`"index": {"state": "rebuilding", "done": …, "total": …}`)
   and the log says `Switched to search index generation N`. Expect roughly 3,000
   resources per second (about an hour per 10M); plan disk space for the old and the new index side by side.
   `index.rebuild: blocking` waits for the rebuild before serving; `manual` only rebuilds
   on `POST /$reindex`.
4. **Read the Behavior changes** for every version you skip in [CHANGELOG.md](CHANGELOG.md).

## What is migrated, and what is rebuilt

| File | On upgrade |
|---|---|
| `resources.sqlite` | Your data. Migrated in place when needed (never rebuilt). |
| `audit.sqlite` | Audit log. Migrated in place when needed. |
| `search_index.sqlite` | Derived. Migrated, and rebuilt from the resources when the search parameters or index format changed. |
| `search_index.fts.sqlite` | Derived (full text). Created and filled on first start. |

Migrations are **forward-only**: an upgraded database can't be opened by an older version.
To go back, restore the backup from step 1 (copy `resources.sqlite` and `audit.sqlite`
into `data_dir`, delete the two `search_index*` files, start the older binary).

## If something goes wrong

- **The server doesn't start**: the log names the failing step. A configuration error (for
  example `Invalid validation.default_profiles`) stops the server on purpose — fix the
  configuration rather than downgrading.
- **Searches miss data after the upgrade**: check `GET /health` — a background rebuild may
  still be running. Otherwise run `curl -X POST http://localhost:8080/$reindex` (operator
  credentials when auth is enabled): it rebuilds in the background (202; 409 while one
  runs). Deleting the `search_index*` files and restarting also rebuilds them.
- **Anything else**: restore the backup and start the previous version, then open an issue
  with the log.

## Version notes

### To 0.16.0

- **Index rebuilds no longer block startup.** The server answers immediately and builds the
  new index in the background (`GET /health` shows progress). Index files may now be named
  `search_index.gN.sqlite` (index generations); the newest complete one is used.
- `POST /$reindex` answers 202 at once and rebuilds in the background (it used to block
  until done and answer 200 with counts); 409 while a rebuild runs.
- **The search index is rebuilt once in a new format (index v2)**: about a tenth of the
  size (6 GB instead of ~60 GB at 10M resources) and faster searches. The old index keeps
  answering searches until the new one is complete; plan the time (about an hour per
  10M resources) and the disk space for the new index next to the old one. Unsorted
  searches now come back in index order rather than by id (the order of an unsorted search
  is unspecified; use `_sort` for an order).

### To 0.15.0

- **The first start rebuilds the search index once** (string values are now indexed as
  written, for a case-sensitive `:exact`); the log says so. Plan for it on large data
  sets — about a minute per 100k resources.
- **The command line has commands now.** `sazare-server` alone prints help instead of
  starting the server. Use `sazare-server start` (background) / `stop` / `status`, or
  `sazare-server run` in the foreground. `--backup <dir>` became `sazare-server backup <dir>`.
- Scripts, systemd units and launchd plists that started `sazare-server [--config …]`:
  add `run` (`sazare-server run --config …`). The Docker image needs no change.
- **Unknown configuration keys are an error.** A file that used to load with a typo now
  stops with the line and the valid keys; fix the key. The deprecated `plugins.dir` /
  `SAZARE_PLUGIN_DIR` are gone: use `apps.dir` / `SAZARE_APPS_DIR`.
- **Paths in `config.yaml` are relative to the file** (they were relative to the directory
  sazare was started from). If you started sazare from the directory holding
  `config.yaml`, nothing changes.
- **Without `storage.data_dir`, data goes to the user data directory** (e.g.
  `~/.local/share/sazare`). An existing `./data` that holds data is still used, with a
  warning: set `storage.data_dir` (or move it) to silence it.
- **IG directories** (`profiles/`, `searchparameters/`, `terminology/`) are read from the
  configuration file's directory (or `ig.dir`), not from the current directory; without a
  configuration file, a current directory holding them is still used, with a warning.
- Plain `key:` / `password:` credentials still work but are warned about; replace them with
  `key_sha256` / `password_hash` (`sazare-server auth new-api-key` / `auth hash-password`).

### To 0.14.0

- Nothing to do. Startup no longer counts the index or refreshes SQLite statistics, so
  large databases (millions of resources) start in under a second instead of minutes.

### To 0.13.0 (security release)

- Docker: the server now runs as user `sazare` (uid 10001). Existing volumes are
  handed over automatically on the first start. If you bind-mount a host directory and
  start with `--user`, make sure that user can write it.
- Plugins became static apps: move links from `/<name>/` to `/apps/<name>/`, and
  `/$plugins` to `/$apps`. Set `apps.dir` (or `SAZARE_APPS_DIR`) — a `./plugins`
  directory is no longer picked up on its own. Rename `plugins.dir` to `apps.dir` when
  convenient (the old key still works, with a warning).
- SMART Backend Services clients need `allowed_scopes` in the configuration; a client
  without it is now granted nothing.
- Bulk Data with JWTs needs a `system/` scope (`system/*.read` for export,
  `system/*.write` for import).
- Patient-scoped tokens can't change resources outside the patient compartment
  (Practitioner, Organization, ...); use a `user/` or `system/` scope for that.
- Tokens using SMART v2 granular scopes (`...?category=...`) get no access through them;
  grant plain `.rs` scopes instead.

### To 0.12.0

- The first start rebuilds the search index once (new built-in reference parameters; the
  log says `was built with different search parameters`). From now on this happens
  automatically whenever the parameter set changes — no manual `$reindex`.
- `_include` / `_revinclude` with an unknown or non-reference parameter, and chains deeper
  than 4 hops, are a 400 instead of returning nothing.
- With auth enabled, scrape `/metrics` with operator credentials.

### To 0.11.0 (security release)

- The binary now listens on `127.0.0.1` by default. If other machines connect to it, set
  `server.host: "0.0.0.0"` **and** `auth.enabled: true`. The Docker image is unchanged.
- With auth enabled, Subscriptions can't deliver to localhost or private networks unless
  `subscription.allow_private_endpoints: true`.
- App tokens can't run `$reindex` / `$demo`; patient-scoped tokens can't manage
  Subscriptions. Use operator credentials for administration.

### To 0.10.0

- The first start builds `search_index.fts.sqlite` (full text) for existing data.

### To 0.9.0

- Searches without `_count` return pages of 100. Clients that expected every result in one
  response must follow the Bundle's `next` link (or pass `_count`, up to 1000).

### To 0.8.0

- The search index gains a numeric column (migration v3); automatic.

### To 0.7.0

- New covering indexes are created on first start (migration v2); automatic.
- A `+` in a search value now means a space (dates excepted); send `%2B` for a literal `+`.

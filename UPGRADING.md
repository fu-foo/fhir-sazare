# Upgrading sazare

## The procedure (any version)

1. **Back up** while the server runs:
   ```bash
   sazare-server --config config.yaml --backup backups/$(date +%Y%m%d-%H%M%S)
   ```
2. **Replace the binary** — download the release, `brew upgrade sazare`, or pull the new
   Docker image — and restart.
3. **Watch the first start.** Schema migrations run automatically, and the search index /
   full text are rebuilt when needed. The log says what happened, e.g.
   `Search index was built with different search parameters; rebuilding from resource store...`
   then `Auto-reindex complete: N resources`. This takes roughly a minute per 100k
   resources; the server answers once it is done.
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
- **Searches miss data after the upgrade**: run `curl -X POST http://localhost:8080/$reindex`
  (with operator credentials when auth is enabled), or delete both `search_index*` files and
  restart — they are rebuilt from the resources.
- **Anything else**: restore the backup and start the previous version, then open an issue
  with the log.

## Version notes

### To 0.13.0 (security release)

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

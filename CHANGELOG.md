# Changelog

All notable changes to sazare. Versions follow [Semantic Versioning](https://semver.org/);
until 1.0, a minor version (0.x.0) may change behavior — those changes are listed under
**Behavior changes**. See [UPGRADING.md](UPGRADING.md) for how to upgrade.

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

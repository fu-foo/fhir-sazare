# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue: use **Report a
vulnerability** on the repository's
[Security tab](https://github.com/fu-foo/fhir-sazare/security). Include the version
(`sazare-server --version`), the configuration that matters (auth on or off, TLS), and
the requests that show the problem.

You'll get an answer within a week. A fix is released as a patch release; the advisory is
published with it, crediting you unless you'd rather not be named.

## Supported versions

| Version | Security fixes |
|---|---|
| Newest release | Yes |
| Older releases | No — upgrade (see [UPGRADING.md](UPGRADING.md)) |

From 1.0, the newest 1.x release is supported.

## What sazare protects, and what it leaves to you

In scope — please report:

- Reading or changing data without the credentials or scopes that should be needed
  (API keys, Basic auth, SMART on FHIR tokens), including a patient-scoped token reaching
  another patient's data.
- Requests that crash the server, or make it use memory or disk out of proportion to the
  request.
- Anything that lets a request run code, read files, or reach other hosts from the
  server.

Deployment, which is up to whoever runs it:

- **Authentication is off by default** so the quick start works. Turn it on
  (`auth.enabled: true`) before anyone but you can reach the server.
- **TLS**: use the built-in TLS (`server.tls`) or a reverse proxy in front; plain HTTP
  exposes credentials and data.
- **The data directory** holds the data unencrypted; protect it (and its backups) as you
  would the data itself.
- **No rate limiting** is built in (repeated failed logins are locked out). Exposed to
  the internet, put a reverse proxy in front that limits request rates and sizes.
- **Bulk operations are operators' tools**: `$import` and `$export` need system-level
  scopes (or an API key / Basic user). An export keeps a copy of the data it covers on
  disk for an hour (at most 8 jobs at once): plan the disk for it.
- **TLS certificates** are read at startup: restart after renewing them.
- **Files served under `/apps` and `/ui`** come only from their directories: no `..`, no
  symbolic links (a link is refused even when it points inside).
- sazare is a single-node server for development, research, prototypes and small
  deployments. It has not been certified for any regulatory regime.

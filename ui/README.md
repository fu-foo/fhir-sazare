# sazare-ui — example client (検査依頼デモ)

This is an **optional example client app**, not the sazare server's UI.

- **The server's own console is the built-in dashboard at `/`** — server-rendered,
  embedded in the single binary, no build step, on by default. That is the
  canonical UI for browsing, searching, and validating.
- **This `ui/` app** is a separate React/Vite single-page application that *talks
  to* a running sazare server over its FHIR REST API. It demonstrates a real
  workflow — registering lab orders (`ServiceRequest`) as distinct profiles and
  searching them via `_profile` — as an example of what you can build on top of
  FHIR.

It is **opt-in and never bundled into the server binary**, so the single-file,
no-Docker promise of sazare is unaffected. Run it separately during development:

```bash
cd ui
npm install
npm run dev          # dev server (talks to a sazare server, default :8080)
# or:
npm run build        # static build into ui/dist
SAZARE_UI_DIR=ui/dist ./target/release/sazare-server   # serve it under /ui (optional)
```

Think of it as a companion sample, like the scripts in `examples/` — a way to
see FHIR in action, kept clearly separate from the server itself.

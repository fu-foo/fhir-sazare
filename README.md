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
- **Search** — Parameter-based search, chain search (`subject:Patient.name=...`), `_include`, `_revinclude`
- **Conditional operations** — Conditional create (`If-None-Exist`), update, and delete
- **Resource filtering** — `_summary` (5 modes) and `_elements` support
- **Validation** — Multi-phase validation with US-Core profile support
- **Bulk data** — NDJSON `$export` and `$import`
- **Plugin system** — Serve domain-specific SPAs at top-level paths (e.g. `/sample-patient-register/`)
- **Web dashboard** — Browser-based server monitoring at `/`
- **Audit logging** — All operations recorded to dedicated SQLite database
- **PATCH** — JSON Patch (RFC 6902)
- **$everything** — Patient compartment operation
- **Subscription** — REST-hook notifications on resource changes
- **Authentication** — API key, Basic auth, JWT (HS256/RS256/JWK URL), SMART on FHIR scopes
- **Compartment access control** — Patient-scoped token restricts access to own data
- **TLS/HTTPS** — Optional TLS via config
- **Webhooks** — Event-driven notifications to external endpoints
- **Graceful shutdown** — Clean shutdown on SIGTERM / Ctrl+C

---

## Quick Start

### Prerequisites

- **Rust 1.85+** (2024 edition)

### Build and Run

```bash
git clone https://github.com/fu-foo/fhir-sazare.git
cd fhir-sazare
cargo build --release
./target/release/sazare-server
```

The server starts at `http://localhost:8080` with default settings (no authentication).

### Verify

```bash
# CapabilityStatement
curl http://localhost:8080/metadata

# Open dashboard in browser
open http://localhost:8080/
```

### Create Your First Resource

```bash
curl -X POST http://localhost:8080/Patient \
  -H "Content-Type: application/json" \
  -d '{
    "resourceType": "Patient",
    "name": [{"family": "Doe", "given": ["Jane"]}],
    "gender": "female",
    "birthDate": "1990-01-01"
  }'
```

---

## Configuration

Copy the example config and customize:

```bash
cp config.example.yaml config.yaml
```

```yaml
server:
  host: "0.0.0.0"
  port: 8080

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
```

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
| `GET` | `/$export` | Bulk export (NDJSON) |
| `POST` | `/$import` | Bulk import (NDJSON) |

### Dashboard

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Web dashboard (browser) |
| `GET` | `/$status` | Server status JSON API |

### Plugins

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/$plugins` | List installed plugins (JSON) |
| `GET` | `/{name}/` | Serve plugin SPA |
| `GET` | `/{name}/{path}` | Serve plugin static files (SPA fallback) |

---

## Search

### Basic Search

```bash
# Search by parameter
curl "http://localhost:8080/Patient?name=Doe"

# With pagination
curl "http://localhost:8080/Patient?_count=10&_offset=0"

# Summary and elements
curl "http://localhost:8080/Patient?_summary=true"
curl "http://localhost:8080/Patient?_elements=name,gender"
```

### Chain Search

Search by referenced resource attributes (1-level, type required):

```bash
# Find Observations where the subject (Patient) has name "Doe"
curl "http://localhost:8080/Observation?subject:Patient.name=Doe"
```

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

```bash
# Export all resources
curl http://localhost:8080/\$export

# Export specific types
curl "http://localhost:8080/\$export?_type=Patient,Observation"
```

### Import

```bash
# Import from NDJSON
curl -X POST http://localhost:8080/\$import \
  -H "Content-Type: application/x-ndjson" \
  -d '{"resourceType":"Patient","name":[{"family":"Doe"}]}
{"resourceType":"Patient","name":[{"family":"Smith"}]}'
```

---

## Plugins

sazare can serve domain-specific SPAs (Single Page Applications) as plugins. Each plugin is a directory under the plugin directory containing static files (HTML, JS, CSS). Plugins are served at top-level URLs (e.g. `http://localhost:8080/my-app/`) — the internal `plugins/` directory path is not exposed. Plugins access FHIR data through sazare's REST API on the same origin, so no CORS configuration is needed.

### Configuration

```yaml
plugins:
  dir: "plugins"    # Directory containing plugin subdirectories
```

Or via environment variable: `SAZARE_PLUGIN_DIR=./plugins`

If no `plugins` section is configured, sazare looks for a `./plugins` directory by default.

### Directory Structure

```
plugins/
  my-app/           → served at /my-app/
    index.html      # Entry point
    style.css
    app.js
  another-app/      → served at /another-app/
    index.html
    ...
```

### Behavior

- **Top-level routing** — Each plugin directory name becomes a top-level URL path (e.g. `plugins/my-app/` → `/my-app/`)
- **SPA fallback** — Requests for non-existent paths return `index.html` (client-side routing)
- **Cache-Control** — `index.html` is served with `no-cache`; other assets with `max-age=604800` (1 week)
- **No authentication** — Plugin static files are served without auth; data access goes through FHIR API which has its own auth
- **Security** — Path traversal is blocked; symlinks are rejected
- **Plugin listing** — `GET /$plugins` returns a JSON list of installed plugins

### Sample Plugin

A sample plugin (`plugins/sample-patient-register/`) is included that demonstrates a Patient registration form with a list view. Access it at `http://localhost:8080/sample-patient-register/`.

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
- **`audit.sqlite`** — Audit log entries

---

## Development

```bash
# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run

# Run with custom config
cargo run -- --config path/to/config.yaml
```

---

## Roadmap

- [ ] JP-Core profile validation
- [ ] Multi-level chain search
- [ ] Subscription via WebSocket

---

## Supporting

If you find this project useful, consider supporting its development:

[![GitHub Sponsors](https://img.shields.io/github/sponsors/fu-foo?style=for-the-badge&logo=github&label=Sponsor)](https://github.com/sponsors/fu-foo)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-Support-ff5e5b?style=for-the-badge&logo=ko-fi)](https://ko-fi.com/fufoo)

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

---

## Japanese / 日本語

<details>
<summary>日本語ドキュメント</summary>

### 概要

**sazare** は Rust で書かれた軽量 FHIR R4 サーバーです。SQLite を内蔵しているため、外部データベースのセットアップは不要です。シングルバイナリでデプロイできます。

### 主な機能

- FHIR R4 (4.0.1) 準拠の REST API
- 全リソースタイプに対する CRUD 操作
- バージョン履歴（vread / _history）
- Bundle 処理（transaction: all-or-nothing / batch: 各エントリ独立）
- `urn:uuid:` 参照の自動解決（transaction 内）
- 検索パラメータ、チェーンサーチ（`subject:Patient.name=テスト姓`）、`_include` / `_revinclude`
- 条件付き操作（作成 / 更新 / 削除）
- JSON Patch (RFC 6902)
- Patient `$everything` オペレーション
- Subscription（rest-hook 通知）
- `_summary` / `_elements` によるリソースフィルタリング
- US-Core プロファイルによるバリデーション
- NDJSON 形式での一括エクスポート / インポート
- プラグインシステム（SPA をトップレベル URL で配信、例: `/sample-patient-register/`）
- ブラウザで確認できる Web ダッシュボード
- 監査ログ（全操作を SQLite に記録）
- API キー / Basic 認証 / JWT (HS256/RS256/JWK URL) / SMART on FHIR スコープ
- コンパートメントベースのアクセス制御
- TLS/HTTPS 対応
- Webhook 通知

### クイックスタート

```bash
git clone https://github.com/fu-foo/fhir-sazare.git
cd fhir-sazare
cargo build --release
./target/release/sazare-server
```

サーバーが `http://localhost:8080` で起動します。ブラウザでアクセスするとダッシュボードが表示されます。

### 設定

`config.example.yaml` を `config.yaml` にコピーして編集してください。設定ファイルがない場合はデフォルト設定（ポート 8080、認証なし）で起動します。

### 使用例

```bash
# Patient リソースの作成
curl -X POST http://localhost:8080/Patient \
  -H "Content-Type: application/json" \
  -d '{"resourceType":"Patient","name":[{"family":"テスト姓","given":["テスト名"]}]}'

# 検索
curl "http://localhost:8080/Patient?name=テスト姓"

# チェーンサーチ（Patient の名前で Observation を検索）
curl "http://localhost:8080/Observation?subject:Patient.name=テスト姓"
```

### 名前の由来

**細石（さざれ）** — 古今和歌集に詠まれる小さな石。やがて集まり巌（いわお）となるように、小さく始めて堅実に成長するサーバーを目指しています。

</details>

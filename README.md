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
- **Search** — Parameter-based search, chain search (`subject:Patient.name=...`), reverse chain (`_has:Observation:subject:code=...`), `_include`, `_revinclude`
- **Conditional operations** — Conditional create (`If-None-Exist`), update, and delete
- **Resource filtering** — `_summary` (5 modes) and `_elements` support
- **Validation** — Multi-phase validation against US Core profiles; load any other IG (e.g. JP Core) by dropping its profiles in a `profiles/` directory
- **US Core conformance** — Passes the Inferno US Core v7 & v8 FHIR API test suites (`examples/us-core-seed.json` for v7, `examples/us-core-v8-seed.json` for v8; the TLS test requires an HTTPS deployment)
- **Japanese name search** — Search names by kana (`name-kana`) / kanji (`name-kanji`), kept as language support (JP Core profiles themselves are loadable, not bundled — see below)
- **Bulk data** — NDJSON `$import`, and `$export` both synchronous and async (FHIR Bulk Data Access IG: `Prefer: respond-async` kick-off, status poll, manifest, `_type`/`_since`/`_outputFormat`)
- **Plugin system** — Serve domain-specific SPAs at top-level paths (e.g. `/sample-patient-register/`)
- **Web dashboard** — Built-in console at `/`: browse resources, a search builder that shows the generated FHIR URL, one-click sample data, English/Japanese — no build step, served from the binary
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

## Quick Start — run a FHIR server in 30 seconds

Download one file, run it, and you're looking at live FHIR data. No Docker, no
JVM, no database, no config.

**On macOS or Linux, Homebrew is the smoothest path** — it also sidesteps the
macOS Gatekeeper warning below, because brew-installed binaries aren't
quarantined:

```bash
brew install fu-foo/tap/sazare
sazare-server --demo --open
```

**On Windows, [Scoop](https://scoop.sh) is the smoothest path** — it likewise
sidesteps the SmartScreen warning below, because Scoop downloads aren't tagged
with the Mark-of-the-Web:

```powershell
scoop bucket add fu-foo https://github.com/fu-foo/scoop-bucket
scoop install sazare
sazare-server --demo --open
```

**Otherwise, download the binary for your OS** from the
[latest release](https://github.com/fu-foo/fhir-sazare/releases/latest)
(macOS Intel/Apple Silicon, Linux x86-64/ARM64, Windows x86-64), then unpack and
run it with sample data:

```bash
# macOS / Linux (Apple Silicon shown — pick the asset matching your OS/arch)
tar xzf sazare-server-macos-arm64.tar.gz
./sazare-server --demo --open
```

```powershell
# Windows: unzip, then
.\sazare-server.exe --demo --open
```

`--demo` pre-loads a few sample patients with vitals, a condition, an encounter,
and a prescription; `--open` launches the built-in dashboard in your browser.
That's the whole setup.

> **macOS first run** (direct download only): the binary is unsigned, so
> Gatekeeper blocks it with an "unidentified developer" dialog. Clear the
> quarantine flag and run:
> ```bash
> xattr -d com.apple.quarantine ./sazare-server && ./sazare-server --demo --open
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
>   .\sazare-server.exe --demo --open
>   ```
>   (GUI equivalent: right-click the `.exe` → **Properties** → tick **Unblock**
>   at the bottom → **OK**.)
>
> Installing with `scoop` avoids this entirely. SmartScreen also stops warning
> on its own once a build earns download reputation; signing the Windows and
> macOS binaries is on the road to 1.0.

> **Linux**: no such gate — download, `chmod +x sazare-server` if needed, and run.

The server listens on `http://localhost:8080` (no authentication by default). The
dashboard has a one-click "Load sample data" button and a search builder, and
speaks English or Japanese.

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

### Build from source (optional)

Prefer to build it yourself? You'll need **Rust 1.85+** (2024 edition):

```bash
git clone https://github.com/fu-foo/fhir-sazare.git
cd fhir-sazare
cargo build --release
./target/release/sazare-server --demo --open
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

# Japanese name search by kana (reading) or kanji
curl "http://localhost:8080/Patient?name-kana=ヤマダ"
curl "http://localhost:8080/Patient?name-kanji=山田"
```

### Chain Search

Search by referenced resource attributes (multi-level, type required at each hop):

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
- 検索パラメータ、チェーンサーチ（`subject:Patient.name=テスト姓`）、逆方向チェーン（`_has:Observation:subject:code=...`）、`_include` / `_revinclude`
- 条件付き操作（作成 / 更新 / 削除）
- JSON Patch (RFC 6902)
- Patient `$everything` オペレーション
- Subscription（rest-hook 通知 / WebSocket `/ws` の R4 `bind`・`ping` 通知）
- Webhook（`BundleCreated`・`TaskCompleted` のライフサイクルイベントを設定エンドポイントへ通知）
- `_summary` / `_elements` によるリソースフィルタリング
- US Core プロファイルによるバリデーション（JP Core 等の他 IG は `profiles/` ディレクトリから読み込み）
- US Core 適合 — Inferno US Core v7 & v8 の FHIR API テストスイートをパス（v7: `examples/us-core-seed.json` / v8: `examples/us-core-v8-seed.json`。TLS テストは HTTPS デプロイが前提）
- 日本語サポート — 氏名のかな検索（`name-kana`）・漢字検索（`name-kanji`）。JP Core プロファイル自体は同梱せず、必要なら `profiles/` から読み込み
- NDJSON 形式での一括エクスポート / インポート
- プラグインシステム（SPA をトップレベル URL で配信、例: `/sample-patient-register/`）
- ブラウザで確認できる Web ダッシュボード
- 監査ログ（全操作を SQLite に記録）
- API キー / Basic 認証 / JWT (HS256/RS256/JWK URL) / SMART on FHIR スコープ
- コンパートメントベースのアクセス制御
- TLS/HTTPS 対応

### クイックスタート — 30秒で FHIR サーバーを動かす

ファイルを1つ落として実行するだけ。Docker も JVM もデータベースも設定も不要です。

**macOS / Linux は Homebrew が一番ラク** — 後述の macOS Gatekeeper 警告も回避できます（brew で入れたバイナリは隔離フラグが付かないため）:

```bash
brew install fu-foo/tap/sazare
sazare-server --demo --open
```

**Windows は [Scoop](https://scoop.sh) が一番ラク** — 同様に後述の SmartScreen 警告を回避できます（Scoop のダウンロードには Mark-of-the-Web が付かないため）:

```powershell
scoop bucket add fu-foo https://github.com/fu-foo/scoop-bucket
scoop install sazare
sazare-server --demo --open
```

**それ以外は、お使いの OS のバイナリ**を
[最新リリース](https://github.com/fu-foo/fhir-sazare/releases/latest)
（macOS Intel/Apple Silicon、Linux x86-64/ARM64、Windows x86-64）からダウンロードして、展開して実行します:

```bash
# macOS / Linux（Apple Silicon の例。OS/アーキテクチャに合うアセットを選んでください）
tar xzf sazare-server-macos-arm64.tar.gz
./sazare-server --demo --open
```

```powershell
# Windows: 展開してから
.\sazare-server.exe --demo --open
```

`--demo` はサンプル患者（バイタル・条件・受診・処方）を事前投入し、`--open` は内蔵ダッシュボードをブラウザで開きます。これで準備完了。サーバーは `http://localhost:8080` で待ち受けます（デフォルトは認証なし）。

> **macOS の初回起動**（直接ダウンロードした場合のみ）: バイナリは未署名なので、Gatekeeper が「開発元を確認できません」と表示してブロックします。隔離フラグを外して実行してください:
> ```bash
> xattr -d com.apple.quarantine ./sazare-server && ./sazare-server --demo --open
> ```
> または **システム設定 → プライバシーとセキュリティ → 「このまま開く」** で許可します（macOS 15 Sequoia では、従来の「右クリック → 開く」は使えなくなりました）。`brew` で入れればこの手順は不要です。

> **Windows の初回起動**（直接ダウンロードした場合のみ）: `.exe` が未署名のため、Microsoft Defender SmartScreen が青い「**Windows によって PC が保護されました**」を表示し、目立つボタンは *実行しない* だけになります。これは上記 macOS のダイアログの Windows 版で、「マルウェア」ではなく「ネットからダウンロードされた、まだ認知されていないアプリ」という意味です。抜け方は2つ:
>
> - **クリックで抜ける**: ダイアログの **詳細情報** をクリック →現れる **実行** ボタンを押す
> - **一度で恒久的に解除（PowerShell）**: ファイルのブロックを解除すれば次回から出ません（macOS の `xattr` に相当）:
>   ```powershell
>   Unblock-File .\sazare-server.exe
>   .\sazare-server.exe --demo --open
>   ```
>   （GUI なら `.exe` を右クリック → **プロパティ** → 下部の **ブロックの解除** にチェック → **OK**）
>
> `scoop` で入れればこの手順は不要です。SmartScreen はダウンロード実績が貯まると自動的に警告しなくなります。Windows / macOS バイナリの署名は 1.0 に向けた課題です。

> **Linux**: この種のゲートはありません。ダウンロードして、必要なら `chmod +x sazare-server` し、実行するだけです。

#### ソースからビルドする場合

Rust ツールチェーンがあれば、自分でビルドすることもできます:

```bash
git clone https://github.com/fu-foo/fhir-sazare.git
cd fhir-sazare
cargo build --release
./target/release/sazare-server
```

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

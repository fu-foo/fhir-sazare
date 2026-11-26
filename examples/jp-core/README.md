# JP Core を sazare で使う

[HL7 FHIR JP Core](https://jpfhir.jp/fhir/core/) の実装ガイドを sazare に読み込む手順です。
JP Core は sazare に同梱していません（CC0-1.0 で公開されているパッケージを、起動時に読み込みます）。
読み込むと、次のものが使えるようになります。

- **バリデーション** — `meta.profile` に JP Core のプロファイル（例: `JP_Patient`）を宣言したリソースを、そのプロファイルで検証します。
  作成・更新時に違反があれば 400 を返し、`$validate` では OperationOutcome で理由を返します。
- **JP Core の検索パラメータ** — カナ氏名、被保険者番号、都道府県番号など、JP Core 1.2.0 が定義する8つすべてが使えます。
- **コード値の検証（任意）** — `--terminology` を付けると JP のコード体系（処置コード、病名交換用コード、ICD-10 など）も読み込み、
  存在しないコードを拒否します。

> English summary: run `setup.sh` (add `--terminology` to also validate JP
> code systems) to download JP Core into `profiles/`, `searchparameters/` and
> `terminology/`, start sazare from that directory, and run
> `load_examples.sh` to load the package's 76 examples and check the JP Core
> searches and validation. Existing data is reindexed automatically on the
> next start.

## 1. パッケージを取得する

```bash
bash examples/jp-core/setup.sh /srv/sazare                 # 省略時はカレントディレクトリ
bash examples/jp-core/setup.sh --terminology /srv/sazare   # コード値も検証する場合
```

`https://jpfhir.jp/fhir/core/1.2.0/package.tgz` を取得し、次のように展開します。
バージョンは `JP_CORE_VERSION=1.2.0` のように環境変数で変えられます。

```
/srv/sazare/
├── profiles/           StructureDefinition-*.json（111件）
├── searchparameters/   SearchParameter-*.json（8件）
├── terminology/        CodeSystem-*.json / ValueSet-*.json（--terminology のときだけ）
└── .jp-core/           ダウンロードしたパッケージ（サンプルは load_examples.sh が使います）
```

`--terminology` は JP Core のパッケージに含まれる CodeSystem / ValueSet に加えて、
[jpfhir-terminology](https://jpfhir.jp/fhir/core/terminology/ig/) 2.2611.0（展開後 約85MB）も取得します。
JP Core 1.2.0 が依存を宣言している 1.4.0 はもう配布されていないため、現行版を使います
（`JP_TERMINOLOGY_VERSION` で変更できます）。JP Core がバインドしている ValueSet はすべてこの版で解決できます。

## 2. sazare を起動する

sazare は**作業ディレクトリ**の `profiles/` と `searchparameters/` を起動時に読みます。

```bash
cd /srv/sazare
sazare-server start
```

起動ログに次の行が出れば読み込めています（3行目は `--terminology` のとき）。

```
INFO sazare_server: Loading 111 custom profiles
INFO sazare_core::profile_loader: Loaded 8 SearchParameter resource(s) from "searchparameters"
INFO sazare_server: Terminology: 117 code systems, 107 value sets loaded (13 value sets not fully resolvable, left unchecked)
```

ターミノロジーを読み込むと、メモリ使用量が約60MB増え、起動が約0.3秒遅くなります。

サポート外の FHIRPath 式を持つ検索パラメータは、`Skipping custom search parameter: ...`
という警告を出して読み込みを見送ります（誤った結果を返さないため）。JP Core 1.2.0 の8件はすべて読み込めます。

### Docker の場合

コンテナの作業ディレクトリは `/etc/sazare` です。ディレクトリをそこへマウントします。

```yaml
services:
  sazare:
    image: ghcr.io/fu-foo/fhir-sazare:latest
    ports:
      - "8080:8080"
    volumes:
      - sazare-data:/data
      - ./profiles:/etc/sazare/profiles:ro
      - ./searchparameters:/etc/sazare/searchparameters:ro
      - ./terminology:/etc/sazare/terminology:ro   # --terminology のとき
volumes:
  sazare-data:
```

### すでにデータがある場合

検索パラメータを後から追加・変更しても、次の起動時にサーバーが定義の変化に気づき、
既存データのインデックスを自動で作り直します（ログに `was built with different search parameters; rebuilding` と出ます）。
起動し直さずに反映したいときは `curl -X POST http://localhost:8080/\$reindex` でも作り直せます。

## 3. サンプルデータで試す

```bash
bash examples/jp-core/load_examples.sh /srv/sazare
```

パッケージ付属のサンプル76件を `PUT` で投入し（id とリソース間の参照はそのまま）、
JP Core の検索と検証を実行して、期待どおりの結果かを確かめます。1つでも違えば終了コード 1 で終わります（CI でも実行しています）。

```
=== JP Core searches (hits, query) ===
1    Patient?jp-kana-sort=ヤマダ  ok
1    Coverage?jp-insured-personnumber=１８７  ok
2    Organization?jp-prefectureno=13  ok
2    MedicationRequest?jp-medication-start=ge2020-01-01  ok
1    Patient?_has:MedicationRequest:subject:jp-medication-start=ge2020-01-01  ok
...
=== JP Core validation ===
reject  JP_Patient without identifier  ok
accept  JP_Procedure with a JP procedure code  ok
reject  JP_Procedure with an unknown JP procedure code  ok
```

## 検索パラメータ一覧（JP Core 1.2.0）

| パラメータ | 対象 | 型 | 例 |
|---|---|---|---|
| `jp-kana-sort` | Patient | string | `Patient?jp-kana-sort=ヤマダ`（`use=usual` のカナ氏名、前方一致） |
| `jp-insured-personsymbol` | Coverage | string | `Coverage?jp-insured-personsymbol=あいう`（被保険者記号） |
| `jp-insured-personnumber` | Coverage | string | `Coverage?jp-insured-personnumber=１８７`（被保険者番号） |
| `jp-insured-personsubnumber` | Coverage | string | `Coverage?jp-insured-personsubnumber=05`（枝番） |
| `jp-prefectureno` | Organization | token | `Organization?jp-prefectureno=13`（都道府県番号） |
| `jp-insurance-organizationcategory` | Organization | token | `Organization?jp-insurance-organizationcategory=1`（点数表番号） |
| `jp-insurance-organizationno` | Organization | token | `Organization?jp-insurance-organizationno=<system>\|1234567`（医療機関コード） |
| `jp-medication-start` | MedicationRequest | date | `MedicationRequest?jp-medication-start=ge2020-01-01`（服用開始日） |

組み込みの検索パラメータと同じように、`ge`/`lt` などの日付の比較、`system|code` 形式のトークン、
`:exact`/`:contains`、チェーン検索、`_has` と組み合わせられます。

組み込みの `name` でも、漢字・カナのどちらの氏名も検索できます（`Patient?name=山田`、`Patient?name=ヤマダ`）。
`jp-kana-sort` は「`use=usual` のカナ表記」だけを対象にする点が違います。

値の空白は `%20` でも `+` でも送れます（`curl --data-urlencode` は `+` にします）。`+` そのものを検索したいときは `%2B` と書いてください。
日付の値だけは例外で、`+` をそのまま残すので、`ge2024-01-01T10:00:00+09:00` のようにタイムゾーンをエンコードせずに書けます。

## バリデーションの例

`meta.profile` に JP Core のプロファイルを宣言すると、そのプロファイルの必須要素などを検証します。

```bash
curl -s -X POST 'http://localhost:8080/Patient/$validate' \
  -H 'Content-Type: application/fhir+json' \
  -d '{"resourceType":"Patient",
       "meta":{"profile":["http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"]},
       "gender":"male"}'
```

```json
{"resourceType":"OperationOutcome","issue":[{"severity":"error","code":"required",
 "diagnostics":"Profile 'http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient' requires element 'Patient.identifier' (min=1) but a parent instance has 0 occurrence(s)",
 "expression":["Patient.identifier"]}]}
```

### コード値の検証（`--terminology`）

プロファイルが required でバインドしているコードを、読み込んだコード体系と照合します。たとえば
`JP_Procedure` の `code.coding` で JP 処置コードのシステムを名乗るコードは、そのコード体系に実在する必要があります。

```
Profile 'http://jpfhir.jp/fhir/core/StructureDefinition/JP_Procedure': element 'Procedure.code.coding:medical'
code 'NOPE' is not in code system http://jpfhir.jp/fhir/core/CodeSystem/JP_ProcedureCodesMedical_CS
(required value set http://jpfhir.jp/fhir/core/ValueSet/JP_ProcedureCodesMedical_VS)
```

誤って正しいデータを拒否しないよう、確実に判断できる場合だけ検証します。

- 検証するのは、`content: complete` のコード体系と、それだけで中身が決まる ValueSet です。
  フィルタや他の ValueSet の参照を含むなど、展開しきれない ValueSet（13件）は検証しません。
- スライス（`Observation.code.coding:jlac10` など）は、スライスがコード体系を固定していて、
  ValueSet がそのコード体系全体を含む場合だけ検証します。院内コードなど他のシステムのコーディングはそのまま通します。

## プロファイルを宣言していないデータに JP Core を適用する

現場のデータは `meta.profile` を宣言していないことが多く、そのままでは JP Core の検証がかかりません。
`config.yaml` でリソースの種類ごとに既定のプロファイルを指定すると、宣言のないリソースもそのプロファイルで検証します。

```yaml
validation:
  default_profiles:
    Patient: "http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"
    Procedure: "http://jpfhir.jp/fhir/core/StructureDefinition/JP_Procedure"
  default_profile_mode: enforce   # または warn
```

- `enforce`（既定）: プロファイルを宣言した場合と同じように、違反していれば 400 で拒否します。
  理由の最後に「既定のプロファイルで検証した」旨の information が付きます。
- `warn`: 保存は受け付け、違反は `$validate` の warning とサーバーログに出します。既存データを移行するときに、
  まず違反の数を把握したい場合に使います。
- 検証はコピーに対して行い、保存するリソースに `meta.profile` を書き足すことはしません。
- 別のプロファイル（US Core など）を宣言しているリソースは、宣言したほうだけで検証します。
- 指定したプロファイルが読み込まれていない（`profiles/` にない、URL の誤記など）ときは、起動時にエラーで止まります。
  検証が黙って無効になるのを防ぐためです。

## 制限事項

- 検証の対象は、`meta.profile` でプロファイルを宣言したリソースと、`validation.default_profiles` で既定のプロファイルを指定した種類のリソースです。
- コード値の検証は `--terminology` を付けたときだけです。付けても、拡張（Extension）の中のコード
  （例: 都道府県番号）と、展開しきれない ValueSet へのバインディングは検証しません。
- `content: complete` と宣言されたコード体系は、そこにないコードを誤りとして扱います。実運用のコードがまだ
  jpfhir-terminology に収録されていない場合は拒否されるので、その場合は `terminology/` から該当ファイルを外してください。
- sazare が公式に適合を確認しているのは US Core（Inferno のテストスイート）です。JP Core については、
  上記のとおりパッケージ付属のサンプル76件がすべて投入・検証でき、8つの検索パラメータが期待どおりの結果を返すことを確認しています。

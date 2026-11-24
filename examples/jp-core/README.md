# JP Core を sazare で使う

[HL7 FHIR JP Core](https://jpfhir.jp/fhir/core/) の実装ガイドを sazare に読み込む手順です。
JP Core は sazare に同梱していません（CC0-1.0 で公開されているパッケージを、起動時に読み込みます）。
読み込むと、次の2つが使えるようになります。

- **バリデーション** — `meta.profile` に JP Core のプロファイル（例: `JP_Patient`）を宣言したリソースを、そのプロファイルで検証します。
  作成・更新時に違反があれば 400 を返し、`$validate` では OperationOutcome で理由を返します。
- **JP Core の検索パラメータ** — カナ氏名、被保険者番号、都道府県番号など、JP Core 1.2.0 が定義する8つすべてが使えます。

> English summary: run `setup.sh` to download the JP Core package into
> `profiles/` and `searchparameters/`, start sazare from that directory, and
> optionally `load_examples.sh` to load the package's 76 examples and try the
> JP Core searches. Existing data needs one `POST /$reindex`.

## 1. パッケージを取得する

```bash
bash examples/jp-core/setup.sh /srv/sazare      # 省略時はカレントディレクトリ
```

`https://jpfhir.jp/fhir/core/1.2.0/package.tgz` を取得し、次のように展開します。
バージョンは `JP_CORE_VERSION=1.2.0` のように環境変数で変えられます。

```
/srv/sazare/
├── profiles/           StructureDefinition-*.json（111件）
├── searchparameters/   SearchParameter-*.json（8件）
└── .jp-core/package/   パッケージ本体（サンプルは load_examples.sh が使います）
```

## 2. sazare を起動する

sazare は**作業ディレクトリ**の `profiles/` と `searchparameters/` を起動時に読みます。

```bash
cd /srv/sazare
sazare-server
```

起動ログに次の行が出れば読み込めています。

```
INFO sazare_server: Loading 111 custom profiles
INFO sazare_core::profile_loader: Loaded 8 SearchParameter resource(s) from "searchparameters"
```

サポート外の FHIRPath 式を持つ検索パラメータは、`Skipping custom search parameter: ...`
という警告を出して読み込みを見送ります（誤った結果を返さないため）。JP Core 1.2.0 の8件はすべて読み込めます。

### Docker の場合

コンテナの作業ディレクトリは `/etc/sazare` です。2つのディレクトリをそこへマウントします。

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
volumes:
  sazare-data:
```

### すでにデータがある場合

検索インデックスは保存時に作られます。検索パラメータを後から追加したときは、
既存データにも効くように一度だけインデックスを作り直してください。

```bash
curl -X POST http://localhost:8080/\$reindex
```

## 3. サンプルデータで試す

```bash
bash examples/jp-core/load_examples.sh /srv/sazare
```

パッケージ付属のサンプル76件を `PUT` で投入し（id とリソース間の参照はそのまま）、
JP Core の検索を実行して件数を表示します。

```
=== JP Core searches (hits, query) ===
1    Patient?jp-kana-sort=ヤマダ
1    Coverage?jp-insured-personnumber=１８７
1    Coverage?jp-insured-personsymbol=あいう
2    Organization?jp-prefectureno=13
1    Organization?jp-insurance-organizationno=http://jpfhir.jp/fhir/core/mhlw/IdSystem/MedicalOrganizationID|9876543
2    MedicationRequest?jp-medication-start=ge2020-01-01
1    Patient?_has:MedicationRequest:subject:jp-medication-start=ge2020-01-01
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

## 制限事項

- 検証の対象は、`meta.profile` でプロファイルを宣言したリソースだけです。宣言のないリソースに JP Core を当てはめることはしません。
- JP Core の ValueSet / CodeSystem（パッケージ内の約40件と、依存する `jpfhir-terminology`）は読み込みません。
  そのため、JP 固有のコード体系へのバインディング（例: 都道府県番号のコード値）は検証しません。
- sazare が公式に適合を確認しているのは US Core（Inferno のテストスイート）です。JP Core については、
  上記のとおりパッケージ付属のサンプル76件がすべて投入・検証でき、8つの検索パラメータが期待どおりの結果を返すことを確認しています。

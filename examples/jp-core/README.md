# Using JP Core with sazare

How to load the [HL7 FHIR JP Core](https://jpfhir.jp/fhir/core/) implementation guide
into sazare. JP Core is not bundled; its package (published under CC0-1.0) is read at
startup. With it loaded you get:

- **Validation** — a resource declaring a JP Core profile in `meta.profile` (e.g.
  `JP_Patient`) is validated against it. A create or update that violates it is a 400;
  `$validate` returns the reasons in an OperationOutcome.
- **JP Core search parameters** — all 8 that JP Core 1.2.0 defines: kana name, insured
  person number, prefecture number, and more.
- **Code validation (optional)** — with `--terminology`, the JP code systems (procedure
  codes, disease exchange codes, ICD-10, …) are loaded too, and unknown codes are
  rejected.

## 1. Get the package

```bash
bash examples/jp-core/setup.sh /srv/sazare                 # default: the current directory
bash examples/jp-core/setup.sh --terminology /srv/sazare   # also validate codes
```

This downloads `https://jpfhir.jp/fhir/core/1.2.0/package.tgz` and unpacks it like this
(choose another version with `JP_CORE_VERSION=1.2.0`):

```
/srv/sazare/
├── profiles/           StructureDefinition-*.json (111)
├── searchparameters/   SearchParameter-*.json (8)
├── terminology/        CodeSystem-*.json / ValueSet-*.json (only with --terminology)
└── .jp-core/           the downloaded package (its examples are used by load_examples.sh)
```

Besides the CodeSystems / ValueSets in the JP Core package, `--terminology` also fetches
[jpfhir-terminology](https://jpfhir.jp/fhir/core/terminology/ig/) 2.2611.0 (about 85 MB
unpacked). JP Core 1.2.0 declares a dependency on 1.4.0, which is no longer distributed,
so the current release is used (change it with `JP_TERMINOLOGY_VERSION`). Every ValueSet
JP Core binds to resolves with it.

## 2. Start sazare

sazare reads `profiles/` and `searchparameters/` in its **working directory** at startup.

```bash
cd /srv/sazare
sazare-server start
```

These lines in the log (`data/logs/sazare.log`) show the package was loaded (the third
only with `--terminology`):

```
INFO sazare_server: Loading 111 custom profiles
INFO sazare_core::profile_loader: Loaded 8 SearchParameter resource(s) from "searchparameters"
INFO sazare_server: Terminology: 117 code systems, 107 value sets loaded (13 value sets not fully resolvable, left unchecked)
```

Loading the terminology adds about 60 MB of memory and 0.3 s of startup time.

A search parameter whose FHIRPath expression is outside the supported subset is skipped
with a `Skipping custom search parameter: ...` warning, rather than giving wrong results.
All 8 of JP Core 1.2.0 load.

### With Docker

The container's working directory is `/etc/sazare`; mount the directories there.

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
      - ./terminology:/etc/sazare/terminology:ro   # with --terminology
volumes:
  sazare-data:
```

### When there is data already

Search parameters added or changed later are noticed at the next start, and the index of
the existing data is rebuilt automatically (the log says `was built with different search
parameters; rebuilding`). To apply them without restarting, run
`curl -X POST http://localhost:8080/\$reindex`.

## 3. Try it with the sample data

```bash
bash examples/jp-core/load_examples.sh /srv/sazare
```

This `PUT`s the package's 76 examples (ids and references between them kept), runs JP Core
searches and validations, and checks the results. Any mismatch ends with exit code 1 (CI
runs it too). The search values are the examples' own data, so they are Japanese:

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

## Search parameters (JP Core 1.2.0)

| parameter | resource | type | example |
|---|---|---|---|
| `jp-kana-sort` | Patient | string | `Patient?jp-kana-sort=ヤマダ` (the `use=usual` kana name, prefix match) |
| `jp-insured-personsymbol` | Coverage | string | `Coverage?jp-insured-personsymbol=あいう` (insured person symbol) |
| `jp-insured-personnumber` | Coverage | string | `Coverage?jp-insured-personnumber=１８７` (insured person number) |
| `jp-insured-personsubnumber` | Coverage | string | `Coverage?jp-insured-personsubnumber=05` (branch number) |
| `jp-prefectureno` | Organization | token | `Organization?jp-prefectureno=13` (prefecture number) |
| `jp-insurance-organizationcategory` | Organization | token | `Organization?jp-insurance-organizationcategory=1` (fee schedule number) |
| `jp-insurance-organizationno` | Organization | token | `Organization?jp-insurance-organizationno=<system>\|1234567` (medical institution code) |
| `jp-medication-start` | MedicationRequest | date | `MedicationRequest?jp-medication-start=ge2020-01-01` (medication start date) |

Like the built-in parameters, they combine with date comparisons (`ge`, `lt`, …),
`system|code` tokens, `:exact` / `:contains`, chains and `_has`.

The built-in `name` parameter also finds names written in kanji or kana; `jp-kana-sort`
differs in matching only the `use=usual` kana form.

Spaces in values can be sent as `%20` or `+` (`curl --data-urlencode` uses `+`); write
`%2B` for a literal `+`. Date values are the exception: a `+` stays, so a time zone can be
written unencoded, as in `ge2024-01-01T10:00:00+09:00`.

## Validation example

A resource declaring a JP Core profile in `meta.profile` is checked for that profile's
required elements and other constraints.

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

### Code validation (`--terminology`)

Codes a profile binds as required are checked against the loaded code systems. For
example, a code in `JP_Procedure`'s `code.coding` that claims the JP procedure code system
must exist in that code system.

```
Profile 'http://jpfhir.jp/fhir/core/StructureDefinition/JP_Procedure': element 'Procedure.code.coding:medical'
code 'NOPE' is not in code system http://jpfhir.jp/fhir/core/CodeSystem/JP_ProcedureCodesMedical_CS
(required value set http://jpfhir.jp/fhir/core/ValueSet/JP_ProcedureCodesMedical_VS)
```

To avoid rejecting correct data, codes are checked only where the answer is certain:

- Only `content: complete` code systems, and ValueSets fully determined by them. ValueSets
  that can't be expanded completely (filters, references to other ValueSets — 13 of them)
  are not checked.
- A slice (`Observation.code.coding:jlac10`, …) is checked only when it fixes a code system
  and the ValueSet includes that whole code system. Codings of other systems (local codes,
  for instance) pass.

## Applying JP Core to data that declares no profile

Real-world data often has no `meta.profile`, so JP Core validation wouldn't apply. In
`config.yaml`, a default profile per resource type validates resources that declare none:

```yaml
validation:
  default_profiles:
    Patient: "http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"
    Procedure: "http://jpfhir.jp/fhir/core/StructureDefinition/JP_Procedure"
  default_profile_mode: enforce   # or warn
```

- `enforce` (default): a violation is a 400, as if the profile were declared. The reasons
  end with an information issue saying the default profile was used.
- `warn`: the resource is stored; violations go to `$validate` warnings and the server log.
  Useful when migrating existing data, to see how much violates first.
- Validation runs on a copy; `meta.profile` is not added to the stored resource.
- A resource declaring another profile (US Core, say) is validated against that one only.
- A default profile that isn't loaded (not in `profiles/`, a typo in the URL) stops the
  server at startup, so validation can't be switched off silently.

## Limitations

- Validated are resources that declare a profile in `meta.profile`, and resources of the
  types given a default profile in `validation.default_profiles`.
- Codes are validated only with `--terminology`. Even then, codes inside extensions (e.g.
  the prefecture number) and bindings to ValueSets that can't be expanded completely are
  not checked.
- A code system declared `content: complete` treats any code it lacks as an error. If
  codes in real use aren't in jpfhir-terminology yet, they are rejected; remove the file
  from `terminology/` in that case.
- sazare's formally tested conformance is US Core (the Inferno test suites). For JP Core,
  as above, all 76 package examples load and validate, and the 8 search parameters return
  the expected results.

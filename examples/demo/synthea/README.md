# Bulk demo data with Synthea

The [hero cohort](../cohort.json) is five hand-curated, legible patients — great
for *understanding* what a query does. When you want **volume** (a realistic
population to stress search, pagination, `$export`, dashboards), generate it with
[Synthea](https://github.com/synthetichealth/synthea).

Synthea is Apache-2.0, and its maintainers state the generated data is free of
cost, privacy, security, and licensing restrictions — so you can load and share
it freely. We deliberately **do not** commit Synthea output to this repo or bake
it into the binary; it can be tens of MB. Generate it locally, load it, done.

## 1. Generate

```bash
git clone https://github.com/synthetichealth/synthea
cd synthea
# 25 patients in Massachusetts as FHIR R4 transaction Bundles:
./run_synthea -p 25 Massachusetts
# Output lands in ./output/fhir/*.json (one transaction Bundle per patient,
# plus hospital/practitioner roster bundles).
```

Synthea emits US-Core-flavoured R4 by default in recent versions. See Synthea's
own docs for toggling the US Core IG, seeds, modules, and locale.

## 2. Load into sazare

```bash
# From the fhir-sazare repo, against a running server on :8080
bash examples/demo/synthea/load_synthea.sh /path/to/synthea/output/fhir
```

The script POSTs each bundle as a FHIR transaction. sazare resolves the
`urn:uuid` cross-references inside each bundle, so patients, encounters,
observations and their links all land intact. It prints a per-file
committed/total count.

## 3. Explore the population

```bash
# How many patients did we load?
curl -s "http://localhost:8080/Patient?_summary=count"

# Page through them
curl -s "http://localhost:8080/Patient?_count=20&_offset=0"

# Everyone with a recorded HbA1c (reverse chain)
curl -s "http://localhost:8080/Patient?_has:Observation:patient:code=4548-4&_summary=count"

# Bulk export the whole dataset as NDJSON
curl -s "http://localhost:8080/\$export"
```

## A note on validation

sazare runs full US Core profile validation only on resources that *claim* a
profile via `meta.profile`. Synthea resources that claim US Core profiles are
validated against the embedded US Core 8 set; if a generated resource is missing
a must-support element sazare enforces, that single entry is rejected and the
script reports `ok < total` for that file. The rest of the bundle still loads.
If you want everything to load unconditionally, generate Synthea without the US
Core IG (plain R4).

# Examples

Sample scripts to test sazare FHIR Server features.

## Prerequisites

Start the server first:

```bash
cargo run
```

## Scripts

| Script | Feature |
|--------|---------|
| `01_basic_crud.sh` | Create, Read, Update, Delete, History, vread |
| `02_search.sh` | Search by params, chain search, pagination |
| `03_bundle_transaction.sh` | Transaction bundle with urn:uuid reference resolution |
| `04_bundle_batch.sh` | Batch bundle (independent entries) |
| `05_conditional_create.sh` | If-None-Exist / ifNoneExist |
| `06_summary_elements.sh` | `_summary` and `_elements` filtering |
| `07_bulk_ndjson.sh` | NDJSON import and export |
| `08_validation.sh` | Resource validation (`$validate`) |

## Demo data — make search *visible*

See [`demo/`](demo/) for the **hero cohort**: five legible patients (diabetic,
hypertensive smoker, prenatal, well-child vaccines, oncology work-up) that make
`_has`, `_include` and `$everything` visibly do something.

```bash
bash demo/load_demo.sh                                # into a running server
SAZARE_SEED_ON_EMPTY=demo/cohort.json sazare-server   # at startup, if empty
```

For a realistic population, [`demo/synthea/`](demo/synthea/) generates and loads
synthetic data with Synthea. The US Core conformance seeds (`us-core-seed.json`
v7, `us-core-v8-seed.json` v8) are loaded by `09_us_core_seed.sh` /
`10_us_core_v8_seed.sh` for Inferno testing.

## Usage

```bash
# Run individual scripts
bash examples/01_basic_crud.sh

# Run all in order
for f in examples/0*.sh; do
  echo "======== $f ========"
  bash "$f"
  echo
done
```

## Notes

- Scripts assume the server is running at `http://localhost:8080` with default settings (no auth)
- Some scripts create test data used by later scripts (e.g., `02_search.sh` creates data for `06_summary_elements.sh`)
- Scripts use `python3 -m json.tool` for pretty-printing JSON responses

# Demo data — make FHIR *visible*

An empty FHIR server is hard to learn from: every search returns nothing, so the
interesting features (`_has`, `_include`, `$everything`, compartment search) have
nothing to act on. This directory fixes that with data that makes those features
visibly *do something*.

## The hero cohort — five legible patients

[`cohort.json`](cohort.json) is a transaction Bundle of **five hand-curated
patients**, each one a single readable clinical story:

| Patient | id | Story |
|---|---|---|
| Ann Davis (58F) | `demo-ann-davis` | Type 2 diabetes on metformin — HbA1c, BMI, BP |
| Carlos Ramirez (45M) | `demo-carlos-ramirez` | Hypertension, current smoker, on lisinopril |
| Emma Chen (29F) | `demo-emma-chen` | Uncomplicated pregnancy, prenatal visit |
| Liam Okafor (6M) | `demo-liam-okafor` | Well-child visit, DTaP/MMR/varicella |
| Margaret Flynn (72F) | `demo-margaret-flynn` | Breast-cancer work-up: order → result |

It uses real LOINC / SNOMED / RxNorm / CVX codes (so it's clinically legible) but
**does not claim `meta.profile`**, so it's base-R4-valid and always loads. The
*conformance* story is told separately by [`../us-core-v8-seed.json`](../us-core-v8-seed.json),
the frozen Inferno seed. The source of truth is [`build_cohort.py`](build_cohort.py)
— read it to see each story as data; run it to regenerate `cohort.json`.

## Load it — two ways (pick either)

**Over HTTP, into a running server:**

```bash
bash examples/demo/load_demo.sh
# remote + auth:
SAZARE_URL=https://your-server SAZARE_USER=admin SAZARE_PASS=secret \
  bash examples/demo/load_demo.sh
```

**At startup, into a fresh store** (great for Docker — populates only if empty):

```bash
SAZARE_SEED_ON_EMPTY=examples/demo/cohort.json sazare-server
# docker:
docker run -e SAZARE_SEED_ON_EMPTY=/data/cohort.json -v "$PWD/examples/demo:/data" ...
```

There's also the tiny built-in set (`sazare-server --demo` or `POST /$demo`, two
patients, embedded) for a zero-file taste.

## Now FHIR does something

```bash
# Everyone in the population
curl -s "http://localhost:8080/Patient"

# Every patient who has an HbA1c result — reverse chain (_has)
curl -s "http://localhost:8080/Patient?_has:Observation:patient:code=4548-4"

# Ann Davis's entire chart in one call
curl -s "http://localhost:8080/Patient/demo-ann-davis/\$everything"

# All Type 2 diabetics
curl -s "http://localhost:8080/Condition?code=http://snomed.info/sct|44054006"

# Prescriptions, each with its patient pulled in (_include)
curl -s "http://localhost:8080/MedicationRequest?_include=MedicationRequest:subject"
```

## Want a real population?

For volume (pagination, `$export`, dashboards), generate a synthetic population
with Synthea — see [`synthea/README.md`](synthea/README.md).

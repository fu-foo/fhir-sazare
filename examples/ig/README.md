# Loading an implementation guide

sazare validates against US Core out of the box. Any other FHIR implementation guide
(IG) — a national base profile set such as JP Core, a project's own profiles — is loaded
at startup from three directories in the server's **working directory**:

| directory | holds | gives you |
|---|---|---|
| `profiles/` | `StructureDefinition-*.json` | validation of resources declaring those profiles (or given them as defaults) |
| `searchparameters/` | `SearchParameter-*.json` | the IG's search parameters, usable like built-in ones |
| `terminology/` | `CodeSystem-*.json`, `ValueSet-*.json` | required bindings to those code systems are checked |

Nothing is bundled; you fetch the IG's published package.

## 1. Fetch the package

`setup.sh` downloads one or more packages (`.tgz`) and lays them out in those
directories:

```bash
bash examples/ig/setup.sh --dir /srv/sazare <package-url>...
bash examples/ig/setup.sh --dir /srv/sazare --terminology <package-url>      # also code systems
bash examples/ig/setup.sh --dir /srv/sazare --terminology <package-url> \
     --terminology-package <terminology-package-url>                        # an IG's separate terminology
```

For example, JP Core 1.2.0 with its terminology:

```bash
bash examples/ig/setup.sh --dir /srv/sazare --terminology \
  https://jpfhir.jp/fhir/core/1.2.0/package.tgz \
  --terminology-package https://jpfhir.jp/fhir/core/terminology/jpfhir-terminology.r4-2.2611.0.tgz
```

```
/srv/sazare/
├── profiles/           StructureDefinition-*.json
├── searchparameters/   SearchParameter-*.json
├── terminology/        CodeSystem-*.json / ValueSet-*.json (with --terminology)
└── .ig/<n>/package/    each unpacked package (its examples: load_examples.sh)
```

You can also copy the files there by hand; the script is a convenience.

## 2. Start sazare from that directory

```bash
cd /srv/sazare
sazare-server start
```

The log (`data/logs/sazare.log`) says what was loaded, e.g. for JP Core:

```
INFO sazare_server: Loading 111 custom profiles
INFO sazare_core::profile_loader: Loaded 8 SearchParameter resource(s) from "searchparameters"
INFO sazare_server: Terminology: 117 code systems, 107 value sets loaded (13 value sets not fully resolvable, left unchecked)
```

- A search parameter whose FHIRPath expression is outside the supported subset is
  skipped with a `Skipping custom search parameter: ...` warning, rather than giving wrong
  results.
- Search parameters added or changed later are noticed at the next start; the index of
  existing data is rebuilt automatically (the log says `was built with different search
  parameters; rebuilding`). `POST /$reindex` applies them without a restart.

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
      - ./terminology:/etc/sazare/terminology:ro
volumes:
  sazare-data:
```

## 3. Load the package's examples

```bash
bash examples/ig/load_examples.sh /srv/sazare
```

`PUT`s every example of the fetched packages (ids and the references between them kept)
and fails if any is refused — a quick check that the IG's own examples validate.

## Validation

A resource declaring a loaded profile in `meta.profile` is checked against it: required
elements, cardinality, fixed and pattern values, required slices, extensions. A create or
update that violates it is a 400; `POST /{type}/$validate` returns the reasons.

### Code systems (`terminology/`)

Codes that a profile binds as **required** are checked against the loaded code systems.
To avoid rejecting correct data, only where the answer is certain:

- only `content: complete` code systems, and ValueSets fully determined by them —
  ValueSets with filters or references to other ValueSets are left unchecked (the log
  counts them);
- a slice (`Observation.code.coding:xyz`) is checked only when it fixes a code system and
  the ValueSet includes that whole code system; codings of other systems pass.

A code system declared `content: complete` treats any code it lacks as an error. If codes
in real use are missing from it, remove that file from `terminology/`.

### Data that declares no profile

Real-world data often has no `meta.profile`. A default profile per resource type in
`config.yaml` validates resources that declare none:

```yaml
validation:
  default_profiles:
    Patient: "http://example.org/fhir/StructureDefinition/MyPatient"
  default_profile_mode: enforce   # or warn
```

- `enforce` (default): a violation is a 400, as if the profile were declared; the reasons
  end with an information issue saying the default profile was used.
- `warn`: the resource is stored; violations go to `$validate` warnings and the log —
  useful to measure a migration first.
- Validation runs on a copy; `meta.profile` is not added to the stored resource.
- A resource declaring another profile is validated against that one only.
- A default profile that isn't loaded stops the server at startup, so validation can't be
  switched off silently.

## What is tested

US Core is tested with the Inferno suites in CI. JP Core is loaded in CI with this
script: all 76 package examples load and validate, and its 8 search parameters return
the expected results (`ci/jp_core_check.sh`).

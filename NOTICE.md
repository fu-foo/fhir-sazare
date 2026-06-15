# NOTICE

fhir-sazare is licensed under the Apache License 2.0 (see `LICENSE`).

It **embeds** third-party FHIR conformance artifacts (profiles) so the single
binary can validate without a network connection. Those artifacts are not
original to this project; they are redistributed here under their own license,
which permits this use. This file records their provenance so anyone
redistributing fhir-sazare can see exactly what is bundled and under what terms.

The bundled profiles are released under **CC0 1.0 Universal** (public domain
dedication), which places no conditions on redistribution. The attribution below
is courtesy, not a legal requirement.

## Bundled profiles

### HL7 FHIR US Core Implementation Guide
- **What:** the complete set of 54 `StructureDefinition` profiles in `sazare-core/profiles/us-core/`
- **Source package:** `hl7.fhir.us.core` version `8.0.0`
- **Publisher:** HL7 International
- **License:** CC0-1.0 (verified in the package manifest)

US Core is the only conformance content embedded in the binary. Other
Implementation Guides (e.g. HL7 FHIR JP Core) are **not** bundled — load them at
runtime by dropping their `StructureDefinition` resources into a `profiles/`
directory next to the binary (see the README).

## Base specification

This server implements **HL7 FHIR R4 (4.0.1)**. The FHIR specification material is
published by HL7 International under CC0-1.0. No restrictively licensed code
system content (e.g. SNOMED CT, LOINC) is embedded — profiles reference such code
systems by canonical URL only, which carries no redistribution obligation.

## Trademarks

HL7®, FHIR® and the FHIR® flame icon are registered trademarks of Health Level
Seven International. Use of these names does not imply endorsement.

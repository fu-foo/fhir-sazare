# NOTICE

fhir-sazare is licensed under the Apache License 2.0 (see `LICENSE`).

It **embeds** third-party FHIR conformance artifacts (profiles and a value set)
so the single binary can validate without a network connection. Those artifacts
are not original to this project; they are redistributed here under their own
licenses, which permit this use. This file records their provenance so anyone
redistributing fhir-sazare can see exactly what is bundled and under what terms.

All bundled conformance artifacts are released under **CC0 1.0 Universal**
(public domain dedication), which places no conditions on redistribution. The
attributions below are courtesy, not a legal requirement.

## Bundled profiles

### HL7 FHIR US Core Implementation Guide
- **What:** 29 `StructureDefinition` profiles in `sazare-core/profiles/us-core/`
- **Source package:** `hl7.fhir.us.core` version `5.0.1`
- **Publisher:** HL7 International
- **License:** CC0-1.0 (verified in the package manifest)

### HL7 FHIR JP Core Implementation Guide
- **What:** 44 `StructureDefinition` profiles in `sazare-core/profiles/jp-core/`
- **Source package:** `jpfhir.jp.core` version `1.2.0`
  (https://jpfhir.jp/fhir/core/1.2.0/package.tgz)
- **Publisher / author:** FHIR Japanese implementation research working group in
  the Japan Association of Medical Informatics (JAMI) —
  一般社団法人日本医療情報学会 FHIR 国内実装基盤研究会
- **License:** CC0-1.0 (verified in the package manifest)

## Bundled value set

### DICOM modality codes
- **What:** `sazare-core/valuesets/jp-dicom-modality.json` — 54 modality codes
- **Code system:** DICOM Controlled Terminology (PS3.16),
  `http://dicom.nema.org/resources/ontology/DCM`
- **Source:** © NEMA. The DICOM standard is published by NEMA and is available at
  no cost for use. Only these modality codes are reproduced; no other DICOM
  content is embedded.

## Base specification

This server implements **HL7 FHIR R4 (4.0.1)**. The FHIR specification material is
published by HL7 International under CC0-1.0. No restrictively licensed code
system content (e.g. SNOMED CT, LOINC) is embedded — profiles reference such code
systems by canonical URL only, which carries no redistribution obligation.

## Trademarks

HL7®, FHIR® and the FHIR® flame icon are registered trademarks of Health Level
Seven International. DICOM® is a registered trademark of the National Electrical
Manufacturers Association (NEMA). Use of these names does not imply endorsement.

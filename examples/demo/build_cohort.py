#!/usr/bin/env python3
"""Build the sazare demo cohort — a small, *legible* US-Core-shaped population.

This is the curated source of truth. Each patient below is one readable clinical
story (the diabetic, the hypertensive smoker, the prenatal visit, the well-child
vaccine schedule, the oncology work-up). Run this to (re)generate `cohort.json`,
a FHIR transaction Bundle you load with `load_demo.sh`.

Design choices (deliberate):
  * Stable, human IDs (`demo-ann-davis`) so docs and queries can name patients.
  * Real code systems (LOINC / SNOMED / RxNorm / CVX / ICD-10) so the data is
    clinically legible and US-Core-flavoured...
  * ...but NO `meta.profile` claims. sazare only runs full US Core profile
    validation when a resource claims a profile, so leaving it off keeps this
    set base-R4-valid and guaranteed to load. The *conformance* story is told by
    `examples/us-core-v8-seed.json` (the frozen Inferno seed); this set's job is
    to make search, _has, _include and $everything visibly *do something*.

No dependencies — standard library only. `python3 build_cohort.py`.
"""
import json
import os

# ---- Code system URIs -------------------------------------------------------
LOINC = "http://loinc.org"
SNOMED = "http://snomed.info/sct"
RXNORM = "http://www.nlm.nih.gov/research/umls/rxnorm"
ICD10 = "http://hl7.org/fhir/sid/icd-10-cm"
CVX = "http://hl7.org/fhir/sid/cvx"
UCUM = "http://unitsofmeasure.org"
ACT = "http://terminology.hl7.org/CodeSystem/v3-ActCode"
COND_CLIN = "http://terminology.hl7.org/CodeSystem/condition-clinical"
COND_VER = "http://terminology.hl7.org/CodeSystem/condition-ver-status"
COND_CAT = "http://terminology.hl7.org/CodeSystem/condition-category"
OBS_CAT = "http://terminology.hl7.org/CodeSystem/observation-category"
MEDREQ_CAT = "http://terminology.hl7.org/CodeSystem/medicationrequest-category"


# ---- Small builders (each models a validated shape from the seed) -----------
def cc(system, code, display):
    """A CodeableConcept with a single coding."""
    return {"coding": [{"system": system, "code": code, "display": display}]}


def patient(pid, family, given, gender, birth_date):
    return {
        "resourceType": "Patient",
        "id": pid,
        "identifier": [{
            "system": "http://hospital.example.org/mrn",
            "value": pid.replace("demo-", "MRN-").upper(),
        }],
        "name": [{"family": family, "given": [given]}],
        "gender": gender,
        "birthDate": birth_date,
    }


def condition(cid, subject, code_cc, onset, category="problem-list-item"):
    return {
        "resourceType": "Condition",
        "id": cid,
        "clinicalStatus": cc(COND_CLIN, "active", "Active"),
        "verificationStatus": cc(COND_VER, "confirmed", "Confirmed"),
        "category": [cc(COND_CAT, category, "Problem List Item")],
        "code": code_cc,
        "subject": {"reference": f"Patient/{subject}"},
        "onsetDateTime": onset,
        "recordedDate": onset,
    }


def observation(oid, subject, code_cc, when, *, value=None, components=None,
                category="vital-signs", category_display="Vital Signs"):
    obs = {
        "resourceType": "Observation",
        "id": oid,
        "status": "final",
        "category": [cc(OBS_CAT, category, category_display)],
        "code": code_cc,
        "subject": {"reference": f"Patient/{subject}"},
        "effectiveDateTime": when,
    }
    if value is not None:
        obs["valueQuantity"] = value
    if components is not None:
        obs["component"] = components
    return obs


def qty(v, unit, code=None):
    return {"value": v, "unit": unit, "system": UCUM, "code": code or unit}


def med_request(mid, subject, med_cc, when, reason_cc=None):
    r = {
        "resourceType": "MedicationRequest",
        "id": mid,
        "status": "active",
        "intent": "order",
        "category": [cc(MEDREQ_CAT, "outpatient", "Outpatient")],
        "medicationCodeableConcept": med_cc,
        "subject": {"reference": f"Patient/{subject}"},
        "authoredOn": when,
    }
    if reason_cc is not None:
        r["reasonCode"] = [reason_cc]
    return r


def immunization(iid, patient_id, vaccine_cc, when):
    return {
        "resourceType": "Immunization",
        "id": iid,
        "status": "completed",
        "vaccineCode": vaccine_cc,
        "patient": {"reference": f"Patient/{patient_id}"},
        "occurrenceDateTime": when,
        "primarySource": True,
    }


def encounter(eid, subject, when, klass_code="AMB", klass_display="ambulatory",
              type_cc=None):
    e = {
        "resourceType": "Encounter",
        "id": eid,
        "status": "finished",
        "class": {"system": ACT, "code": klass_code, "display": klass_display},
        "subject": {"reference": f"Patient/{subject}"},
        "period": {"start": when, "end": when},
    }
    if type_cc is not None:
        e["type"] = [type_cc]
    return e


def service_request(sid, subject, code_cc, when):
    return {
        "resourceType": "ServiceRequest",
        "id": sid,
        "status": "active",
        "intent": "order",
        "code": code_cc,
        "subject": {"reference": f"Patient/{subject}"},
        "authoredOn": when,
    }


def diagnostic_report(did, subject, code_cc, when, result_refs=None):
    r = {
        "resourceType": "DiagnosticReport",
        "id": did,
        "status": "final",
        "code": code_cc,
        "subject": {"reference": f"Patient/{subject}"},
        "effectiveDateTime": when,
        "issued": when + "T12:00:00Z" if "T" not in when else when,
    }
    if result_refs:
        r["result"] = [{"reference": ref} for ref in result_refs]
    return r


# ---- The cohort: five legible stories ---------------------------------------
def cohort():
    out = []

    # 1) Ann Davis, 58F — Type 2 diabetes on metformin. The "chase the HbA1c" story.
    out += [
        patient("demo-ann-davis", "Davis", "Ann", "female", "1967-03-12"),
        condition("demo-ann-t2dm", "demo-ann-davis",
                  cc(SNOMED, "44054006", "Type 2 diabetes mellitus"), "2019-06-01"),
        observation("demo-ann-hba1c", "demo-ann-davis",
                    cc(LOINC, "4548-4", "Hemoglobin A1c/Hemoglobin.total in Blood"),
                    "2026-05-20", value=qty(8.2, "%"),
                    category="laboratory", category_display="Laboratory"),
        observation("demo-ann-bmi", "demo-ann-davis",
                    cc(LOINC, "39156-5", "Body mass index (BMI) [Ratio]"),
                    "2026-05-20", value=qty(31.2, "kg/m2")),
        observation("demo-ann-bp", "demo-ann-davis",
                    cc(LOINC, "85354-9", "Blood pressure panel"), "2026-05-20",
                    components=[
                        {"code": cc(LOINC, "8480-6", "Systolic blood pressure"),
                         "valueQuantity": qty(148, "mmHg", "mm[Hg]")},
                        {"code": cc(LOINC, "8462-4", "Diastolic blood pressure"),
                         "valueQuantity": qty(92, "mmHg", "mm[Hg]")},
                    ]),
        med_request("demo-ann-metformin", "demo-ann-davis",
                    cc(RXNORM, "860975", "metformin hydrochloride 500 MG Oral Tablet"),
                    "2026-05-20",
                    reason_cc=cc(SNOMED, "44054006", "Type 2 diabetes mellitus")),
        encounter("demo-ann-enc", "demo-ann-davis", "2026-05-20"),
    ]

    # 2) Carlos Ramirez, 45M — Hypertension, current smoker. The "modifiable risk" story.
    out += [
        patient("demo-carlos-ramirez", "Ramirez", "Carlos", "male", "1980-11-02"),
        condition("demo-carlos-htn", "demo-carlos-ramirez",
                  cc(SNOMED, "59621000", "Essential hypertension"), "2021-02-15"),
        observation("demo-carlos-bp", "demo-carlos-ramirez",
                    cc(LOINC, "85354-9", "Blood pressure panel"), "2026-04-10",
                    components=[
                        {"code": cc(LOINC, "8480-6", "Systolic blood pressure"),
                         "valueQuantity": qty(150, "mmHg", "mm[Hg]")},
                        {"code": cc(LOINC, "8462-4", "Diastolic blood pressure"),
                         "valueQuantity": qty(95, "mmHg", "mm[Hg]")},
                    ]),
        observation("demo-carlos-smoking", "demo-carlos-ramirez",
                    cc(LOINC, "72166-2", "Tobacco smoking status"), "2026-04-10",
                    category="social-history", category_display="Social History"),
        observation("demo-carlos-weight", "demo-carlos-ramirez",
                    cc(LOINC, "29463-7", "Body weight"), "2026-04-10",
                    value=qty(94.5, "kg")),
        med_request("demo-carlos-lisinopril", "demo-carlos-ramirez",
                    cc(RXNORM, "314076", "lisinopril 10 MG Oral Tablet"), "2026-04-10",
                    reason_cc=cc(SNOMED, "59621000", "Essential hypertension")),
        encounter("demo-carlos-enc", "demo-carlos-ramirez", "2026-04-10"),
    ]
    # Smoking status uses a SNOMED value, carried as the Observation value.
    out[-4]["valueCodeableConcept"] = cc(SNOMED, "449868002",
                                         "Smokes tobacco daily")

    # 3) Emma Chen, 29F — Prenatal visit, uncomplicated pregnancy.
    out += [
        patient("demo-emma-chen", "Chen", "Emma", "female", "1997-07-19"),
        condition("demo-emma-pregnancy", "demo-emma-chen",
                  cc(SNOMED, "72892002", "Normal pregnancy"), "2026-01-05",
                  category="encounter-diagnosis"),
        observation("demo-emma-bp", "demo-emma-chen",
                    cc(LOINC, "85354-9", "Blood pressure panel"), "2026-05-30",
                    components=[
                        {"code": cc(LOINC, "8480-6", "Systolic blood pressure"),
                         "valueQuantity": qty(118, "mmHg", "mm[Hg]")},
                        {"code": cc(LOINC, "8462-4", "Diastolic blood pressure"),
                         "valueQuantity": qty(72, "mmHg", "mm[Hg]")},
                    ]),
        observation("demo-emma-ega", "demo-emma-chen",
                    cc(LOINC, "11884-4", "Gestational age Estimated"), "2026-05-30",
                    value=qty(28, "weeks", "wk"),
                    category="exam", category_display="Exam"),
        observation("demo-emma-weight", "demo-emma-chen",
                    cc(LOINC, "29463-7", "Body weight"), "2026-05-30",
                    value=qty(68.0, "kg")),
        encounter("demo-emma-enc", "demo-emma-chen", "2026-05-30"),
    ]

    # 4) Liam Okafor, 6M (pediatric) — Well-child visit, vaccine schedule.
    out += [
        patient("demo-liam-okafor", "Okafor", "Liam", "male", "2020-02-14"),
        immunization("demo-liam-dtap", "demo-liam-okafor",
                     cc(CVX, "20", "diphtheria, tetanus toxoids and acellular pertussis vaccine"),
                     "2026-03-01"),
        immunization("demo-liam-mmr", "demo-liam-okafor",
                     cc(CVX, "03", "measles, mumps and rubella virus vaccine"),
                     "2026-03-01"),
        immunization("demo-liam-varicella", "demo-liam-okafor",
                     cc(CVX, "21", "varicella virus vaccine"), "2026-03-01"),
        observation("demo-liam-height", "demo-liam-okafor",
                    cc(LOINC, "8302-2", "Body height"), "2026-03-01",
                    value=qty(115, "cm")),
        observation("demo-liam-weight", "demo-liam-okafor",
                    cc(LOINC, "29463-7", "Body weight"), "2026-03-01",
                    value=qty(20.5, "kg")),
        observation("demo-liam-temp", "demo-liam-okafor",
                    cc(LOINC, "8310-5", "Body temperature"), "2026-03-01",
                    value=qty(36.8, "Cel")),
        encounter("demo-liam-enc", "demo-liam-okafor", "2026-03-01"),
    ]

    # 5) Margaret Flynn, 72F — Breast cancer work-up: order -> specimen -> result.
    out += [
        patient("demo-margaret-flynn", "Flynn", "Margaret", "female", "1953-09-30"),
        condition("demo-margaret-breast-ca", "demo-margaret-flynn",
                  cc(SNOMED, "254837009", "Malignant tumor of breast"), "2026-02-10"),
        service_request("demo-margaret-order", "demo-margaret-flynn",
                        cc(LOINC, "58410-2", "CBC panel - Blood by Automated count"),
                        "2026-02-12"),
        observation("demo-margaret-ca153", "demo-margaret-flynn",
                    cc(LOINC, "6875-9", "Cancer Ag 15-3 [Units/volume] in Serum or Plasma"),
                    "2026-02-20", value=qty(48, "U/mL"),
                    category="laboratory", category_display="Laboratory"),
        diagnostic_report("demo-margaret-path", "demo-margaret-flynn",
                          cc(LOINC, "60568-3", "Pathology Synoptic report"),
                          "2026-02-20",
                          result_refs=["Observation/demo-margaret-ca153"]),
        encounter("demo-margaret-enc", "demo-margaret-flynn", "2026-02-20"),
    ]

    return out


def main():
    resources = cohort()
    bundle = {
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [
            {
                "fullUrl": f"{r['resourceType']}/{r['id']}",
                "resource": r,
                "request": {"method": "PUT", "url": f"{r['resourceType']}/{r['id']}"},
            }
            for r in resources
        ],
    }
    here = os.path.dirname(os.path.abspath(__file__))
    out_path = os.path.join(here, "cohort.json")
    with open(out_path, "w") as f:
        json.dump(bundle, f, indent=2)
        f.write("\n")

    by_type = {}
    for r in resources:
        by_type[r["resourceType"]] = by_type.get(r["resourceType"], 0) + 1
    print(f"Wrote {out_path}: {len(resources)} resources")
    for t in sorted(by_type):
        print(f"  {by_type[t]:2d} {t}")


if __name__ == "__main__":
    main()

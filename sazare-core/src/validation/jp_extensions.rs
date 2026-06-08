//! JP Core extension structure validation.
//!
//! JP Core defines single-valued extensions with a fixed `value[x]` type
//! (e.g. `JP_Coverage_InsuredPersonNumber` is a `valueString`,
//! `JP_Organization_PrefectureNo` a `valueCoding`). This validates that, when
//! a resource uses one of these extensions anywhere (top-level or nested), it
//! carries the correct value type. Unknown extensions are ignored.

use crate::operation_outcome::OperationOutcome;
use serde_json::Value;

/// (extension url, expected `value[x]` field) for JP Core's single-valued
/// extensions, generated from the `jp-core.r4` 1.2.0 package.
const JP_EXTENSION_VALUE_TYPES: &[(&str, &str)] = &[
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Condition_DiseaseOutcome", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Condition_DiseasePostfixModifier", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Condition_DiseasePrefixModifier", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonNumber", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonSubNumber", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonSymbol", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Immunization_CertificatedDate", "valueDate"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Immunization_DueDateOfNextDose", "valueDate"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Immunization_ManufacturedDate", "valueDate"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationAdministration_Location", "valueReference"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationAdministration_RequestAuthoredOn", "valueDateTime"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationAdministration_RequestDepartment", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationAdministration_Requester", "valueReference"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationAdministration_UncategorizedComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDispense_Preparation", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_Device", "valueReference"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_DosageComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_Line", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_LineComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_MethodComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_PeriodOfUse", "valuePeriod"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_RateComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_RouteComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_SiteComment", "valueString"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_UsageDuration", "valueDuration"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationRequest_DispenseRequest_ExpectedRepeatCount", "valueInteger"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationRequest_DispenseRequest_InstructionForDispense", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Medication_IngredientStrength_StrengthType", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Medication_Ingredient_DrugNo", "valueInteger"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_DentalOral_BodySiteStatus", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_DentalOral_ToothRoot", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_DentalOral_ToothSurface", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_Electrocardiogram_DeviceInterpretation", "valueBoolean"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_Electrocardiogram_Duration", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_Electrocardiogram_NumberOfLead", "valueInteger"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Observation_Electrocardiogram_StressType", "valueCodeableConcept"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Organization_InsuranceOrganizationCategory", "valueCoding"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Organization_InsuranceOrganizationNo", "valueIdentifier"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Organization_PrefectureNo", "valueCoding"),
    ("http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Patient_Race", "valueCodeableConcept"),
];

/// Validate JP Core extension value types anywhere in the resource.
pub fn validate(resource: &Value) -> Result<(), OperationOutcome> {
    match resource {
        Value::Object(map) => {
            if let Some(url) = map.get("url").and_then(|v| v.as_str())
                && let Some((_, expected)) =
                    JP_EXTENSION_VALUE_TYPES.iter().find(|(u, _)| *u == url)
                && !map.contains_key(*expected)
            {
                return Err(OperationOutcome::validation_error(format!(
                    "JP Core extension '{}' must carry '{}'",
                    url, expected
                ))
                .with_expression(vec!["extension".to_string()]));
            }
            for v in map.values() {
                validate(v)?;
            }
            Ok(())
        }
        Value::Array(arr) => {
            for v in arr {
                validate(v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_correct_jp_extension_type_passes() {
        let coverage = json!({
            "resourceType": "Coverage",
            "extension": [{
                "url": "http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonNumber",
                "valueString": "12345"
            }]
        });
        assert!(validate(&coverage).is_ok());
    }

    #[test]
    fn test_wrong_jp_extension_type_fails() {
        // InsuredPersonNumber must be valueString, not valueInteger.
        let coverage = json!({
            "resourceType": "Coverage",
            "extension": [{
                "url": "http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonNumber",
                "valueInteger": 12345
            }]
        });
        assert!(validate(&coverage).is_err());
    }

    #[test]
    fn test_nested_jp_extension_validated() {
        // The dosage period-of-use extension (valuePeriod) used with the wrong
        // type, nested under dosageInstruction, must be caught.
        let mr = json!({
            "resourceType": "MedicationRequest",
            "dosageInstruction": [{
                "extension": [{
                    "url": "http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_PeriodOfUse",
                    "valueString": "2025-12-01"
                }]
            }]
        });
        assert!(validate(&mr).is_err());
    }

    #[test]
    fn test_unknown_extension_ignored() {
        let r = json!({
            "resourceType": "Patient",
            "extension": [{"url": "http://example.com/custom", "valueInteger": 1}]
        });
        assert!(validate(&r).is_ok());
    }
}

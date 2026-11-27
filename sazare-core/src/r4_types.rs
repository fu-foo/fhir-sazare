//! The resource types of FHIR R4 (4.0.1): the first column of
//! `summary-elements/r4.tsv`, generated from the R4 StructureDefinitions.

use std::collections::BTreeSet;
use std::sync::LazyLock;

static TYPES: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    include_str!("../summary-elements/r4.tsv").lines().filter_map(|l| l.split('\t').next()).filter(|t| !t.is_empty()).collect()
});

/// Whether `name` is an R4 resource type (`Patient`, `Subscription`, …).
pub fn is_resource_type(name: &str) -> bool {
    TYPES.contains(name)
}

/// Every R4 resource type, sorted.
pub fn resource_types() -> impl Iterator<Item = &'static str> {
    TYPES.iter().copied()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_r4_resource_types() {
        assert_eq!(super::resource_types().count(), 146);
        for t in ["Patient", "Subscription", "Medication", "Bundle", "Group", "Appointment"] {
            assert!(super::is_resource_type(t), "{t}");
        }
        for t in ["Foo", "patient", "Resource", "DomainResource", ""] {
            assert!(!super::is_resource_type(t), "{t}");
        }
    }
}

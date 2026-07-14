use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use arrow::datatypes::DataType;

pub mod live;
pub mod snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationMode {
    Snapshot,
    Live,
    Both,
}

impl OperationMode {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("both") {
            "snapshot" => Ok(Self::Snapshot),
            "live" => Ok(Self::Live),
            "both" => Ok(Self::Both),
            value => Err(format!(
                "mode must be 'snapshot', 'live', or 'both', got '{value}'"
            )),
        }
    }

    pub fn wants_snapshot(self) -> bool {
        matches!(self, Self::Snapshot | Self::Both)
    }

    pub fn wants_live(self) -> bool {
        matches!(self, Self::Live | Self::Both)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicSelector {
    pub topic: String,
    pub source: Option<String>,
    pub instance: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformSpec {
    pub input: TopicSelector,
    pub multiplier: f64,
    pub offset: f64,
    pub fields: Option<Vec<String>>,
    pub unit: Option<String>,
    pub units: HashMap<String, String>,
    pub output_topic: String,
    pub mode: OperationMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeSpec {
    pub topics: Vec<(String, Vec<String>)>,
    pub base_topic: String,
    pub output_topic: String,
    pub source: Option<String>,
    pub output_names: Vec<Vec<String>>,
    pub mode: OperationMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupBySpec {
    pub input: TopicSelector,
    pub field: String,
    pub fields: Option<Vec<String>>,
    pub output_template: String,
    pub mode: OperationMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OperationSpec {
    Transform(TransformSpec),
    Merge(MergeSpec),
    GroupBy(GroupBySpec),
}

pub type OperationBuffer = Rc<RefCell<Vec<OperationSpec>>>;

pub(crate) type OutputSchema = Vec<(String, DataType, Option<String>)>;

#[derive(Debug, Clone, PartialEq)]
struct TopicClaim {
    operation: usize,
    schema: Option<OutputSchema>,
}

/// Generation-wide ownership and schema pins for the single declarative source.
#[derive(Debug, Clone, Default)]
pub struct TopicRegistry {
    claims: HashMap<String, TopicClaim>,
}

impl TopicRegistry {
    pub(crate) fn preclaim_static(&mut self, specs: &[OperationSpec]) -> Result<(), String> {
        for (operation, spec) in specs.iter().enumerate() {
            let topic = match spec {
                OperationSpec::Transform(spec) => Some(spec.output_topic.as_str()),
                OperationSpec::Merge(spec) => Some(spec.output_topic.as_str()),
                OperationSpec::GroupBy(_) => None,
            };
            if let Some(topic) = topic {
                self.claim_batch(operation, &[(topic.to_owned(), None)])?;
            }
        }
        Ok(())
    }

    /// Validate an entire output group first, then commit every claim together.
    pub(crate) fn claim_batch(
        &mut self,
        operation: usize,
        claims: &[(String, Option<OutputSchema>)],
    ) -> Result<(), String> {
        let mut staged = HashMap::<&str, &Option<OutputSchema>>::new();
        for (topic, schema) in claims {
            if let Some(previous) = staged.insert(topic, schema)
                && previous != schema
            {
                return Err(format!(
                    "output topic '{topic}' has conflicting schemas in operation {operation}"
                ));
            }
            if let Some(existing) = self.claims.get(topic) {
                if existing.operation != operation {
                    return Err(format!(
                        "output topic '{topic}' is owned by operation {}; operation {operation} cannot also produce it",
                        existing.operation
                    ));
                }
                if let (Some(existing), Some(schema)) = (&existing.schema, schema)
                    && existing != schema
                {
                    return Err(format!(
                        "output topic '{topic}' schema changed from {existing:?} to {schema:?}"
                    ));
                }
            }
        }

        for (topic, schema) in claims {
            let claim = self.claims.entry(topic.clone()).or_insert(TopicClaim {
                operation,
                schema: None,
            });
            if claim.schema.is_none() {
                claim.schema.clone_from(schema);
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_transform(
    multiplier: f64,
    offset: f64,
    unit: Option<&str>,
    units: &HashMap<String, String>,
) -> Result<(), String> {
    if !multiplier.is_finite() {
        return Err("transform multiplier must be finite".to_owned());
    }
    if !offset.is_finite() {
        return Err("transform offset must be finite".to_owned());
    }
    if unit.is_some() && !units.is_empty() {
        return Err("transform unit and units are mutually exclusive".to_owned());
    }
    Ok(())
}

pub(crate) fn validate_group_template(template: &str) -> Result<(), String> {
    if !template.contains("{value}") {
        return Err("group_by output_topic must contain '{value}'".to_owned());
    }
    Ok(())
}

pub(crate) fn merged_field_names(topics: &[(&str, Vec<&str>)]) -> Result<Vec<String>, String> {
    let mut counts = HashMap::new();
    for (_, fields) in topics {
        for field in fields {
            *counts.entry(*field).or_insert(0usize) += 1;
        }
    }

    let mut names = Vec::new();
    let mut unique = HashSet::new();
    for (topic, fields) in topics {
        for field in fields {
            let name = if counts[field] > 1 {
                format!("{topic}_{field}")
            } else {
                (*field).to_owned()
            };
            if !unique.insert(name.clone()) {
                return Err(format!("merge output field name '{name}' is duplicated"));
            }
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn merge_prefixes_every_colliding_name() {
        let names = merged_field_names(&[
            ("ATTITUDE", vec!["roll", "pitch"]),
            ("TARGET", vec!["roll"]),
        ])
        .unwrap();
        assert_eq!(names, vec!["ATTITUDE_roll", "pitch", "TARGET_roll"]);
    }

    #[test]
    fn merge_rejects_duplicate_names_after_prefixing() {
        assert!(merged_field_names(&[("A", vec!["x", "A_x"]), ("B", vec!["x"])]).is_err());
    }

    #[test]
    fn group_template_requires_value() {
        assert_eq!(
            validate_group_template("{topic}/fixed").unwrap_err(),
            "group_by output_topic must contain '{value}'"
        );
        assert!(validate_group_template("{topic}/{value}").is_ok());
    }

    #[test]
    fn mode_defaults_and_rejects_unknown_values() {
        assert_eq!(OperationMode::parse(None).unwrap(), OperationMode::Both);
        assert_eq!(
            OperationMode::parse(Some("snapshot")).unwrap(),
            OperationMode::Snapshot
        );
        assert_eq!(
            OperationMode::parse(Some("live")).unwrap(),
            OperationMode::Live
        );
        assert_eq!(
            OperationMode::parse(Some("both")).unwrap(),
            OperationMode::Both
        );
        assert!(OperationMode::parse(Some("stream")).is_err());
    }

    #[test]
    fn transform_requires_finite_operands() {
        assert!(validate_transform(f64::NAN, 0.0, None, &HashMap::new()).is_err());
        assert!(validate_transform(1.0, f64::INFINITY, None, &HashMap::new()).is_err());
        assert!(validate_transform(1.0, 0.0, None, &HashMap::new()).is_ok());
    }

    #[test]
    fn transform_unit_overrides_are_mutually_exclusive() {
        let units = HashMap::from([("roll".to_owned(), "deg".to_owned())]);
        assert!(validate_transform(1.0, 0.0, Some("deg"), &units).is_err());
    }

    fn schema(fields: &[(&str, DataType, Option<&str>)]) -> OutputSchema {
        fields
            .iter()
            .map(|(name, dtype, unit)| ((*name).to_owned(), dtype.clone(), unit.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn topic_registry_rejects_renames_reordering_types_and_units_for_the_owner() {
        let original = schema(&[
            ("roll", DataType::Float64, Some("rad")),
            ("frame", DataType::Utf8, None),
        ]);
        for changed in [
            schema(&[
                ("renamed", DataType::Float64, Some("rad")),
                ("frame", DataType::Utf8, None),
            ]),
            schema(&[
                ("frame", DataType::Utf8, None),
                ("roll", DataType::Float64, Some("rad")),
            ]),
            schema(&[
                ("roll", DataType::Utf8, Some("rad")),
                ("frame", DataType::Utf8, None),
            ]),
            schema(&[
                ("roll", DataType::Float64, Some("deg")),
                ("frame", DataType::Utf8, None),
            ]),
        ] {
            let mut registry = TopicRegistry::default();
            registry
                .claim_batch(4, &[("OUT".into(), Some(original.clone()))])
                .unwrap();
            assert!(
                registry
                    .claim_batch(4, &[("OUT".into(), Some(changed))])
                    .is_err()
            );
        }
    }

    #[test]
    fn topic_registry_rejects_cross_operation_schema_variants() {
        let original = schema(&[
            ("roll", DataType::Float64, Some("rad")),
            ("frame", DataType::Utf8, None),
        ]);
        let variants = [
            schema(&[
                ("renamed", DataType::Float64, Some("rad")),
                ("frame", DataType::Utf8, None),
            ]),
            schema(&[
                ("frame", DataType::Utf8, None),
                ("roll", DataType::Float64, Some("rad")),
            ]),
            schema(&[
                ("roll", DataType::Utf8, Some("rad")),
                ("frame", DataType::Utf8, None),
            ]),
            schema(&[
                ("roll", DataType::Float64, Some("deg")),
                ("frame", DataType::Utf8, None),
            ]),
        ];
        for variant in variants {
            let mut registry = TopicRegistry::default();
            registry
                .claim_batch(0, &[("OUT".into(), Some(original.clone()))])
                .unwrap();
            let error = registry
                .claim_batch(1, &[("OUT".into(), Some(variant))])
                .unwrap_err();
            assert!(error.contains("owned by operation 0"), "{error}");
        }
    }

    #[test]
    fn topic_registry_rolls_back_a_failed_multi_topic_claim() {
        let fields = schema(&[("value", DataType::Float64, None)]);
        let mut registry = TopicRegistry::default();
        registry
            .claim_batch(0, &[("TAKEN".into(), Some(fields.clone()))])
            .unwrap();
        assert!(
            registry
                .claim_batch(
                    1,
                    &[
                        ("FREE".into(), Some(fields.clone())),
                        ("TAKEN".into(), Some(fields.clone())),
                    ],
                )
                .is_err()
        );
        registry
            .claim_batch(2, &[("FREE".into(), Some(fields))])
            .unwrap();
    }
}

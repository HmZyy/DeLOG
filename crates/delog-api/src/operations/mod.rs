use std::collections::{HashMap, HashSet};

use arrow::datatypes::DataType;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationMode {
    Snapshot,
    Live,
    Both,
}

impl OperationMode {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("both") {
            "snapshot" => Ok(Self::Snapshot),
            "live" => Ok(Self::Live),
            "both" => Ok(Self::Both),
            value => Err(Error::invalid_input(format!(
                "mode must be 'snapshot', 'live', or 'both', got '{value}'"
            ))),
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

impl TransformSpec {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input: TopicSelector,
        multiplier: f64,
        offset: f64,
        fields: Option<Vec<String>>,
        unit: Option<String>,
        units: HashMap<String, String>,
        output_topic: Option<String>,
        mode: OperationMode,
    ) -> Result<Self> {
        validate_transform(multiplier, offset, unit.as_deref(), &units)?;
        if matches!(&fields, Some(fields) if fields.is_empty()) {
            return Err(Error::invalid_input("transform fields must not be empty"));
        }
        if output_topic.as_deref() == Some("") {
            return Err(Error::invalid_input(
                "transform output_topic must not be empty",
            ));
        }
        let output_topic = output_topic.unwrap_or_else(|| input.topic.clone());
        Ok(Self {
            input,
            multiplier,
            offset,
            fields,
            unit,
            units,
            output_topic,
            mode,
        })
    }
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

impl MergeSpec {
    pub fn new(
        topics: Vec<(String, Vec<String>)>,
        base_topic: String,
        output_topic: String,
        source: Option<String>,
        mode: OperationMode,
    ) -> Result<Self> {
        if topics.is_empty() {
            return Err(Error::invalid_input("merge topics must not be empty"));
        }
        for (topic, fields) in &topics {
            if fields.is_empty() {
                return Err(Error::invalid_input(format!(
                    "merge topic '{topic}' fields must not be empty"
                )));
            }
        }
        if !topics.iter().any(|(topic, _)| topic == &base_topic) {
            return Err(Error::invalid_input(format!(
                "merge base_topic '{base_topic}' must be present in topics"
            )));
        }
        let borrowed = topics
            .iter()
            .map(|(topic, fields)| {
                (
                    topic.as_str(),
                    fields.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let flat_names = merged_field_names(&borrowed)?;
        let mut names = flat_names.into_iter();
        let output_names = topics
            .iter()
            .map(|(_, fields)| names.by_ref().take(fields.len()).collect::<Vec<_>>())
            .collect();
        Ok(Self {
            topics,
            base_topic,
            output_topic,
            source,
            output_names,
            mode,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitBySpec {
    pub input: TopicSelector,
    pub field: String,
    pub fields: Option<Vec<String>>,
    pub output_template: String,
    pub mode: OperationMode,
}

impl SplitBySpec {
    pub fn new(
        input: TopicSelector,
        field: String,
        fields: Option<Vec<String>>,
        output_topic: Option<String>,
        mode: OperationMode,
    ) -> Result<Self> {
        if matches!(&fields, Some(fields) if fields.is_empty()) {
            return Err(Error::invalid_input("split_by fields must not be empty"));
        }
        let output_template = output_topic.unwrap_or_else(|| "{topic}/{value}".to_owned());
        validate_split_template(&output_template)?;
        Ok(Self {
            input,
            field,
            fields,
            output_template,
            mode,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OperationSpec {
    Transform(TransformSpec),
    Merge(MergeSpec),
    SplitBy(SplitBySpec),
}

pub type OutputSchema = Vec<(String, DataType, Option<String>)>;

#[derive(Debug, Clone, PartialEq)]
struct TopicClaim {
    operation: usize,
    schema: Option<OutputSchema>,
}

#[derive(Debug, Clone, Default)]
pub struct TopicRegistry {
    claims: HashMap<String, TopicClaim>,
}

impl TopicRegistry {
    pub fn preclaim_static(&mut self, specs: &[OperationSpec]) -> Result<()> {
        for (operation, spec) in specs.iter().enumerate() {
            let topic = match spec {
                OperationSpec::Transform(spec) => Some(spec.output_topic.as_str()),
                OperationSpec::Merge(spec) => Some(spec.output_topic.as_str()),
                OperationSpec::SplitBy(_) => None,
            };
            if let Some(topic) = topic {
                self.claim_batch(operation, &[(topic.to_owned(), None)])?;
            }
        }
        Ok(())
    }

    pub fn claim_batch(
        &mut self,
        operation: usize,
        claims: &[(String, Option<OutputSchema>)],
    ) -> Result<()> {
        let mut staged = HashMap::<&str, &Option<OutputSchema>>::new();
        for (topic, schema) in claims {
            if let Some(previous) = staged.insert(topic, schema)
                && previous != schema
            {
                return Err(Error::invalid_input(format!(
                    "output topic '{topic}' has conflicting schemas in operation {operation}"
                )));
            }
            if let Some(existing) = self.claims.get(topic) {
                if existing.operation != operation {
                    return Err(Error::invalid_input(format!(
                        "output topic '{topic}' is owned by operation {}; operation {operation} cannot also produce it",
                        existing.operation
                    )));
                }
                if let (Some(existing), Some(schema)) = (&existing.schema, schema)
                    && existing != schema
                {
                    return Err(Error::invalid_input(format!(
                        "output topic '{topic}' schema changed from {existing:?} to {schema:?}"
                    )));
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

fn validate_transform(
    multiplier: f64,
    offset: f64,
    unit: Option<&str>,
    units: &HashMap<String, String>,
) -> Result<()> {
    if !multiplier.is_finite() {
        return Err(Error::invalid_input("transform multiplier must be finite"));
    }
    if !offset.is_finite() {
        return Err(Error::invalid_input("transform offset must be finite"));
    }
    if unit.is_some() && !units.is_empty() {
        return Err(Error::invalid_input(
            "transform unit and units are mutually exclusive",
        ));
    }
    Ok(())
}

fn validate_split_template(template: &str) -> Result<()> {
    if !template.contains("{value}") {
        return Err(Error::invalid_input(
            "split_by output_topic must contain '{value}'",
        ));
    }
    Ok(())
}

pub fn merged_field_names(topics: &[(&str, Vec<&str>)]) -> Result<Vec<String>> {
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
                return Err(Error::invalid_input(format!(
                    "merge output field name '{name}' is duplicated"
                )));
            }
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_mode_defaults_and_preserves_unknown_mode_errors() {
        assert_eq!(OperationMode::parse(None).unwrap(), OperationMode::Both);
        assert_eq!(
            OperationMode::parse(Some("stream"))
                .unwrap_err()
                .to_string(),
            "mode must be 'snapshot', 'live', or 'both', got 'stream'"
        );
    }

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
    fn split_template_requires_value() {
        let input = TopicSelector {
            topic: "ATT".into(),
            source: None,
            instance: None,
        };
        assert_eq!(
            SplitBySpec::new(
                input.clone(),
                "instance".into(),
                None,
                Some("{topic}/fixed".into()),
                OperationMode::Both,
            )
            .unwrap_err()
            .to_string(),
            "split_by output_topic must contain '{value}'"
        );
        assert_eq!(
            SplitBySpec::new(input, "instance".into(), None, None, OperationMode::Both)
                .unwrap()
                .output_template,
            "{topic}/{value}"
        );
    }

    #[test]
    fn transform_requires_finite_operands_and_exclusive_units() {
        let input = TopicSelector {
            topic: "ATT".into(),
            source: None,
            instance: None,
        };
        assert!(
            TransformSpec::new(
                input.clone(),
                f64::NAN,
                0.0,
                None,
                None,
                HashMap::new(),
                None,
                OperationMode::Both,
            )
            .is_err()
        );
        assert!(
            TransformSpec::new(
                input,
                1.0,
                0.0,
                None,
                Some("deg".into()),
                HashMap::from([("roll".into(), "deg".into())]),
                None,
                OperationMode::Both,
            )
            .is_err()
        );
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
            assert!(
                error.to_string().contains("owned by operation 0"),
                "{error}"
            );
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

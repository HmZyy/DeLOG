use std::collections::{HashMap, HashSet};

use arrow::datatypes::{DataType, Field, Schema};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FORMAT_KEY: &str = "delog.format";
pub const VERSION_KEY: &str = "delog.version";
pub const MANIFEST_KEY: &str = "delog.manifest";
pub const FORMAT_NAME: &str = "multi-topic";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub topics: Vec<TopicManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicManifest {
    pub id: u32,
    pub original_source: String,
    pub original_topic: String,
    pub timestamp_column: u32,
    pub fields: Vec<FieldManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldManifest {
    pub column: u32,
    pub name: String,
    pub unit: Option<String>,
    pub multiplier: f64,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedManifest {
    pub version: u32,
    pub topics: Vec<ValidatedTopic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedTopic {
    pub id: u32,
    pub original_source: String,
    pub original_topic: String,
    pub timestamp_column: usize,
    pub fields: Vec<ValidatedField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedField {
    pub column: usize,
    pub name: String,
    pub unit: Option<String>,
    pub multiplier: f64,
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("invalid manifest JSON: {0}")]
    InvalidManifest(#[from] serde_json::Error),
    #[error("missing required metadata key `{0}`")]
    MissingMetadata(&'static str),
    #[error("unexpected format marker `{0}`")]
    InvalidFormat(String),
    #[error("invalid format version `{0}`")]
    InvalidVersion(String),
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u32),
    #[error("metadata version {metadata} does not match manifest version {manifest}")]
    InconsistentVersion { metadata: u32, manifest: u32 },
    #[error("field metadata conflicts with reserved schema key `{0}`")]
    MetadataConflict(&'static str),
    #[error("invalid structured Parquet schema: {0}")]
    InvalidSchema(String),
}

pub fn encode_schema(fields: Vec<Field>, manifest: &Manifest) -> Result<Schema, FormatError> {
    reject_field_metadata_conflicts(&fields)?;
    validate(fields.as_slice(), manifest)?;

    let mut metadata = HashMap::new();
    metadata.insert(FORMAT_KEY.to_owned(), FORMAT_NAME.to_owned());
    metadata.insert(VERSION_KEY.to_owned(), FORMAT_VERSION.to_string());
    metadata.insert(MANIFEST_KEY.to_owned(), serde_json::to_string(manifest)?);
    Ok(Schema::new_with_metadata(fields, metadata))
}

pub fn decode_schema(schema: &Schema) -> Result<Option<ValidatedManifest>, FormatError> {
    let metadata = schema.metadata();
    let Some(format) = metadata.get(FORMAT_KEY) else {
        return Ok(None);
    };
    if format != FORMAT_NAME {
        return Err(FormatError::InvalidFormat(format.clone()));
    }

    let version = metadata
        .get(VERSION_KEY)
        .ok_or(FormatError::MissingMetadata(VERSION_KEY))?
        .parse::<u32>()
        .map_err(|_| {
            FormatError::InvalidVersion(
                metadata
                    .get(VERSION_KEY)
                    .expect("checked metadata version exists")
                    .clone(),
            )
        })?;
    if version != FORMAT_VERSION {
        return Err(FormatError::UnsupportedVersion(version));
    }

    let manifest: Manifest = serde_json::from_str(
        metadata
            .get(MANIFEST_KEY)
            .ok_or(FormatError::MissingMetadata(MANIFEST_KEY))?,
    )?;
    if manifest.version != version {
        return Err(FormatError::InconsistentVersion {
            metadata: version,
            manifest: manifest.version,
        });
    }

    let fields = schema
        .fields()
        .iter()
        .map(|field| field.as_ref().clone())
        .collect::<Vec<_>>();
    validate(&fields, &manifest).map(Some)
}

pub fn resolved_topic_names(manifest: &ValidatedManifest) -> Vec<String> {
    let mut counts = HashMap::<&str, usize>::new();
    for topic in &manifest.topics {
        *counts.entry(&topic.original_topic).or_default() += 1;
    }

    let mut reserved = counts
        .iter()
        .filter(|(_, count)| **count == 1)
        .map(|(name, _)| (*name).to_owned())
        .collect::<HashSet<_>>();
    let mut occurrences = HashMap::<&str, usize>::new();
    manifest
        .topics
        .iter()
        .map(|topic| {
            if counts[topic.original_topic.as_str()] == 1 {
                topic.original_topic.clone()
            } else {
                let occurrence = occurrences.entry(&topic.original_topic).or_default();
                loop {
                    let resolved = format!("{}[{occurrence}]", topic.original_topic);
                    *occurrence += 1;
                    if reserved.insert(resolved.clone()) {
                        break resolved;
                    }
                }
            }
        })
        .collect()
}

fn reject_field_metadata_conflicts(fields: &[Field]) -> Result<(), FormatError> {
    for field in fields {
        for key in [FORMAT_KEY, VERSION_KEY, MANIFEST_KEY] {
            if field.metadata().contains_key(key) {
                return Err(FormatError::MetadataConflict(key));
            }
        }
    }
    Ok(())
}

fn validate(fields: &[Field], manifest: &Manifest) -> Result<ValidatedManifest, FormatError> {
    if manifest.version != FORMAT_VERSION {
        return Err(FormatError::UnsupportedVersion(manifest.version));
    }

    let mut topic_ids = HashSet::new();
    let mut referenced_columns = vec![false; fields.len()];
    let mut topics = Vec::with_capacity(manifest.topics.len());

    for topic in &manifest.topics {
        if !topic_ids.insert(topic.id) {
            return Err(invalid("duplicate topic ID"));
        }
        if topic.original_source.is_empty() {
            return Err(invalid("topic original source must not be empty"));
        }
        if topic.original_topic.is_empty() {
            return Err(invalid("topic original name must not be empty"));
        }

        let timestamp_column = reserve_column(
            &mut referenced_columns,
            topic.timestamp_column,
            "timestamp column",
        )?;
        let timestamp = &fields[timestamp_column];
        if !timestamp.is_nullable() || timestamp.data_type() != &DataType::Int64 {
            return Err(invalid("timestamp columns must be nullable Int64 fields"));
        }

        let mut names = HashSet::new();
        let mut validated_fields = Vec::with_capacity(topic.fields.len());
        for field in &topic.fields {
            if field.name.is_empty() {
                return Err(invalid("topic field names must not be empty"));
            }
            if !names.insert(&field.name) {
                return Err(invalid("duplicate topic field name"));
            }
            if !field.multiplier.is_finite() {
                return Err(invalid("field multiplier must be finite"));
            }

            let column = reserve_column(&mut referenced_columns, field.column, "field column")?;
            let physical = &fields[column];
            if !physical.is_nullable() {
                return Err(invalid("value columns must be nullable"));
            }
            if !is_supported_value_type(physical.data_type()) {
                return Err(invalid("value column has an unsupported Arrow type"));
            }

            validated_fields.push(ValidatedField {
                column,
                name: field.name.clone(),
                unit: normalize_optional(&field.unit),
                multiplier: field.multiplier,
                description: normalize_optional(&field.description),
            });
        }

        topics.push(ValidatedTopic {
            id: topic.id,
            original_source: topic.original_source.clone(),
            original_topic: topic.original_topic.clone(),
            timestamp_column,
            fields: validated_fields,
        });
    }

    if referenced_columns.iter().any(|referenced| !referenced) {
        return Err(invalid(
            "every physical column must be referenced exactly once",
        ));
    }

    Ok(ValidatedManifest {
        version: manifest.version,
        topics,
    })
}

fn reserve_column(
    referenced_columns: &mut [bool],
    column: u32,
    description: &str,
) -> Result<usize, FormatError> {
    let column = usize::try_from(column).map_err(|_| {
        FormatError::InvalidSchema(format!("{description} index does not fit this platform"))
    })?;
    let Some(referenced) = referenced_columns.get_mut(column) else {
        return Err(invalid(format!("{description} index is out of bounds")));
    };
    if *referenced {
        return Err(invalid(format!("duplicate {description} reference")));
    }
    *referenced = true;
    Ok(column)
}

fn normalize_optional(value: &Option<String>) -> Option<String> {
    value.clone().filter(|value| !value.is_empty())
}

fn is_supported_value_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Boolean
            | DataType::Utf8
            | DataType::LargeUtf8
    )
}

fn invalid(message: impl Into<String>) -> FormatError {
    FormatError::InvalidSchema(message.into())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

    use super::*;

    fn fields() -> Vec<Field> {
        vec![
            Field::new("__delog_t0_time", DataType::Int64, true),
            Field::new("__delog_t0_f0", DataType::Float32, true),
        ]
    }

    fn manifest() -> Manifest {
        Manifest {
            version: FORMAT_VERSION,
            topics: vec![TopicManifest {
                id: 0,
                original_source: "flight-a".into(),
                original_topic: "ATT".into(),
                timestamp_column: 0,
                fields: vec![FieldManifest {
                    column: 1,
                    name: "Roll".into(),
                    unit: Some("rad".into()),
                    multiplier: 1.0,
                    description: Some("roll angle".into()),
                }],
            }],
        }
    }

    fn validated_topics(names: &[&str]) -> ValidatedManifest {
        ValidatedManifest {
            version: FORMAT_VERSION,
            topics: names
                .iter()
                .enumerate()
                .map(|(index, name)| ValidatedTopic {
                    id: index as u32,
                    original_source: format!("source-{index}"),
                    original_topic: (*name).into(),
                    timestamp_column: index,
                    fields: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn manifest_round_trips_and_validates_against_schema() {
        let schema = encode_schema(fields(), &manifest()).unwrap();
        let decoded = decode_schema(&schema).unwrap().unwrap();
        assert_eq!(decoded.topics[0].original_topic, "ATT");
        assert_eq!(decoded.topics[0].fields[0].name, "Roll");
    }

    #[test]
    fn absent_marker_is_generic_and_marked_corruption_is_an_error() {
        let generic = Schema::new(vec![Field::new("time", DataType::Int64, false)]);
        assert!(decode_schema(&generic).unwrap().is_none());

        let mut metadata = HashMap::new();
        metadata.insert(FORMAT_KEY.into(), FORMAT_NAME.into());
        metadata.insert(VERSION_KEY.into(), FORMAT_VERSION.to_string());
        metadata.insert(MANIFEST_KEY.into(), "{broken".into());
        let marked = Schema::new_with_metadata(generic.fields().to_vec(), metadata);
        assert!(matches!(
            decode_schema(&marked),
            Err(FormatError::InvalidManifest(_))
        ));
    }

    #[test]
    fn validation_rejects_invalid_versions_references_and_types() {
        struct Case {
            name: &'static str,
            mutate: fn(&mut Vec<Field>, &mut Manifest),
        }

        let cases = [
            Case {
                name: "unsupported version",
                mutate: |_, manifest| manifest.version = FORMAT_VERSION + 1,
            },
            Case {
                name: "duplicate topic ids",
                mutate: |_, manifest| manifest.topics.push(manifest.topics[0].clone()),
            },
            Case {
                name: "duplicate column references",
                mutate: |_, manifest| manifest.topics[0].fields[0].column = 0,
            },
            Case {
                name: "unreferenced columns",
                mutate: |fields, _| fields.push(Field::new("extra", DataType::Boolean, true)),
            },
            Case {
                name: "non-null timestamps",
                mutate: |fields, _| fields[0] = Field::new("time", DataType::Int64, false),
            },
            Case {
                name: "non-Int64 timestamps",
                mutate: |fields, _| fields[0] = Field::new("time", DataType::Float64, true),
            },
            Case {
                name: "empty source names",
                mutate: |_, manifest| manifest.topics[0].original_source.clear(),
            },
            Case {
                name: "empty topic names",
                mutate: |_, manifest| manifest.topics[0].original_topic.clear(),
            },
            Case {
                name: "empty field names",
                mutate: |_, manifest| manifest.topics[0].fields[0].name.clear(),
            },
            Case {
                name: "duplicate topic field names",
                mutate: |fields, manifest| {
                    fields.push(Field::new("pitch", DataType::Float32, true));
                    let mut duplicate = manifest.topics[0].fields[0].clone();
                    duplicate.column = 2;
                    manifest.topics[0].fields.push(duplicate);
                },
            },
            Case {
                name: "non-finite multipliers",
                mutate: |_, manifest| manifest.topics[0].fields[0].multiplier = f64::INFINITY,
            },
            Case {
                name: "unsupported Arrow types",
                mutate: |fields, _| {
                    fields[1] = Field::new(
                        "value",
                        DataType::Timestamp(TimeUnit::Microsecond, None),
                        true,
                    );
                },
            },
        ];

        for case in cases {
            let mut case_fields = fields();
            let mut case_manifest = manifest();
            (case.mutate)(&mut case_fields, &mut case_manifest);
            assert!(
                encode_schema(case_fields, &case_manifest).is_err(),
                "case {} should be rejected",
                case.name
            );
        }
    }

    #[test]
    fn empty_optional_text_is_normalized() {
        let mut manifest = manifest();
        manifest.topics[0].fields[0].unit = Some(String::new());
        manifest.topics[0].fields[0].description = Some(String::new());

        let schema = encode_schema(fields(), &manifest).unwrap();
        let decoded = decode_schema(&schema).unwrap().unwrap();
        assert_eq!(decoded.topics[0].fields[0].unit, None);
        assert_eq!(decoded.topics[0].fields[0].description, None);
    }

    #[test]
    fn resolved_topic_names_are_unique_and_stable() {
        let unique = ValidatedManifest {
            version: FORMAT_VERSION,
            topics: vec![
                ValidatedTopic {
                    id: 0,
                    original_source: "source-a".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 0,
                    fields: vec![],
                },
                ValidatedTopic {
                    id: 1,
                    original_source: "source-b".into(),
                    original_topic: "GPS".into(),
                    timestamp_column: 1,
                    fields: vec![],
                },
            ],
        };
        assert_eq!(resolved_topic_names(&unique), ["ATT", "GPS"]);

        let repeated = ValidatedManifest {
            version: FORMAT_VERSION,
            topics: vec![
                ValidatedTopic {
                    id: 2,
                    original_source: "source-a".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 0,
                    fields: vec![],
                },
                ValidatedTopic {
                    id: 5,
                    original_source: "source-b".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 1,
                    fields: vec![],
                },
                ValidatedTopic {
                    id: 1,
                    original_source: "source-c".into(),
                    original_topic: "GPS".into(),
                    timestamp_column: 2,
                    fields: vec![],
                },
                ValidatedTopic {
                    id: 0,
                    original_source: "source-d".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 3,
                    fields: vec![],
                },
            ],
        };
        assert_eq!(
            resolved_topic_names(&repeated),
            ["ATT[0]", "ATT[1]", "GPS", "ATT[2]"]
        );
    }

    #[test]
    fn duplicate_topic_names_get_deterministic_instances() {
        let manifest = validated_topics(&["ATT", "ATT"]);

        assert_eq!(resolved_topic_names(&manifest), ["ATT[0]", "ATT[1]"]);
    }

    #[test]
    fn unique_original_name_is_reserved_before_duplicate_instances() {
        let manifest = validated_topics(&["ATT", "ATT", "ATT[0]"]);

        assert_eq!(
            resolved_topic_names(&manifest),
            ["ATT[1]", "ATT[2]", "ATT[0]"]
        );
    }

    #[test]
    fn nested_duplicate_groups_share_one_global_reservation_set() {
        let manifest = validated_topics(&["ATT", "ATT", "ATT[0]", "ATT[0]", "ATT[0][0]"]);

        let resolved = resolved_topic_names(&manifest);

        assert_eq!(
            resolved,
            ["ATT[0]", "ATT[1]", "ATT[0][1]", "ATT[0][2]", "ATT[0][0]"]
        );
        assert_eq!(
            resolved.iter().collect::<HashSet<_>>().len(),
            resolved.len()
        );
    }
}

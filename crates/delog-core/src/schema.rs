//! Topic and field schema metadata (PLAN.md §4.3).
//!
//! The canonical store preserves original Arrow-compatible dtypes. Units and
//! multipliers stay in metadata and are applied only when a render cache is
//! built.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use arrow::datatypes::DataType;

/// One data field within a topic schema.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSchema {
    pub name: String,
    pub dtype: DataType,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub multiplier: f64,
}

/// Ordered field schema for one topic.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicSchema {
    name: String,
    fields: Vec<FieldSchema>,
    field_indices: HashMap<String, usize>,
}

/// Schema validation failures.
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaError {
    EmptyTopicName,
    EmptyFieldName,
    DuplicateFieldName(String),
    UnsupportedDataType(DataType),
    NonFiniteMultiplier { field: String, multiplier: f64 },
}

impl FieldSchema {
    pub fn new(
        name: impl Into<String>,
        dtype: DataType,
        unit: Option<impl Into<String>>,
        multiplier: f64,
    ) -> Result<Self, SchemaError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SchemaError::EmptyFieldName);
        }
        if !is_supported_dtype(&dtype) {
            return Err(SchemaError::UnsupportedDataType(dtype));
        }
        if !multiplier.is_finite() {
            return Err(SchemaError::NonFiniteMultiplier {
                field: name,
                multiplier,
            });
        }

        Ok(Self {
            name,
            dtype,
            unit: unit
                .map(Into::into)
                .and_then(|u: String| if u.is_empty() { None } else { Some(u) }),
            description: None,
            multiplier,
        })
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = match description.into() {
            description if description.is_empty() => None,
            description => Some(description),
        };
        self
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self.dtype,
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
        )
    }

    pub fn is_string(&self) -> bool {
        matches!(self.dtype, DataType::Utf8 | DataType::LargeUtf8)
    }

    pub fn is_plottable(&self) -> bool {
        self.is_numeric() || self.dtype == DataType::Boolean
    }

    /// Discrete enough to enumerate distinct values from (int/uint/bool/string;
    /// floats excluded) — gates "Generate markers" (ANA-11). Lets `delog-app`
    /// decide without touching Arrow types directly (§3.2).
    pub fn is_discrete(&self) -> bool {
        matches!(
            self.dtype,
            DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::UInt8
                | DataType::UInt16
                | DataType::UInt32
                | DataType::UInt64
                | DataType::Boolean
                | DataType::Utf8
                | DataType::LargeUtf8
        )
    }

    /// Short, fixed display tag for this field's dtype (e.g. `"i32"`, `"f64"`,
    /// `"str"`). Lets `delog-app` show a dtype chip without touching Arrow
    /// types directly (§3.2).
    pub fn dtype_label(&self) -> &'static str {
        match self.dtype {
            DataType::Int8 => "i8",
            DataType::Int16 => "i16",
            DataType::Int32 => "i32",
            DataType::Int64 => "i64",
            DataType::UInt8 => "u8",
            DataType::UInt16 => "u16",
            DataType::UInt32 => "u32",
            DataType::UInt64 => "u64",
            DataType::Float32 => "f32",
            DataType::Float64 => "f64",
            DataType::Boolean => "bool",
            DataType::Utf8 | DataType::LargeUtf8 => "str",
            _ => "?",
        }
    }
}

impl TopicSchema {
    pub fn new(
        name: impl Into<String>,
        fields: impl IntoIterator<Item = FieldSchema>,
    ) -> Result<Self, SchemaError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SchemaError::EmptyTopicName);
        }

        let fields: Vec<_> = fields.into_iter().collect();
        let mut field_indices = HashMap::with_capacity(fields.len());
        for (idx, field) in fields.iter().enumerate() {
            if field_indices.insert(field.name.clone(), idx).is_some() {
                return Err(SchemaError::DuplicateFieldName(field.name.clone()));
            }
        }

        Ok(Self {
            name,
            fields,
            field_indices,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fields(&self) -> &[FieldSchema] {
        &self.fields
    }

    pub fn field(&self, index: usize) -> Option<&FieldSchema> {
        self.fields.get(index)
    }

    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.field_indices.get(name).copied()
    }

    pub fn field_by_name(&self, name: &str) -> Option<&FieldSchema> {
        self.field(self.field_index(name)?)
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTopicName => write!(f, "topic name must not be empty"),
            Self::EmptyFieldName => write!(f, "field name must not be empty"),
            Self::DuplicateFieldName(name) => write!(f, "duplicate field name `{name}`"),
            Self::UnsupportedDataType(dtype) => write!(f, "unsupported field dtype {dtype:?}"),
            Self::NonFiniteMultiplier { field, multiplier } => {
                write!(f, "field `{field}` has non-finite multiplier {multiplier}")
            }
        }
    }
}

impl Error for SchemaError {}

fn is_supported_dtype(dtype: &DataType) -> bool {
    matches!(
        dtype,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, dtype: DataType) -> FieldSchema {
        FieldSchema::new(name, dtype, None::<String>, 1.0).unwrap()
    }

    #[test]
    fn topic_schema_preserves_field_order_and_metadata() {
        let alt = FieldSchema::new("Alt", DataType::Int32, Some("cm"), 0.01).unwrap();
        let temp = FieldSchema::new("Temp", DataType::Float32, Some("C"), 1.0).unwrap();
        let schema = TopicSchema::new("BARO", [alt.clone(), temp.clone()]).unwrap();

        assert_eq!(schema.name(), "BARO");
        assert_eq!(schema.fields(), &[alt, temp]);
        assert_eq!(schema.field_index("Alt"), Some(0));
        assert_eq!(schema.field_index("Temp"), Some(1));
        assert_eq!(
            schema.field_by_name("Alt").unwrap().unit.as_deref(),
            Some("cm")
        );
        assert_eq!(schema.field_by_name("Alt").unwrap().multiplier, 0.01);
    }

    #[test]
    fn duplicate_field_names_are_rejected() {
        let err = TopicSchema::new(
            "GPS",
            [field("Lat", DataType::Int32), field("Lat", DataType::Int32)],
        )
        .unwrap_err();

        assert_eq!(err, SchemaError::DuplicateFieldName("Lat".to_owned()));
    }

    #[test]
    fn unsupported_types_are_rejected() {
        let err = FieldSchema::new("When", DataType::Date64, None::<String>, 1.0).unwrap_err();
        assert_eq!(err, SchemaError::UnsupportedDataType(DataType::Date64));
    }

    #[test]
    fn non_finite_multipliers_are_rejected() {
        let err = FieldSchema::new("Alt", DataType::Float64, Some("m"), f64::NAN).unwrap_err();
        assert!(matches!(err, SchemaError::NonFiniteMultiplier { .. }));
    }

    #[test]
    fn empty_unit_is_normalized_to_none() {
        let f = FieldSchema::new("Alt", DataType::Float64, Some(""), 1.0).unwrap();
        assert_eq!(f.unit, None);
    }

    #[test]
    fn field_description_is_optional_and_builder_preserves_other_metadata() {
        let field = FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap();
        assert_eq!(field.description, None);

        let field = field.with_description("filtered altitude");
        assert_eq!(field.description.as_deref(), Some("filtered altitude"));
        assert_eq!(field.unit.as_deref(), Some("m"));

        assert_eq!(field.with_description("").description, None);
    }

    #[test]
    fn strings_are_supported_but_not_plottable() {
        let msg = field("Message", DataType::Utf8);
        let armed = field("Armed", DataType::Boolean);
        let alt = field("Alt", DataType::Float32);

        assert!(msg.is_string());
        assert!(!msg.is_plottable());
        assert!(armed.is_plottable());
        assert!(alt.is_plottable());
    }
}

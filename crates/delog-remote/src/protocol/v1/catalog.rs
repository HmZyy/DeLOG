use arrow::datatypes::DataType;
use delog_core::identity::SourceKind;
use serde::{Deserialize, Serialize};

use crate::handles::OpaqueId;
use crate::protocol::v1::error::ApiError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogDto {
    pub snapshot_epoch: u64,
    pub sources: Vec<SourceDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDto {
    pub handle: OpaqueId,
    pub label: String,
    pub kind: SourceKindDto,
    pub offset_ns: i64,
    pub topics: Vec<TopicDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicDto {
    pub handle: OpaqueId,
    pub name: String,
    pub base_name: String,
    pub instance: Option<u32>,
    pub row_count: u64,
    pub time_range_ns: Option<TimeRangeDto>,
    pub fields: Vec<FieldDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDto {
    pub handle: OpaqueId,
    pub name: String,
    pub selector: FieldSelectorDto,
    pub arrow_type: FieldTypeDto,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub multiplier: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSelectorDto {
    pub source: String,
    pub topic: String,
    pub instance: Option<u32>,
    pub field: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindDto {
    File,
    Live,
    Derived,
    LiveDerived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldTypeDto {
    Int8,
    Int16,
    Int32,
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Float32,
    Float64,
    Boolean,
    Utf8,
    LargeUtf8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRangeDto {
    pub start_ns: i64,
    pub end_ns: i64,
}

impl From<SourceKind> for SourceKindDto {
    fn from(kind: SourceKind) -> Self {
        match kind {
            SourceKind::File => Self::File,
            SourceKind::Live => Self::Live,
            SourceKind::Derived => Self::Derived,
            SourceKind::LiveDerived => Self::LiveDerived,
        }
    }
}

impl FieldTypeDto {
    pub fn from_arrow(dtype: &DataType) -> Result<Self, ApiError> {
        Ok(match dtype {
            DataType::Int8 => Self::Int8,
            DataType::Int16 => Self::Int16,
            DataType::Int32 => Self::Int32,
            DataType::Int64 => Self::Int64,
            DataType::UInt8 => Self::Uint8,
            DataType::UInt16 => Self::Uint16,
            DataType::UInt32 => Self::Uint32,
            DataType::UInt64 => Self::Uint64,
            DataType::Float32 => Self::Float32,
            DataType::Float64 => Self::Float64,
            DataType::Boolean => Self::Boolean,
            DataType::Utf8 => Self::Utf8,
            DataType::LargeUtf8 => Self::LargeUtf8,
            other => {
                return Err(ApiError::internal(format!(
                    "unsupported field wire dtype {other:?}"
                )));
            }
        })
    }
}

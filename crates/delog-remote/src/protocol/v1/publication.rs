use serde::{Deserialize, Serialize};

use crate::handles::OpaqueId;
use crate::protocol::v1::catalog::FieldTypeDto;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationDto {
    pub handle: OpaqueId,
    pub generation: u64,
    pub topic: PublicationTopicDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationTopicDto {
    pub handle: OpaqueId,
    pub name: String,
    pub row_count: u64,
    pub fields: Vec<PublicationFieldDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationFieldDto {
    pub handle: OpaqueId,
    pub name: String,
    pub arrow_type: FieldTypeDto,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub multiplier: f64,
}

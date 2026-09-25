use serde::{Deserialize, Serialize};

use crate::handles::OpaqueId;
use crate::protocol::v1::{API_MAJOR, API_MAX_MINOR, API_MIN_MINOR};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDto {
    pub instance_id: OpaqueId,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_description: Option<String>,
    pub api_major: u16,
    pub api_min_minor: u16,
    pub api_max_minor: u16,
}

impl InstanceDto {
    pub fn current(
        instance_id: OpaqueId,
        label: impl Into<String>,
        session_description: Option<String>,
    ) -> Self {
        Self {
            instance_id,
            label: label.into(),
            session_description,
            api_major: API_MAJOR,
            api_min_minor: API_MIN_MINOR,
            api_max_minor: API_MAX_MINOR,
        }
    }
}

use serde::{Deserialize, Serialize};

use crate::handles::RequestId;
use crate::protocol::v1::error::ErrorEnvelope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStateDto {
    InFlight,
    Completed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestStatusDto {
    pub request_id: RequestId,
    pub state: RequestStateDto,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub response: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<ErrorEnvelope>,
}

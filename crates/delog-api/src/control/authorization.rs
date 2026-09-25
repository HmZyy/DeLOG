use std::sync::Arc;
use std::time::Duration;

use super::{ControlRequest, ControlResponse, ResourceOwner};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Safe,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPrincipal {
    pub owner: ResourceOwner,
    pub access: AccessMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlCall {
    Trusted(ControlRequest),
    External {
        principal: ControlPrincipal,
        request: ControlRequest,
    },
}

impl ControlCall {
    pub fn request(&self) -> &ControlRequest {
        match self {
            Self::Trusted(request) | Self::External { request, .. } => request,
        }
    }
}

pub type CancelCheck = Arc<dyn Fn() -> bool + Send + Sync>;

pub trait AuthorizedControlHost: Send + Sync {
    fn call_as(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
    ) -> Result<ControlResponse>;

    fn call_as_within(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
        _timeout: Duration,
    ) -> Result<ControlResponse> {
        self.call_as(principal, request)
    }

    fn call_as_cancellable(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
        timeout: Duration,
        _cancelled: CancelCheck,
    ) -> Result<ControlResponse> {
        self.call_as_within(principal, request, timeout)
    }
}

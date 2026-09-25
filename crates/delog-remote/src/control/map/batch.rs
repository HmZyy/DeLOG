use std::sync::Mutex;

use delog_api::control::{
    AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse, request_is_batchable,
};

use super::ControlMapper;
use crate::protocol::v1::control::{ControlCommandDto, ControlResultDto};
use crate::protocol::v1::error::ApiError;

#[derive(Default)]
struct CaptureHost(Mutex<Vec<ControlRequest>>);

impl AuthorizedControlHost for CaptureHost {
    fn call_as(
        &self,
        _principal: ControlPrincipal,
        request: ControlRequest,
    ) -> delog_api::Result<ControlResponse> {
        self.0.lock().expect("batch capture poisoned").push(request);
        Ok(ControlResponse::Unit)
    }
}

fn returns_value(command: &ControlCommandDto) -> bool {
    matches!(
        command,
        ControlCommandDto::WindowOpen { .. }
            | ControlCommandDto::WorkspaceAddPlot { .. }
            | ControlCommandDto::WorkspaceSplit { .. }
            | ControlCommandDto::TraceAdd { .. }
            | ControlCommandDto::AnnotationAdd { .. }
            | ControlCommandDto::MarkerAdd { .. }
            | ControlCommandDto::VehicleAdd { .. }
            | ControlCommandDto::VehicleSet { .. }
            | ControlCommandDto::VehicleProfileList
            | ControlCommandDto::VehicleProfileLoad { .. }
            | ControlCommandDto::VehicleProfileApply { .. }
            | ControlCommandDto::LayoutList
            | ControlCommandDto::LayoutCurrent
            | ControlCommandDto::RemoveOwned
    )
}

impl ControlMapper<'_> {
    pub fn execute_batch(
        &mut self,
        commands: Vec<ControlCommandDto>,
    ) -> Result<ControlResultDto, ApiError> {
        if commands.is_empty() {
            return Err(ApiError::invalid_input(
                "a batch must contain at least one command",
            ));
        }
        let capture = CaptureHost::default();
        let mut shadow = self.handles.clone();
        let mut requests = Vec::with_capacity(commands.len());
        for (index, command) in commands.into_iter().enumerate() {
            if returns_value(&command) {
                return Err(ApiError::invalid_input(format!(
                    "batch command {index} returns a result and cannot be batched"
                )));
            }
            let result =
                ControlMapper::new(&capture, self.principal.clone(), &mut shadow, self.fields)
                    .execute(command)?;
            let mut captured =
                std::mem::take(&mut *capture.0.lock().expect("batch capture poisoned"));
            let batchable = result == ControlResultDto::Unit
                && captured.len() == 1
                && request_is_batchable(&captured[0]);
            if !batchable {
                return Err(ApiError::invalid_input(format!(
                    "batch command {index} is not batchable"
                )));
            }
            requests.push(captured.remove(0));
        }
        self.call(ControlRequest::Batch(requests))?
            .into_unit()
            .map_err(ApiError::from)?;
        *self.handles = shadow;
        Ok(ControlResultDto::Unit)
    }
}

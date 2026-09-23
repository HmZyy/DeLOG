use delog_script::{ControlRequest, ControlResponse, PlotRequest};

use crate::plotting::markers::Markers;

pub struct AppControl<'a> {
    pub markers: &'a mut Markers,
    pub workspace: &'a mut crate::shell::workspace::Workspace,
}

pub fn apply(
    control: &mut AppControl<'_>,
    request: ControlRequest,
) -> Result<ControlResponse, String> {
    match request {
        ControlRequest::Markers(request) => {
            control.markers.apply_control_request(request);
            Ok(ControlResponse::Unit)
        }
        ControlRequest::Plots(PlotRequest::List) => {
            Ok(ControlResponse::Plots(control.workspace.plot_infos()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_script::{MarkerRequest, PendingMarker};

    fn marker(time_us: i64, label: &str) -> PendingMarker {
        PendingMarker {
            time_us,
            label: label.into(),
            color: None,
            note: String::new(),
        }
    }

    #[test]
    fn a_marker_replace_request_reaches_the_marker_store() {
        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
        };
        let response = apply(
            &mut control,
            ControlRequest::Markers(MarkerRequest::Replace {
                owner: "flight.py".into(),
                generation: 1,
                markers: vec![marker(10, "armed")],
            }),
        )
        .unwrap();
        assert_eq!(response, ControlResponse::Unit);
        assert_eq!(markers.as_slice().len(), 1);
        assert_eq!(markers.as_slice()[0].label, "armed");
    }

    #[test]
    fn a_rerun_replaces_only_its_own_owner() {
        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
        };
        for owner in ["flight.py", "other.py"] {
            apply(
                &mut control,
                ControlRequest::Markers(MarkerRequest::Replace {
                    owner: owner.into(),
                    generation: 1,
                    markers: vec![marker(10, owner)],
                }),
            )
            .unwrap();
        }
        apply(
            &mut control,
            ControlRequest::Markers(MarkerRequest::Replace {
                owner: "flight.py".into(),
                generation: 2,
                markers: vec![marker(20, "new")],
            }),
        )
        .unwrap();
        let labels: Vec<&str> = markers
            .as_slice()
            .iter()
            .map(|m| m.label.as_str())
            .collect();
        assert_eq!(labels, ["other.py", "new"]);
    }
}

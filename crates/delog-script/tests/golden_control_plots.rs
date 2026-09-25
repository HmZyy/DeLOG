#![cfg(feature = "python")]

use std::sync::Arc;

use delog_api::control::{ControlHost, ControlRequest, ControlResponse, PlotInfo, PlotRequest};

struct TwoPlots;

impl ControlHost for TwoPlots {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        match request {
            ControlRequest::Plots(PlotRequest::List { window: None }) => {
                Ok(ControlResponse::Plots(vec![
                    PlotInfo {
                        owner: None,
                        window: 0,
                        tile: 7,
                        instance_id: 1,
                        index: 0,
                        label: "Plot 1".into(),
                    },
                    PlotInfo {
                        owner: None,
                        window: 0,
                        tile: 9,
                        instance_id: 1,
                        index: 1,
                        label: "Plot 2".into(),
                    },
                ]))
            }
            other => Err(delog_api::Error::execution(format!(
                "unexpected request: {other:?}"
            ))),
        }
    }
}

#[test]
fn plots_are_listed_in_ui_order_with_their_labels() {
    let host: Arc<dyn ControlHost> = Arc::new(TwoPlots);
    let labels =
        delog_script::control::testing::eval_to_strings(host, "[p.label for p in delog.plots()]")
            .unwrap();
    assert_eq!(labels, ["Plot 1", "Plot 2"]);
}

#[test]
fn plots_raise_a_clear_error_when_no_window_is_attached() {
    let error = delog_script::control::testing::eval_without_host("delog.plots()").unwrap_err();
    assert!(error.contains("not available"), "{error}");
}

struct TwoWindows;

impl ControlHost for TwoWindows {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        match request {
            ControlRequest::Plots(PlotRequest::List { window }) => {
                assert_eq!(window, Some(3), "the window filter must reach the app");
                Ok(ControlResponse::Plots(vec![PlotInfo {
                    owner: None,
                    window: 3,
                    tile: 11,
                    instance_id: 1,
                    index: 0,
                    label: "Plot 1".into(),
                }]))
            }
            other => Err(delog_api::Error::execution(format!(
                "unexpected request: {other:?}"
            ))),
        }
    }
}

#[test]
fn a_window_filter_reaches_the_app_rather_than_being_applied_client_side() {
    let host: Arc<dyn ControlHost> = Arc::new(TwoWindows);
    let labels = delog_script::control::testing::eval_to_strings(
        host,
        "[p.label for p in delog.plots(window=3)]",
    )
    .unwrap();
    assert_eq!(labels, ["Plot 1"]);
}

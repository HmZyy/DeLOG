#![cfg(feature = "python")]

use std::sync::Arc;

use delog_script::{ControlHost, ControlRequest, ControlResponse, PlotInfo, PlotRequest};

struct TwoPlots;

impl ControlHost for TwoPlots {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String> {
        match request {
            ControlRequest::Plots(PlotRequest::List) => Ok(ControlResponse::Plots(vec![
                PlotInfo {
                    window: 0,
                    tile: 7,
                    index: 0,
                    label: "Plot 1".into(),
                },
                PlotInfo {
                    window: 0,
                    tile: 9,
                    index: 1,
                    label: "Plot 2".into(),
                },
            ])),
            other => Err(format!("unexpected request: {other:?}")),
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

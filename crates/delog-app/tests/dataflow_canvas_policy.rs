#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::{APP_MANIFEST, DATA_FLOW_DOCS, DATAFLOW_CANVAS, DATAFLOW_WINDOW};

#[test]
fn dataflow_canvas_uses_egui_graph_without_custom_edge_renderer() {
    assert!(APP_MANIFEST.contains("egui_graph"));
    for required in [
        "egui_graph::Graph",
        "egui_graph::node::Node",
        "egui_graph::edge::Edge",
        ".resize_behavior(egui_graph::ResizeBehavior::MaintainView)",
        ".animation_time(0.0)",
    ] {
        assert!(DATAFLOW_CANVAS.contains(required), "missing {required}");
    }
    assert!(!DATAFLOW_CANVAS.contains(".center_view("));
    for forbidden in [
        "CubicBezierShape",
        "paint_bezier",
        "input_screen_pos",
        "output_screen_pos",
    ] {
        assert!(
            !DATAFLOW_CANVAS.contains(forbidden),
            "custom renderer leaked: {forbidden}"
        );
    }
    assert!(DATA_FLOW_DOCS.contains("thin adapter over `egui_graph`"));
}

#[test]
fn dataflow_fit_is_an_empty_canvas_double_click_not_a_toolbar_button() {
    assert!(DATAFLOW_CANVAS.contains("response.response.double_clicked()"));
    assert!(DATAFLOW_CANVAS.contains("state.request_fit()"));
    assert!(!DATAFLOW_WINDOW.contains("Fit nodes to view"));
    assert!(!DATAFLOW_WINDOW.contains("crate::icons::maximize()"));
}

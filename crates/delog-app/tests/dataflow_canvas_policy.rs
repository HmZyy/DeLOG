const APP_MANIFEST: &str = include_str!("../Cargo.toml");
const CANVAS_SOURCE: &str = include_str!("../src/dataflow/canvas.rs");
const WINDOW_SOURCE: &str = include_str!("../src/dataflow/window.rs");
const DATA_FLOW_DOCS: &str = include_str!("../../../docs/data_flow.md");

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
        assert!(CANVAS_SOURCE.contains(required), "missing {required}");
    }
    assert!(!CANVAS_SOURCE.contains(".center_view("));
    for forbidden in [
        "CubicBezierShape",
        "paint_bezier",
        "input_screen_pos",
        "output_screen_pos",
    ] {
        assert!(
            !CANVAS_SOURCE.contains(forbidden),
            "custom renderer leaked: {forbidden}"
        );
    }
    assert!(DATA_FLOW_DOCS.contains("thin adapter over `egui_graph`"));
}

#[test]
fn dataflow_fit_is_an_empty_canvas_double_click_not_a_toolbar_button() {
    assert!(CANVAS_SOURCE.contains("response.response.double_clicked()"));
    assert!(CANVAS_SOURCE.contains("state.request_fit()"));
    assert!(!WINDOW_SOURCE.contains("Fit nodes to view"));
    assert!(!WINDOW_SOURCE.contains("crate::icons::maximize()"));
}

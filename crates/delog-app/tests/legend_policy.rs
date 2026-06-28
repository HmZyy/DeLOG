const LEGEND_SOURCE: &str = include_str!("../src/legend.rs");

#[test]
fn legend_panel_uses_bounded_vertical_scroll_area() {
    assert!(LEGEND_SOURCE.contains("egui::ScrollArea::vertical()"));
    assert!(LEGEND_SOURCE.contains("legend_content_max_size(bounds, &frame)"));
    assert!(LEGEND_SOURCE.contains(".max_width(content_max_size.x)"));
    assert!(LEGEND_SOURCE.contains(".max_height(content_max_size.y)"));
    assert!(LEGEND_SOURCE.contains("ui.set_max_size(content_max_size)"));
    assert!(LEGEND_SOURCE.contains(".truncate()"));
    assert!(LEGEND_SOURCE.contains("fn legend_trace_label_width"));
    assert!(LEGEND_SOURCE.contains("fn legend_label_width"));
    assert!(LEGEND_SOURCE.contains("ui.add_sized("));
    assert!(LEGEND_SOURCE.contains("egui::vec2(label_width"));
    assert!(LEGEND_SOURCE.contains("LEGEND_TEXT_FILTER_WIDTH"));
    assert!(LEGEND_SOURCE.contains(".min_scrolled_height("));
    assert!(LEGEND_SOURCE.contains("LEGEND_DELTA_RESERVE_WIDTH"));
}

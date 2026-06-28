const LEGEND_SOURCE: &str = include_str!("../src/legend.rs");

#[test]
fn legend_panel_uses_bounded_vertical_scroll_area() {
    assert!(LEGEND_SOURCE.contains("egui::ScrollArea::vertical()"));
    assert!(LEGEND_SOURCE.contains("legend_content_max_size(bounds, &frame)"));
    assert!(LEGEND_SOURCE.contains(".max_width(content_max_size.x)"));
    assert!(LEGEND_SOURCE.contains(".max_height(content_max_size.y)"));
    assert!(LEGEND_SOURCE.contains("ui.set_max_size(content_max_size)"));
    assert!(LEGEND_SOURCE.contains(".truncate()"));
}

const LEGEND_SOURCE: &str = include_str!("../src/legend.rs");
const WORKSPACE_SOURCE: &str = include_str!("../src/workspace.rs");
const SETTINGS_SOURCE: &str = include_str!("../src/settings.rs");

#[test]
fn legend_panel_uses_bounded_vertical_scroll_area() {
    assert!(LEGEND_SOURCE.contains("egui::ScrollArea::vertical()"));
    assert!(LEGEND_SOURCE.contains("legend_content_max_size(bounds, &frame)"));
    assert!(LEGEND_SOURCE.contains(".constrain_to(bounds)"));
    assert!(LEGEND_SOURCE.contains("ui.shrink_clip_rect(bounds)"));
    assert!(LEGEND_SOURCE.contains(".max_width(content_max_size.x)"));
    assert!(LEGEND_SOURCE.contains(".max_height(content_max_size.y)"));
    assert!(LEGEND_SOURCE.contains("ui.set_max_size(content_max_size)"));
    assert!(LEGEND_SOURCE.contains(".truncate()"));
    assert!(LEGEND_SOURCE.contains("fn legend_trace_row_widths"));
    assert!(LEGEND_SOURCE.contains("fn legend_ghost_label_width"));
    assert!(LEGEND_SOURCE.contains(".add_sized("));
    assert!(LEGEND_SOURCE.contains("egui::vec2("));
    assert!(LEGEND_SOURCE.contains("LEGEND_PREFERRED_TEXT_FILTER_WIDTH"));
    assert!(LEGEND_SOURCE.contains(".min_scrolled_height("));
    assert!(LEGEND_SOURCE.contains("LEGEND_PREFERRED_DELTA_WIDTH"));
    assert!(LEGEND_SOURCE.contains("LEGEND_MIN_TEXT_FILTER_WIDTH"));
    assert!(LEGEND_SOURCE.contains("widths.filter >= LEGEND_MIN_TEXT_FILTER_WIDTH"));
    assert!(LEGEND_SOURCE.contains("fn legend_can_show_color_picker"));
    assert!(LEGEND_SOURCE.contains("legend_can_show_color_picker("));
}

#[test]
fn legend_labels_hug_content_left_aligned() {
    // Labels must hug their content (capped at the row budget) instead of being
    // stretched to the full bounded width, so the legend stays compact and
    // left-aligned rather than spanning the whole plot.
    assert!(LEGEND_SOURCE.contains("allocate_ui_with_layout"));
    assert!(LEGEND_SOURCE.contains("egui::Layout::left_to_right(egui::Align::Center)"));
}

#[test]
fn workspace_exposes_all_plot_legend_visibility_helpers() {
    assert!(WORKSPACE_SOURCE.contains("pub fn set_all_plot_legends("));
    assert!(WORKSPACE_SOURCE.contains("pub fn all_plot_legends_visible("));
}

#[test]
fn shade_between_markers_defaults_on() {
    let start = SETTINGS_SOURCE
        .find("impl Default for PlotDisplay")
        .expect("PlotDisplay default should exist");
    let rest = &SETTINGS_SOURCE[start..];
    let end = rest
        .find("impl Default for RenderTuning")
        .expect("RenderTuning default should follow PlotDisplay default");
    let defaults = &rest[..end];
    assert!(defaults.contains("marker_shade_regions: true"));
}

#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::{LEGEND, WORKSPACE};

#[test]
fn legend_panel_uses_bounded_vertical_scroll_area() {
    assert!(LEGEND.contains("egui::ScrollArea::vertical()"));
    assert!(LEGEND.contains("legend_content_max_size(bounds, &frame)"));
    assert!(LEGEND.contains(".constrain_to(bounds)"));
    assert!(LEGEND.contains("ui.shrink_clip_rect(bounds)"));
    assert!(LEGEND.contains(".max_width(content_max_size.x)"));
    assert!(LEGEND.contains(".max_height(content_max_size.y)"));
    assert!(LEGEND.contains("ui.set_max_size(content_max_size)"));
    assert!(LEGEND.contains(".truncate()"));
    assert!(LEGEND.contains("fn legend_trace_row_widths"));
    assert!(LEGEND.contains("fn legend_ghost_label_width"));
    assert!(LEGEND.contains(".add_sized("));
    assert!(LEGEND.contains("egui::vec2("));
    assert!(LEGEND.contains("LEGEND_PREFERRED_TEXT_FILTER_WIDTH"));
    assert!(LEGEND.contains(".min_scrolled_height("));
    assert!(LEGEND.contains("LEGEND_PREFERRED_DELTA_WIDTH"));
    assert!(LEGEND.contains("LEGEND_MIN_TEXT_FILTER_WIDTH"));
    assert!(LEGEND.contains("widths.filter >= LEGEND_MIN_TEXT_FILTER_WIDTH"));
    assert!(LEGEND.contains("fn legend_can_show_color_picker"));
    assert!(LEGEND.contains("legend_can_show_color_picker("));
}

#[test]
fn legend_labels_hug_content_left_aligned() {
    // Labels must hug their content (capped at the row budget) instead of being
    // stretched to the full bounded width, so the legend stays compact and
    // left-aligned rather than spanning the whole plot.
    assert!(LEGEND.contains("allocate_ui_with_layout"));
    assert!(LEGEND.contains("egui::Layout::left_to_right(egui::Align::Center)"));
}

#[test]
fn workspace_exposes_all_plot_legend_visibility_helpers() {
    assert!(WORKSPACE.contains("pub fn set_all_plot_legends("));
    assert!(WORKSPACE.contains("pub fn all_plot_legends_visible("));
}

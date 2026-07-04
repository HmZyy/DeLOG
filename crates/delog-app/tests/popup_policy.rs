const POPUP_SOURCES: &[&str] = &[
    include_str!("../src/app.rs"),
    include_str!("../src/browser.rs"),
    include_str!("../src/generate_markers.rs"),
    include_str!("../src/live.rs"),
    include_str!("../src/parsers.rs"),
    include_str!("../src/scripts.rs"),
    include_str!("../src/settings.rs"),
    include_str!("../src/vehicle_dialog.rs"),
    include_str!("../src/workspace.rs"),
];
const APP_SOURCE: &str = include_str!("../src/app.rs");
const WORKSPACE_SOURCE: &str = include_str!("../src/workspace.rs");
const SETTINGS_SOURCE: &str = include_str!("../src/settings.rs");

fn occurrence_count(needle: &str) -> usize {
    POPUP_SOURCES
        .iter()
        .map(|source| source.matches(needle).count())
        .sum()
}

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("start marker should exist");
    let rest = &source[start..];
    let end = rest.find(end).expect("end marker should exist");
    &rest[..end]
}

#[test]
fn tools_menu_exposes_custom_parser_actions() {
    assert!(APP_SOURCE.contains("ui.menu_button(\"Parsers\""));
    assert!(APP_SOURCE.contains("Add new parser..."));
    assert!(APP_SOURCE.contains("crate::icons::pencil()"));
    assert!(APP_SOURCE.contains(".on_hover_text(\"Edit\")"));
}

#[test]
fn browser_exposes_field_metadata_inspector() {
    let browser = include_str!("../src/browser.rs");

    assert!(browser.contains("inspect_field_metadata"));
    assert!(browser.contains("Field metadata"));
    assert!(APP_SOURCE.contains("show_field_metadata_window"));
}

#[test]
fn moved_plot_controls_live_on_icon_toolbar_not_plot_context_menu() {
    let context_menu = between(
        WORKSPACE_SOURCE,
        "fn plot_context_menu(",
        "fn plot_info_window(",
    );
    for removed in [
        "\"Show legend\"",
        "\"Hover mode\"",
        "\"Snap\"",
        "\"Add measuring marker\"",
        "\"Remove measuring marker\"",
    ] {
        assert!(
            !context_menu.contains(removed),
            "{removed} should not be rendered from the plot context menu"
        );
    }

    let toolbar = between(
        APP_SOURCE,
        "egui::Panel::top(\"tool_icons\")",
        "drop(ui_toolbar_timer);",
    );
    for id in [
        "toolbar-hover-mode",
        "toolbar-snap-playhead",
        "toolbar-measuring-marker",
        "toolbar-legends",
        "toolbar-legend-position",
    ] {
        assert!(
            toolbar.contains(id),
            "{id} should be an icon toolbar control"
        );
    }
    assert!(!toolbar.contains("toolbar-marker-shade"));
    assert!(!toolbar.contains("Shade between markers"));
    assert!(toolbar.contains("hover_mode_menu_button("));
    assert!(!toolbar.contains("next_sample_mode("));
    assert!(toolbar.contains("legend_position_icon("));
    assert!(!toolbar.contains("crate::icons::panel_top()"));
    assert!(toolbar.contains(".on_hover_text(\"Cycle legend position\")"));
    assert!(!toolbar.contains("Cycle legend position - current"));
    let marker = toolbar
        .find("toolbar-measuring-marker")
        .expect("measuring marker control should exist");
    let legends = toolbar
        .find("toolbar-legends")
        .expect("legend visibility control should exist");
    assert!(
        toolbar[marker..legends].contains("ui.separator();"),
        "a separator should split marker tools from legend tools"
    );
    for tooltip in [
        "Select hover mode",
        "Toggle playhead snap",
        "Add measuring marker",
        "Toggle legends",
        "Cycle legend position",
    ] {
        assert!(
            toolbar.contains(tooltip),
            "{tooltip} should be exposed as hover text"
        );
    }
    for mode in ["Previous", "Next", "Linear"] {
        assert!(
            toolbar.contains(mode),
            "{mode} should be selectable from the hover mode menu"
        );
    }
    for icon in [
        "dice_top_left",
        "dice_top_right",
        "dice_bottom_left",
        "dice_bottom_right",
    ] {
        assert!(APP_SOURCE.contains(icon));
    }
}

#[test]
fn measuring_marker_scope_is_not_a_runtime_plot_setting() {
    assert!(!WORKSPACE_SOURCE.contains("marker_scope"));
    assert!(!WORKSPACE_SOURCE.contains("MarkerScope"));
    assert!(!APP_SOURCE.contains("marker_scope:"));
    assert!(!SETTINGS_SOURCE.contains("settings-marker-scope"));
}

#[test]
fn browser_topic_tables_keep_field_drag_source() {
    let browser = include_str!("../src/browser.rs");

    let visible_loop = browser
        .find("for &field_idx in &visible_topic.fields")
        .expect("topic tables should iterate the filtered field indexes");
    let table_row_call = browser[visible_loop..]
        .find("field_table_row(ui, field, selection, &visible)")
        .map(|offset| visible_loop + offset)
        .expect("filtered field loop should render field table rows");
    assert!(
        table_row_call - visible_loop < 200,
        "field_table_row should be called directly from the visible field loop"
    );

    let table_row = browser
        .find("fn field_table_row(")
        .expect("field_table_row helper should exist");
    let field_row_delegate = browser[table_row..]
        .find("field_row(ui, field, selection, visible")
        .map(|offset| table_row + offset)
        .expect("field_table_row should delegate to field_row");
    assert!(
        field_row_delegate - table_row < 250,
        "field_table_row should delegate before rendering custom contents"
    );

    let field_row = browser
        .find("fn field_row(")
        .expect("field_row helper should exist");
    let drag_source = browser[field_row..]
        .find("drag_source_with_click(ui, id, payload")
        .map(|offset| field_row + offset)
        .expect("field_row should own the drag source wrapper");
    assert!(
        drag_source - field_row < 900,
        "drag source should remain in the field row path"
    );

    let header = browser
        .find("fn field_table_header(")
        .expect("field_table_header helper should exist");
    // The field column header is intentionally left empty.
    for label in ["\"first\"", "\"last\"", "\"unit\"", "\"type\""] {
        let label = browser[header..]
            .find(label)
            .map(|offset| header + offset)
            .expect("field_table_header should contain every table label");
        assert!(
            label - header < 900,
            "header labels should be rendered inside field_table_header"
        );
    }

    let table_cell_calls = browser[table_row..field_row]
        .matches("field_table_cell(\n                ui,")
        .count();
    assert_eq!(
        table_cell_calls, 5,
        "field_table_row should render every column through the table cell helper"
    );
    assert!(
        browser.contains("egui::Label::new(text).truncate()"),
        "field table cells should truncate to their fixed widths"
    );
    assert!(browser.contains("cell_hover_text"));
    assert!(!browser.contains("#[allow(dead_code)]\nfn display_endpoint"));
}

#[test]
fn browser_topic_table_layout_keeps_source_actions() {
    let browser = include_str!("../src/browser.rs");

    assert!(browser.contains("Source metadata"));
    assert!(browser.contains("Remove source"));
    assert!(browser.contains("offset_widget"));
    assert!(browser.contains("offset_dialog_window"));
    assert!(browser.contains("collapse_requested"));
}

#[test]
fn layout_menu_exposes_clear_current_layout() {
    assert!(APP_SOURCE.contains("ui.menu_button(\"Layout\""));
    assert!(APP_SOURCE.contains("Clear current layout"));
    assert!(APP_SOURCE.contains("self.clear_current_layout();"));
}

#[test]
fn removed_workspace_fields_are_pruned_before_cache_requests() {
    let prune = APP_SOURCE
        .find("self.workspace.prune_removed_fields(&snapshot)")
        .expect("workspace should prune removed fields on epoch changes");
    let request = APP_SOURCE
        .find("self.caches.request(field, &snapshot);")
        .expect("workspace fields should request render caches");

    assert!(prune < request);
}

#[test]
fn every_popup_is_non_collapsible_and_centered_by_default() {
    let popup_count = occurrence_count("egui::Window::new(");

    assert_eq!(occurrence_count(".collapsible(false)"), popup_count);
    assert_eq!(
        occurrence_count(".default_pos(ctx.content_rect().center())")
            + occurrence_count(".default_pos(ui.ctx().content_rect().center())"),
        popup_count
    );
    assert_eq!(
        occurrence_count(".pivot(egui::Align2::CENTER_CENTER)"),
        popup_count
    );
}

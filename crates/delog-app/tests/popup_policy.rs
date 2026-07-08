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
const DOCKS_SOURCE: &str = include_str!("../src/docks.rs");
const SCRIPTS_SOURCE: &str = include_str!("../src/scripts.rs");
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
fn menus_expose_scripts_parsers_and_scripting_console_dock() {
    assert!(APP_SOURCE.contains("AppDockTab::ScriptingConsole, \"Scripting (F9)\""));
    assert!(APP_SOURCE.contains("AppDockTab::Logging, \"Logging (F12)\""));
    assert!(APP_SOURCE.contains("self.dock_open_checkbox(ui, AppDockTab::ScriptingConsole"));
    assert!(APP_SOURCE.contains("ui.menu_button(\"Scripts\""));
    assert!(APP_SOURCE.contains("ui.menu_button(\"Parsers\""));
    assert!(APP_SOURCE.contains("ui.button(\"Editor...\")"));
    assert!(APP_SOURCE.contains("ui.menu_button(\"Run\""));
    assert!(APP_SOURCE.contains("ui.menu_button(\"Parse File\""));
}

#[test]
fn view_menu_orders_docks_and_function_keys_focus_them() {
    let view_menu = between(
        APP_SOURCE,
        "ui.menu_button(\"View\"",
        "ui.menu_button(\"Layout\"",
    );
    let expected_order = [
        "\"Diagnostic (F1)\"",
        "\"Performance (F2)\"",
        "\"Markers (F3)\"",
        "\"Scripting (F9)\"",
        "\"Logging (F12)\"",
    ];
    let mut previous = 0;
    for label in expected_order {
        let index = view_menu[previous..]
            .find(label)
            .unwrap_or_else(|| panic!("{label} should be in the View menu"))
            + previous;
        previous = index + label.len();
    }

    for key in [
        "egui::Key::F1",
        "egui::Key::F2",
        "egui::Key::F3",
        "egui::Key::F9",
        "egui::Key::F12",
    ] {
        assert!(APP_SOURCE.contains(key));
    }

    assert!(APP_SOURCE.contains("self.open_dock(AppDockTab::Diagnostics);"));
    assert!(APP_SOURCE.contains("self.open_dock(AppDockTab::Performance);"));
    assert!(APP_SOURCE.contains("self.open_dock(AppDockTab::Markers);"));
    assert!(APP_SOURCE.contains("self.open_dock(AppDockTab::ScriptingConsole);"));
    assert!(APP_SOURCE.contains("self.open_dock(AppDockTab::Logging);"));
    assert!(!APP_SOURCE.contains("self.diagnostics_dock.open = !self.diagnostics_dock.open"));
    assert!(!APP_SOURCE.contains("self.performance_dock.open = !self.performance_dock.open"));
    assert!(!APP_SOURCE.contains("self.markers_dock.open = !self.markers_dock.open"));
    assert!(!APP_SOURCE.contains("self.logging_dock.open = !self.logging_dock.open"));
}

#[test]
fn bottom_docks_use_egui_dock_fixed_tabs_without_floating_or_reordering() {
    assert!(APP_SOURCE.contains("egui::Panel::bottom(\"app_docks\")"));
    assert!(APP_SOURCE.contains(".default_size(240.0)"));
    assert!(APP_SOURCE.contains("docks.show_inside(ui, viewer);"));
    assert!(DOCKS_SOURCE.contains("egui_dock::DockArea"));
    assert!(DOCKS_SOURCE.contains("egui_dock::AllowedSplits::None"));
    assert!(DOCKS_SOURCE.contains(".draggable_tabs(false)"));
    assert!(DOCKS_SOURCE.contains(".show_close_buttons(false)"));
    assert!(DOCKS_SOURCE.contains(".show_leaf_close_all_buttons(false)"));
    assert!(DOCKS_SOURCE.contains(".show_leaf_collapse_buttons(false)"));
    assert!(DOCKS_SOURCE.contains("FIXED_ORDER"));
    assert!(DOCKS_SOURCE.contains("pub fn open_tabs(&self) -> Vec<AppDockTab>"));
    assert!(APP_SOURCE.contains("fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool"));
    assert!(APP_SOURCE.contains("fn is_closeable(&self, _tab: &Self::Tab) -> bool"));
    assert!(APP_SOURCE.contains("false"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"diagnostics\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"logging\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"performance\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"markers\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"scripting_console\")"));
}

#[test]
fn bottom_dock_bodies_do_not_render_redundant_headers_or_close_buttons() {
    let diagnostics = include_str!("../src/diagnostics.rs");
    let logging = include_str!("../src/logging.rs");
    let performance = include_str!("../src/performance.rs");
    let markers = include_str!("../src/markers.rs");

    for source in [diagnostics, logging, performance, markers, SCRIPTS_SOURCE] {
        assert!(!source.contains("ui.button(\"Close\")"));
        assert!(!source.contains("ui.button(\"Clear\")"));
    }

    assert!(!diagnostics.contains("ui.strong(\"Diagnostics\")"));
    assert!(!logging.contains("ui.strong(\"Logging\")"));
    assert!(!performance.contains("ui.strong(\"Performance\")"));
    assert!(!markers.contains("ui.strong(\"Markers\")"));
    assert!(!SCRIPTS_SOURCE.contains("ui.strong(\"Scripting Console\")"));

    assert!(diagnostics.contains("crate::icons::trash()"));
    assert!(logging.contains("crate::icons::trash()"));
    assert!(SCRIPTS_SOURCE.contains("crate::icons::trash()"));
}

#[test]
fn scripting_console_dock_matches_diagnostics_height_and_reserves_prompt() {
    assert!(SCRIPTS_SOURCE.contains("egui::Panel::bottom(\"scripting_console_input\")"));
    let console = between(
        SCRIPTS_SOURCE,
        "pub fn console_dock_ui(",
        "fn variables_window(",
    );
    assert!(!console.contains("self.status"));
}

#[test]
fn scripting_console_refocuses_prompt_after_enter_dispatch() {
    let console = between(
        SCRIPTS_SOURCE,
        "pub fn console_dock_ui(",
        "fn variables_window(",
    );

    assert!(console.contains("if dispatch_enabled && self.take_repl_refocus_request() {"));
    assert!(console.contains("resp.request_focus();"));
    assert!(console.contains("self.request_repl_refocus();"));
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
        "toolbar-equal-plot-heights",
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
    let equal_heights = toolbar
        .find("toolbar-equal-plot-heights")
        .expect("equal plot heights control should exist");
    let legends = toolbar
        .find("toolbar-legends")
        .expect("legend visibility control should exist");
    assert!(
        marker < equal_heights && equal_heights < legends,
        "equal plot heights should sit between the measuring marker and legend controls"
    );
    assert!(
        toolbar[marker..legends].contains("ui.separator();"),
        "a separator should split marker tools from legend tools"
    );
    assert!(toolbar.contains("self.workspace.equalize_plot_heights();"));
    assert!(toolbar.contains("crate::icons::ruler_dimension_line()"));
    assert!(toolbar.contains("crate::icons::grid_2x2_check()"));
    assert!(toolbar.contains("let legends_hidden = !self.workspace.all_plot_legends_visible();"));
    assert!(toolbar.contains("legend_tint = if legends_hidden"));
    assert!(toolbar.contains("crate::icons::eye_off()"));
    assert!(toolbar.contains("legend_tint,\n                    legends_hidden,"));
    for tooltip in [
        "Select hover mode",
        "Toggle playhead snap",
        "Add measuring marker",
        "Resize all plots",
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

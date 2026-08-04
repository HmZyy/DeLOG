#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::{
    APP as APP_SOURCE, BROWSER, DATA_EXPORT as DATA_EXPORT_SOURCE, DIAGNOSTICS,
    DOCKS as DOCKS_SOURCE, GENERATE_MARKERS, LIVE, LOGGING, MARKERS, MESSAGE_POPUP, PARSERS,
    PERFORMANCE, SCRIPTS as SCRIPTS_SOURCE, SETTINGS as SETTINGS_SOURCE,
    SYNC_WINDOW as SYNC_WINDOW_SOURCE, VEHICLE_DIALOG, WORKSPACE as WORKSPACE_SOURCE,
};

const CONTEXT_HEADER_SOURCE: &str = include_str!("../src/shell/app/context_header.rs");
const COMMANDS_SOURCE: &str = include_str!("../src/shell/app/commands.rs");
const GLOBAL_TOOLBAR_SOURCE: &str = include_str!("../src/shell/app/global_plot_toolbar.rs");

const POPUP_SOURCES: &[&str] = &[
    APP_SOURCE,
    BROWSER,
    GENERATE_MARKERS,
    LIVE,
    MESSAGE_POPUP,
    PARSERS,
    SCRIPTS_SOURCE,
    SETTINGS_SOURCE,
    VEHICLE_DIALOG,
    WORKSPACE_SOURCE,
];

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

fn assert_commands_in_order(source: &str, commands: &[&str]) {
    let mut previous = 0;
    for command in commands {
        let index = source[previous..]
            .find(command)
            .unwrap_or_else(|| panic!("{command} should be present"))
            + previous;
        previous = index + command.len();
    }
}

#[test]
fn sync_toolbar_uses_icons_instead_of_unsupported_arrow_glyphs() {
    assert!(!SYNC_WINDOW_SOURCE.contains('→'));
    assert!(!SYNC_WINDOW_SOURCE.contains('↔'));
    assert!(SYNC_WINDOW_SOURCE.contains("crate::ui::icons::arrow_right()"));
    assert!(SYNC_WINDOW_SOURCE.contains("crate::ui::icons::arrow_left_right()"));
}

#[test]
fn menus_expose_scripts_parsers_and_scripting_console_dock() {
    let tools_static = between(
        CONTEXT_HEADER_SOURCE,
        "const TOOLS_MENU",
        "const TOOLS_SCRIPTS_MENU",
    );
    assert_commands_in_order(tools_static, &["CommandId::OpenSettings"]);
    let scripts = between(
        CONTEXT_HEADER_SOURCE,
        "const TOOLS_SCRIPTS_MENU",
        "const TOOLS_PARSERS_MENU",
    );
    assert_commands_in_order(
        scripts,
        &[
            "CommandId::OpenScriptEditor",
            "CommandId::OpenScriptVariables",
        ],
    );
    let parsers = between(
        CONTEXT_HEADER_SOURCE,
        "const TOOLS_PARSERS_MENU",
        "#[cfg(test)]",
    );
    assert_commands_in_order(parsers, &["CommandId::OpenParserEditor"]);
    let tools = &CONTEXT_HEADER_SOURCE[CONTEXT_HEADER_SOURCE
        .find("ui.menu_button(\"Tools\"")
        .expect("Tools menu should exist")..];
    assert!(tools.contains("ui.menu_button(\"Scripts\""));
    assert!(tools.contains("ui.menu_button(\"Run Scripts\""));
    assert!(tools.contains("ui.menu_button(\"Parsers\""));
    assert!(tools.contains("ui.menu_button(\"Run Parser\""));
    assert!(tools.contains("ui.menu_button(\"Layouts\""));
    assert_commands_in_order(
        tools,
        &[
            "ui.menu_button(\"Scripts\"",
            "ui.menu_button(\"Parsers\"",
            "ui.menu_button(\"Layouts\"",
            "TOOLS_MENU",
        ],
    );
    assert!(!scripts.contains("CommandId::OpenScripting"));
    assert!(APP_SOURCE.contains("AppCommand::RunScript"));
    assert!(APP_SOURCE.contains("AppCommand::OpenWithParser"));
    assert!(APP_SOURCE.contains("CommandId::OpenScripting => Some(AppDockTab::ScriptingConsole)"));
    assert!(COMMANDS_SOURCE.contains("Some(\"F9\")"));
    assert!(COMMANDS_SOURCE.contains("Some(\"F12\")"));
}

#[test]
fn dynamic_commands_live_under_the_user_authoritative_nested_menus() {
    let file = between(
        CONTEXT_HEADER_SOURCE,
        "ui.menu_button(\"File\"",
        "ui.menu_button(\"View\"",
    );
    let open_with = between(
        file,
        "ui.menu_button(\"Open With\"",
        "ui.menu_button(\"Export\"",
    );
    assert!(open_with.contains("AppCommand::OpenWithBuiltInParser"));

    let tools = &CONTEXT_HEADER_SOURCE[CONTEXT_HEADER_SOURCE
        .find("ui.menu_button(\"Tools\"")
        .expect("Tools menu should exist")..];
    let scripts = between(
        tools,
        "ui.menu_button(\"Scripts\"",
        "ui.menu_button(\"Parsers\"",
    );
    let run_script = between(
        scripts,
        "ui.menu_button(\"Run Scripts\"",
        "TOOLS_SCRIPTS_MENU",
    );
    assert!(run_script.contains("AppCommand::RunScript"));

    let parsers = between(
        tools,
        "ui.menu_button(\"Parsers\"",
        "ui.menu_button(\"Layouts\"",
    );
    let parser_editor = parsers
        .find("TOOLS_PARSERS_MENU")
        .expect("Parser Editor should be rendered");
    let run_parser = parsers
        .find("ui.menu_button(\"Run Parser\"")
        .expect("Run Parser submenu should exist");
    assert!(parser_editor < run_parser);
    assert!(parsers[run_parser..].contains("AppCommand::OpenWithParser"));

    let layouts = &tools[tools
        .find("ui.menu_button(\"Layouts\"")
        .expect("Layouts submenu should exist")..];
    let load_layout = between(
        layouts,
        "ui.menu_button(\"Load Layout\"",
        "&TOOLS_LAYOUTS_MENU[",
    );
    assert!(load_layout.contains("AppCommand::LoadNamedLayout"));
    assert!(!load_layout.contains("CommandId::LoadLayout"));

    assert!(APP_SOURCE.contains("self.spawn_open_dialog(ctx, Some(&name))"));
    assert!(APP_SOURCE.contains("self.scripts.request_open(ctx, &name)"));
}

#[test]
fn dynamic_menu_rows_use_only_their_display_names() {
    let presentations = between(
        APP_SOURCE,
        "fn command_presentations(",
        "fn command_palette_entries(",
    );
    for forbidden_prefix in [
        "format!(\"Open with",
        "format!(\"Parse file with",
        "format!(\"Run script:",
        "format!(\"Load layout:",
    ] {
        assert!(
            !presentations.contains(forbidden_prefix),
            "dynamic row retains action prefix {forbidden_prefix:?}"
        );
    }
}

#[test]
fn view_and_panel_rows_render_from_canonical_checked_state() {
    let view = between(
        CONTEXT_HEADER_SOURCE,
        "ui.menu_button(\"View\"",
        "ui.menu_button(\"Analyze\"",
    );
    assert_eq!(view.matches("checked_menu_items(").count(), 2);
    assert!(CONTEXT_HEADER_SOURCE.contains("egui::Checkbox::new(&mut is_selected, text)"));
    assert!(CONTEXT_HEADER_SOURCE.contains("presentation.selected.unwrap_or(false)"));
}

#[test]
fn view_panels_menu_orders_docks_and_function_keys_focus_them() {
    let panels = between(
        CONTEXT_HEADER_SOURCE,
        "const VIEW_PANELS_MENU",
        "const ANALYZE_MENU",
    );
    let expected_order = [
        "CommandId::OpenDiagnostics",
        "CommandId::OpenPerformance",
        "CommandId::OpenMarkers",
        "CommandId::OpenScripting",
        "CommandId::OpenLogging",
    ];
    let mut previous = 0;
    for command in expected_order {
        let index = panels[previous..]
            .find(command)
            .unwrap_or_else(|| panic!("{command} should be in the Panels menu"))
            + previous;
        previous = index + command.len();
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
    assert!(APP_SOURCE.contains("if let Some(dock) = dock_for_command(command)"));
    assert!(APP_SOURCE.contains("self.open_dock(dock);"));
    assert!(APP_SOURCE.contains("self.toggle_dock(AppDockTab::Diagnostics)"));
}

#[test]
fn bottom_docks_use_egui_dock_fixed_tabs_without_floating_or_reordering() {
    assert!(APP_SOURCE.contains("egui::Panel::bottom(\"app_docks\")"));
    assert!(APP_SOURCE.contains(".default_size(240.0)"));
    assert!(APP_SOURCE.contains("docks.show_inside(ui, viewer);"));
    assert!(DOCKS_SOURCE.contains("egui_dock::DockArea"));
    assert!(DOCKS_SOURCE.contains("egui_dock::AllowedSplits::None"));
    assert!(DOCKS_SOURCE.contains(".draggable_tabs(false)"));
    assert!(DOCKS_SOURCE.contains(".show_close_buttons(true)"));
    assert!(DOCKS_SOURCE.contains(".show_leaf_close_all_buttons(true)"));
    assert!(DOCKS_SOURCE.contains(".show_leaf_collapse_buttons(false)"));
    assert!(DOCKS_SOURCE.contains("FIXED_ORDER"));
    assert!(DOCKS_SOURCE.contains("pub fn open_tabs(&self) -> Vec<AppDockTab>"));
    assert!(APP_SOURCE.contains("fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"diagnostics\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"logging\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"performance\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"markers\")"));
    assert!(!APP_SOURCE.contains("egui::Panel::bottom(\"scripting_console\")"));
}

#[test]
fn bottom_dock_bodies_do_not_render_redundant_headers_or_close_buttons() {
    let diagnostics = DIAGNOSTICS;
    let logging = LOGGING;
    let performance = PERFORMANCE;
    let markers = MARKERS;

    for source in [diagnostics, logging, performance, markers, SCRIPTS_SOURCE] {
        assert!(!source.contains("ui.button(\"Close\")"));
        assert!(!source.contains("ui.button(\"Clear\")"));
    }

    assert!(!diagnostics.contains("ui.strong(\"Diagnostics\")"));
    assert!(!logging.contains("ui.strong(\"Logging\")"));
    assert!(!performance.contains("ui.strong(\"Performance\")"));
    assert!(!markers.contains("ui.strong(\"Markers\")"));
    assert!(!SCRIPTS_SOURCE.contains("ui.strong(\"Scripting Console\")"));

    assert!(diagnostics.contains("crate::ui::icons::trash()"));
    assert!(logging.contains("crate::ui::icons::trash()"));
    assert!(SCRIPTS_SOURCE.contains("crate::ui::icons::trash()"));
}

#[test]
fn clear_trash_buttons_are_aligned_to_the_right_of_their_control_rows() {
    let diagnostics = DIAGNOSTICS;
    let logging = LOGGING;
    let diagnostics_controls = between(
        diagnostics,
        "egui::TextEdit::singleline(&mut self.search)",
        "let filtered = filtered_records(",
    );
    let logging_controls = between(
        logging,
        "egui::TextEdit::singleline(&mut self.search)",
        "let filtered = filtered_records(",
    );
    // The clear button is pinned flush right via a right-to-left layout rather
    // than reserving a guessed width, which previously left a gap at the edge.
    assert!(!diagnostics_controls.contains("ui.add_space(ui.available_width())"));
    assert!(!logging_controls.contains("ui.add_space(ui.available_width())"));
    assert!(!diagnostics_controls.contains("interact_size.x"));
    assert!(!logging_controls.contains("interact_size.x"));
    assert!(diagnostics_controls.contains("egui::Layout::right_to_left(egui::Align::Center)"));
    assert!(logging_controls.contains("egui::Layout::right_to_left(egui::Align::Center)"));

    let console = between(
        SCRIPTS_SOURCE,
        "let dispatch_enabled = self.ordinary_dispatch_enabled();",
        "// The popup owns Up/Down/Tab/Enter/Esc while it is open.",
    );
    assert!(!console.contains(".desired_width(f32::INFINITY)"));
    assert!(!console.contains("interact_size.x"));
    assert!(console.contains("egui::Layout::right_to_left(egui::Align::Center)"));
    assert!(console.contains("crate::ui::icons::trash()"));
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
fn browser_exposes_field_metadata_inspector() {
    let browser = BROWSER;

    assert!(browser.contains("inspect_field_metadata"));
    assert!(browser.contains("Field metadata"));
    assert!(APP_SOURCE.contains("show_field_metadata_window"));
}

#[test]
fn global_toolbar_keeps_only_global_plot_state_controls() {
    assert!(!APP_SOURCE.contains("egui::Panel::top(\"tool_icons\")"));
    for command in [
        "CommandId::TogglePlayheadSnap",
        "CommandId::AddMeasuringMarker",
        "CommandId::EqualizePlots",
        "CommandId::ToggleLegends",
        "CommandId::CycleLegendPosition",
    ] {
        assert!(COMMANDS_SOURCE.contains(command.trim_start_matches("CommandId::")));
        assert!(APP_SOURCE.contains(command));
    }
    assert!(!GLOBAL_TOOLBAR_SOURCE.contains("GlobalPlotControl::FitAll"));
    assert!(!GLOBAL_TOOLBAR_SOURCE.contains("GlobalPlotControl::ToggleAllLegends"));
    assert!(!GLOBAL_TOOLBAR_SOURCE.contains("GlobalPlotControl::EqualizePlotHeights"));
    assert!(!GLOBAL_TOOLBAR_SOURCE.contains("X axes linked"));
    assert!(GLOBAL_TOOLBAR_SOURCE.contains("CommandId::AddMeasuringMarker"));
}

#[test]
fn plot_controls_respect_global_and_local_scope() {
    assert!(!WORKSPACE_SOURCE.contains("fn plot_toolbar("));
    for label in [
        "Split horizontally",
        "Split vertically",
        "Show legend",
        "Show tooltip",
        "Plot Info",
    ] {
        assert!(
            WORKSPACE_SOURCE.contains(label),
            "missing pane action {label}"
        );
    }
    assert!(!WORKSPACE_SOURCE.contains("Toggle measuring marker"));
}

#[test]
fn scene_controls_share_one_horizontal_toolbar() {
    let overlay = between(
        WORKSPACE_SOURCE,
        "fn scene_overlay_buttons(",
        "fn menu_icon(",
    );
    assert!(overlay.contains("ui.horizontal(|ui|"));
    assert!(!overlay.contains("ui.vertical(|ui|"));
    assert_eq!(overlay.matches("components::icon_button(").count(), 3);
}

#[test]
fn field_stats_is_a_global_toolbar_action_not_a_per_plot_one() {
    let context_menu = between(
        WORKSPACE_SOURCE,
        "fn plot_context_menu(",
        "fn plot_info_window(",
    );

    assert!(
        !context_menu.contains("Field stats"),
        "field stats should no longer be offered per plot"
    );
    assert!(
        !WORKSPACE_SOURCE.contains("inspect_field_stats"),
        "the per-plot field stats action should be gone entirely"
    );
    assert!(!context_menu.contains("Inspect trace"));
    assert!(!context_menu.contains("self.actions.inspect_trace"));
    assert!(!BROWSER.contains("focus the Inspector"));

    assert!(
        GLOBAL_TOOLBAR_SOURCE.contains("GlobalPlotControl::OpenFieldStats"),
        "the global toolbar should offer field stats"
    );
    assert!(
        GLOBAL_TOOLBAR_SOURCE.contains("crate::ui::icons::sigma()"),
        "the global field stats button should use the sigma icon"
    );
    assert!(
        APP_SOURCE.contains("self.field_stats.open_plotted(self.workspace.unique_fields())"),
        "the global action should open stats for every plotted trace"
    );
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
    let browser = BROWSER;

    let visible_loop = browser
        .find("for &field_idx in &visible_topic.fields")
        .expect("topic tables should iterate the filtered field indexes");
    let leaf_node = browser[visible_loop..]
        .find("NodeBuilder::leaf(BrowserNode::Field(")
        .map(|offset| visible_loop + offset)
        .expect("filtered field loop should build one tree leaf per field");
    assert!(
        leaf_node - visible_loop < 320,
        "each visible field should become a tree leaf directly in the loop"
    );
    let table_row_call = browser[leaf_node..]
        .find("field_table_row(ui, field, selection, &visible)")
        .map(|offset| leaf_node + offset)
        .expect("field leaves should render field table rows");
    assert!(
        table_row_call - leaf_node < 320,
        "field_table_row should be rendered by the field leaf label"
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
    let browser = BROWSER;

    assert!(browser.contains("Source metadata"));
    assert!(browser.contains("Remove source"));
    assert!(browser.contains("offset_widget"));
    assert!(browser.contains("offset_dialog_window"));
    assert!(browser.contains("collapse_requested"));
}

#[test]
fn tools_layouts_menu_exposes_clear_current_layout() {
    let layouts = between(
        CONTEXT_HEADER_SOURCE,
        "const TOOLS_LAYOUTS_MENU",
        "#[cfg(test)]",
    );
    assert_commands_in_order(
        layouts,
        &[
            "CommandId::SaveLayout",
            "CommandId::ManageLayouts",
            "CommandId::ImportLayout",
            "CommandId::ExportLayout",
            "CommandId::ClearLayout",
        ],
    );
    assert!(COMMANDS_SOURCE.contains("\"Clear current layout\""));
    assert!(APP_SOURCE.contains("CommandId::ClearLayout => self.clear_current_layout()"));
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
fn kml_export_results_surface_message_popups() {
    assert!(APP_SOURCE.contains("message_popup::show_all(&mut self.message_popups"));
    assert!(APP_SOURCE.contains("MessagePopup::info("));
    assert!(APP_SOURCE.contains("MessagePopup::error("));
    assert!(!APP_SOURCE.contains("rfd::MessageDialog"));
}

#[test]
fn file_menu_and_nested_export_keep_the_requested_order() {
    let file = between(
        CONTEXT_HEADER_SOURCE,
        "const FILE_MENU",
        "const FILE_EXPORT_MENU",
    );
    let export = between(
        CONTEXT_HEADER_SOURCE,
        "const FILE_EXPORT_MENU",
        "const VIEW_MENU",
    );
    assert_commands_in_order(
        file,
        &[
            "CommandId::Open",
            "CommandId::ConnectLive",
            "CommandId::CancelTasks",
        ],
    );
    assert!(!file.contains("CommandId::DisconnectLive"));
    assert_commands_in_order(
        export,
        &[
            "CommandId::ExportData",
            "CommandId::ExportDiagnostics",
            "CommandId::ExportProfiling",
            "CommandId::ExportWorkspacePng",
        ],
    );
    let file_menu = between(
        CONTEXT_HEADER_SOURCE,
        "ui.menu_button(\"File\"",
        "ui.menu_button(\"View\"",
    );
    assert!(file_menu.contains("ui.menu_button(\"Open With\""));
    assert!(file_menu.contains("ui.menu_button(\"Export\""));
    assert!(!file_menu.contains("AppCommand::DisconnectLink"));
    assert!(file_menu.contains("ui.separator();"));
    let separator = file_menu.rfind("ui.separator();").unwrap();
    let exit = file_menu.rfind("CommandId::Exit").unwrap();
    assert!(separator < exit, "Exit must be last after a separator");
}

#[test]
fn source_tools_use_context_availability_and_unified_dispatch() {
    assert!(APP_SOURCE.contains("source.entry.kind == delog_core::identity::SourceKind::File"));
    assert!(COMMANDS_SOURCE.contains("context.offline_source_count < 2"));
    assert!(
        APP_SOURCE
            .contains("CommandId::SyncSources => self.sync_window = SyncWindow::open(snapshot)")
    );
    assert!(APP_SOURCE.contains("CommandId::OpenDataFlow => self.dataflow.open = true"));
}

#[test]
fn view_and_analyze_menus_keep_display_and_analysis_actions_separate() {
    let view = between(
        CONTEXT_HEADER_SOURCE,
        "const VIEW_MENU",
        "const VIEW_PANELS_MENU",
    );
    assert_commands_in_order(
        view,
        &[
            "CommandId::ToggleDataBrowser",
            "CommandId::ToggleInspector",
            "CommandId::ToggleScene3d",
        ],
    );
    assert!(!view.contains("CommandId::SyncSources"));
    assert!(!view.contains("CommandId::DisconnectLive"));
    let view_menu = between(
        CONTEXT_HEADER_SOURCE,
        "ui.menu_button(\"View\"",
        "ui.menu_button(\"Analyze\"",
    );
    assert!(view_menu.contains("ui.menu_button(\"Panels\""));
    assert!(!view_menu.contains("ui.menu_button(\"Layouts\""));

    let analyze = between(
        CONTEXT_HEADER_SOURCE,
        "const ANALYZE_MENU",
        "const TOOLS_MENU",
    );
    assert_commands_in_order(
        analyze,
        &[
            "CommandId::SyncSources",
            "CommandId::OpenDataFlow",
        ],
    );
    assert!(!analyze.contains("CommandId::AddMarker"));
    assert!(!analyze.contains("CommandId::OpenMarkers"));
    assert!(!analyze.contains("CommandId::ToggleScene3d"));
}

#[test]
fn context_header_orders_the_application_menus() {
    let menu_bar = between(
        CONTEXT_HEADER_SOURCE,
        "ui.horizontal_wrapped",
        "fn menu_items(",
    );
    let expected_order = ["File", "View", "Analyze", "Tools"];
    let mut previous = 0;

    for label in expected_order {
        let needle = format!("\"{label}\"");
        let index = menu_bar[previous..]
            .find(&needle)
            .unwrap_or_else(|| panic!("{label} should be in the context header"))
            + previous;
        previous = index + needle.len();
    }

    assert!(!menu_bar.contains("\"Edit\""));
    assert!(!menu_bar.contains("\"Source\""));
    assert!(!menu_bar.contains("\"Workspace\""));
    assert!(!menu_bar.contains("\"Extensions\""));
    assert!(!menu_bar.contains("Ctrl+K  Commands"));
    assert!(APP_SOURCE.contains("command_palette::should_toggle_palette"));
}

#[test]
fn user_directed_header_and_empty_shell_omit_onboarding_and_source_name() {
    assert!(!CONTEXT_HEADER_SOURCE.contains("active_source_label"));
    assert!(!APP_SOURCE.contains("workspace-empty-state"));
    assert!(!APP_SOURCE.contains("empty_state::show("));
}

#[test]
fn export_menu_and_results_use_unified_data_export_path() {
    assert!(COMMANDS_SOURCE.contains("\"Export data…\""));
    assert!(APP_SOURCE.contains("CommandId::ExportData => self.data_export.open()"));
    assert!(APP_SOURCE.contains("fn spawn_data_export("));
    assert!(APP_SOURCE.contains("\"data-export\""));
    assert!(!APP_SOURCE.contains(concat!("Export ", "CSV...")));
    assert!(!APP_SOURCE.contains(concat!("csv_", "export")));
    assert!(!APP_SOURCE.contains(concat!("csv_", "cancel")));
}

#[test]
fn data_export_rejects_stale_fields_before_opening_save_dialog() {
    let worker = between(APP_SOURCE, "fn spawn_data_export(", "fn load_layout(");
    let resolution = worker
        .find("resolve_export_fields")
        .expect("the complete selection should be resolved exactly");
    let spawn = worker
        .find(".spawn(move ||")
        .expect("the save dialog should run on a worker");
    let save_dialog = worker
        .find(".save_file()")
        .expect("the worker should open a save dialog");

    assert!(resolution < spawn);
    assert!(spawn < save_dialog);
    assert!(worker.contains("data_export_tx.send(DataExportEvent::Failed"));
}

#[test]
fn export_picker_controls_scrollbars_divider_and_add_hitbox_are_stable() {
    let dialog_body = between(DATA_EXPORT_SOURCE, "pub fn dialog_ui(", "pub const MODES");
    let format = dialog_body.find("ui.label(\"Format:\")").unwrap();
    let range = dialog_body.find("ui.label(\"Range:\")").unwrap();
    let resample = dialog_body.find("ui.label(\"Resample:\")").unwrap();
    let picker = dialog_body
        .find("field_picker_ui(ui, state, available)")
        .unwrap();
    assert!(format < range && range < resample && resample < picker);

    assert!(DATA_EXPORT_SOURCE.contains("id_salt(\"data_export_available_fields\")"));
    assert!(DATA_EXPORT_SOURCE.contains("id_salt(\"data_export_selected_fields\")"));
    assert_eq!(
        DATA_EXPORT_SOURCE
            .matches("ScrollBarVisibility::AlwaysVisible")
            .count(),
        2
    );

    let picker_body = between(
        DATA_EXPORT_SOURCE,
        "fn field_picker_ui(",
        "/// `visible` is the current ViewX",
    );
    assert!(picker_body.contains("ui.separator();"));
    assert!(picker_body.contains(".add_sized([24.0, 24.0], egui::Button::new(\"+\"))"));
    assert!(picker_body.contains("let already_selected = state.selected.contains(&field.id);"));
    assert!(picker_body.contains(".add_enabled_ui(!already_selected, |ui|"));
    assert!(picker_body.contains("let source_fully_selected = available"));
    assert!(picker_body.contains("!source_fully_selected,"));
    assert!(picker_body.contains("egui::Button::new(\"Add all\")"));
    assert!(picker_body.contains("add_source = Some(field.source.clone());"));
    assert!(!DATA_EXPORT_SOURCE.contains("ui.small_button(\"+\")"));

    assert!(DATA_EXPORT_SOURCE.contains(".default_height(440.0)"));
    assert!(DATA_EXPORT_SOURCE.contains(".min_height(300.0)"));
    assert!(DATA_EXPORT_SOURCE.contains(".resizable([true, true])"));
    assert!(DATA_EXPORT_SOURCE.contains("Panel::top(\"data_export_controls\")"));
    assert!(DATA_EXPORT_SOURCE.contains("Panel::bottom(\"data_export_actions\")"));
    assert!(DATA_EXPORT_SOURCE.contains("CentralPanel::default()"));
    assert!(picker_body.contains(".horizontal_top(|ui|"));
    assert_eq!(picker_body.matches(".allocate_ui_with_layout(").count(), 2);
    assert!(!DATA_EXPORT_SOURCE.contains("ui.set_height(ui.available_height())"));
    assert!(!DATA_EXPORT_SOURCE.contains("let footer_height ="));
    assert!(!DATA_EXPORT_SOURCE.contains("let picker_height ="));
    assert_eq!(
        DATA_EXPORT_SOURCE
            .matches(".auto_shrink([false, false])")
            .count(),
        2
    );
    assert_eq!(
        DATA_EXPORT_SOURCE
            .matches(".max_height(ui.available_height())")
            .count(),
        2
    );
    assert!(!DATA_EXPORT_SOURCE.contains(".max_height(280.0)"));
}

#[test]
fn parquet_export_disables_resampling_and_uses_native_topic_samples() {
    let dialog_body = between(DATA_EXPORT_SOURCE, "pub fn dialog_ui(", "pub const MODES");

    assert!(dialog_body.contains("state.set_format(format, available)"));
    assert!(dialog_body.contains("state.format == ExportFormat::Csv"));
    assert!(dialog_body.contains("ui.add_enabled_ui("));
    assert!(dialog_body.contains("\"Native samples per topic\""));
}

#[test]
fn writing_exports_report_progress_and_stay_cancellable() {
    let progress = between(DATA_EXPORT_SOURCE, "pub fn progress_ui(", "pub const MODES");

    assert!(progress.contains("egui::Window::new(\"Exporting data\")"));
    assert!(progress.contains(".collapsible(false)"));
    assert!(progress.contains("egui::ProgressBar::new(active.fraction())"));
    assert!(progress.contains("active.status()"));
    assert!(progress.contains("ui.button(\"Cancel\")"));
    assert!(progress.contains("active.request_cancel()"));

    assert!(APP_SOURCE.contains("crate::export::data_export::progress_ui("));

    let worker = between(APP_SOURCE, "fn spawn_data_export(", "fn load_layout(");
    let save_dialog = worker.find(".save_file()").unwrap();
    let started = worker
        .find("DataExportEvent::Started")
        .expect("a chosen destination starts a tracked export");
    let ctl = worker
        .find("crate::export::data_export::ExportCtl::new(")
        .expect("the writer runs under a cancellable control");

    assert!(save_dialog < started && started < ctl);
}

#[test]
fn export_footer_keeps_cancel_left_and_export_right() {
    let actions = between(
        DATA_EXPORT_SOURCE,
        "Panel::bottom(\"data_export_actions\")",
        "CentralPanel::default()",
    );
    let cancel = actions.find("ui.button(\"Cancel\")").unwrap();
    let right_layout = actions.find("Layout::right_to_left").unwrap();
    let export = actions.find("state.format.action_label()").unwrap();

    assert!(cancel < right_layout && right_layout < export);
}

#[test]
fn global_toolbar_is_rendered_once_through_the_context_header() {
    assert_eq!(APP_SOURCE.matches("global_plot_toolbar::show").count(), 1);
    let header_call = APP_SOURCE.find("context_header::show").unwrap();
    let toolbar_call = APP_SOURCE.find("global_plot_toolbar::show").unwrap();
    let workspace = APP_SOURCE
        .find("central_workspace_frame(ui.style()).show")
        .unwrap();

    assert!(header_call < toolbar_call && toolbar_call < workspace);
}

#[test]
fn header_separates_tools_from_an_ungrouped_toolbar() {
    assert!(CONTEXT_HEADER_SOURCE.contains(
        "refresh_dynamic_catalog |= tools_menu.response.clicked();\n            ui.separator();\n            commands.extend(show_toolbar(ui));"
    ));
    let toolbar = between(
        GLOBAL_TOOLBAR_SOURCE,
        "pub fn show(",
        "fn sample_mode_label(",
    );
    assert!(toolbar.contains("toolbar_container_frame(ui.style()).show"));
    assert!(!toolbar.contains("egui::Frame::group"));
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

#[test]
fn every_context_menu_uses_the_dense_row_tokens() {
    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable directory") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut checked = 0usize;
    for file in files {
        let source = std::fs::read_to_string(&file).expect("readable source");
        for (index, _) in source.match_indices(".context_menu(|ui| {") {
            checked += 1;
            let head: String = source[index..].chars().take(140).collect();
            assert!(
                head.contains("dense_rows(ui);"),
                "a context menu in {} does not apply the dense row tokens: {head}",
                file.display()
            );
        }
    }
    assert!(
        checked >= 6,
        "expected to audit every context menu, only found {checked}"
    );
}

#[test]
fn the_parquet_path_has_no_import_dialog() {
    assert!(APP_SOURCE.contains("\"parquet\""));

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable directory") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    assert!(
        files.len() >= 20,
        "expected to walk a substantial number of source files under {}, only found {}",
        src.display(),
        files.len()
    );

    for file in files {
        let source = std::fs::read_to_string(&file).expect("readable source");
        assert!(
            !source.contains("TimestampSelection"),
            "{} still references TimestampSelection; generic Parquet loads without a timestamp picker",
            file.display()
        );
        assert!(
            !source.contains("egui::Window::new(\"Import"),
            "{} still opens an Import window; generic Parquet loads without a timestamp picker",
            file.display()
        );
    }
}

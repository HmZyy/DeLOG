#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::{APP, APP_COMMANDS, BROWSER, WORKSPACE};

const COMMAND_AVAILABILITY: &str = include_str!("../src/shell/app/commands/mod.rs");

#[test]
fn per_pane_ids_are_salted_by_window_so_two_windows_do_not_share_state() {
    for id in [
        "plot_annotation",
        "playhead",
        "plot_hover",
        "plot_legend",
        "plot-info",
        "rename_trace",
    ] {
        let unsalted = format!("(\"{id}\", tile_id)");
        assert!(
            !WORKSPACE.contains(&unsalted),
            "{id} must be salted with the window it belongs to, found {unsalted}"
        );
    }
    assert!(WORKSPACE.contains("egui::Id::new((\"plot_workspace\", window.0))"));
}

#[test]
fn the_browser_takes_an_id_salt_so_each_window_owns_its_tree_state() {
    assert!(BROWSER.contains("pub fn filter_id(salt: egui::Id)"));
    assert!(!BROWSER.contains("egui::Id::new(\"browser_tree\")"));
    assert!(!BROWSER.contains("egui::Id::new(\"browser_tree_filtered\")"));
    assert!(!BROWSER.contains("egui::Id::new((\"field\", field.id.0))"));
}

#[test]
fn a_command_opens_a_new_plot_window() {
    assert_eq!(APP_COMMANDS.matches("NewPlotWindow => spec!(").count(), 1);
    assert_eq!(APP_COMMANDS.matches("\"New plot window\"").count(), 1);
    assert!(APP.contains("CommandId::NewPlotWindow => self.open_extended_window()"));
}

#[test]
fn extended_windows_render_before_the_main_window_and_after_the_frame_reset() {
    let begin = APP
        .find("self.gpu.begin_plot_frame(frame)")
        .expect("the frame should initialize the plot frame");
    let extended = APP
        .find("self.render_extended_windows(")
        .expect("extended windows should be rendered");
    let retain = APP
        .find("self.gpu.retain_plotted_buffers(frame, &plotted)")
        .expect("the frame should retain plotted buffers");
    let evict = APP
        .find("self.caches.evict_over_budget()")
        .expect("the frame should evict caches over budget");

    assert!(begin < extended, "windows render after the allocator reset");
    assert!(
        evict < extended,
        "extended windows render after the epoch roll and the per-frame cache pass, like the main window"
    );
    assert!(
        extended < retain,
        "buffers are retained after every window has built"
    );
    assert!(APP.contains("show_viewport_immediate"));
}

#[test]
fn the_browser_model_is_rebuilt_whether_or_not_the_main_browser_is_open() {
    let rebuild = APP
        .find("self.browser_model = Some((snapshot.epoch, BrowserModel::from_snapshot(&snapshot)))")
        .expect("the prelude should rebuild the browser model from the current snapshot");
    let browser_panel = APP
        .find("let ui_browser_timer")
        .expect("the main browser panel should be timed");
    let extended = APP
        .find("self.render_extended_windows(")
        .expect("extended windows should be rendered");

    assert!(
        rebuild < browser_panel && rebuild < extended,
        "the model has to be current before the main browser, the export dialog and every extended window read it"
    );
    assert_eq!(
        APP.matches("BrowserModel::from_snapshot(").count(),
        1,
        "one build site only: a rebuild inside the browser panel would skip the collapsed branch"
    );
}

#[test]
fn an_extended_window_browser_offers_the_same_live_actions_as_the_main_one() {
    assert_eq!(
        APP.matches("self.apply_browser_response(").count(),
        2,
        "both browsers must run their response through the one handler"
    );
    for action in [
        "response.offset_change",
        "response.remove_source",
        "response.inspect_source",
        "response.inspect_field_metadata",
        "response.inspect_field_stats",
        "response.generate_markers",
    ] {
        assert!(
            APP.contains(action),
            "{action} must be handled, not dropped"
        );
    }
    assert!(
        APP.contains("if window.browser.collapsed {"),
        "hiding an extended window's browser must actually hide the panel"
    );
    assert!(
        APP.contains("window.browser.collapsed = false;"),
        "a hidden extended browser must be reachable again"
    );
}

#[test]
fn extended_windows_hold_plots_only() {
    assert!(APP.contains("allow_image_export: window.is_main()"));
}

#[test]
fn gpu_and_cache_bookkeeping_spans_every_window() {
    assert!(APP.contains("union_fields(&self.workspace, &self.windows)"));
    assert!(
        !APP.contains("union_fields(&self.workspace, &[])"),
        "the union must include extended windows, not just the main one"
    );
    assert!(APP.contains("for workspace in self.each_workspace_mut()"));
}

#[test]
fn per_frame_cache_requests_span_every_window() {
    assert!(
        APP.contains("for field in self.plotted_fields() {\n            self.caches.request(field, &snapshot);\n        }"),
        "the per-frame cache request loop must walk the union of every window's fields, not just self.workspace.fields()"
    );
    assert!(
        !APP.contains("for field in self.workspace.fields().collect::<Vec<_>>() {\n            self.caches.request(field, &snapshot);\n        }"),
        "the request loop must not be scoped to the main workspace alone"
    );
}

#[test]
fn a_loaded_layout_reserves_ids_above_every_restored_window() {
    assert!(
        APP.contains("self.next_window_id = crate::shell::windows::next_window_id(&self.windows);"),
        "the next id must clear the highest restored id, not the window count"
    );
    assert!(
        !APP.contains("self.next_window_id = self.windows.len() as u64 + 1;"),
        "counting windows hands a new window the title of a restored one"
    );
}

#[test]
fn the_annotation_toolbar_spans_every_window_in_both_availability_and_action() {
    assert!(
        COMMAND_AVAILABILITY
            .contains("Self::ToggleAnnotationToolbar if !context.has_plotted_traces =>"),
        "the toolbar now lists annotations from every window, so its predicate must be the union"
    );
    assert!(
        COMMAND_AVAILABILITY.contains("Self::ToggleFieldStats if !context.has_plotted_traces"),
        "field stats acts on the union of every window, so its predicate stays the union"
    );
    assert!(
        APP.contains("crate::shell::windows::annotation_rows(&self.workspace, &self.windows)"),
        "the row list must be built across main plus every extended window"
    );
    assert!(
        APP.contains("crate::shell::windows::apply_annotation_action(")
            && APP.contains("&mut self.workspace,")
            && APP.contains("&mut self.windows,"),
        "actions must be routed to the workspace they address, not just self.workspace"
    );
}

#[test]
fn command_availability_checks_plotted_fields_in_every_window() {
    assert!(
        APP.contains(
            "self.workspace.fields().next().is_some()\n                || self\n                    .windows\n                    .iter()\n                    .any(|window| window.workspace.fields().next().is_some()),"
        ),
        "has_plotted_traces must span extended windows too, or ToggleFieldStats/ToggleAnnotationToolbar stay disabled for fields plotted only there"
    );
}

#[test]
fn the_header_offers_a_right_aligned_scene_button() {
    use policy_sources::CONTEXT_HEADER;

    let toolbar = CONTEXT_HEADER
        .find("commands.extend(show_toolbar(ui));")
        .expect("the header should render the global plot toolbar");
    let button = CONTEXT_HEADER
        .find("crate::ui::icons::cube()")
        .expect("the header should offer a 3D scene button");
    let aligned = CONTEXT_HEADER
        .find("egui::Layout::right_to_left(egui::Align::Center)")
        .expect("the 3D scene button should be right-aligned");

    assert!(
        button > toolbar,
        "the button belongs at the end of the toolbar row, after the toolbar itself"
    );
    assert!(
        aligned < button,
        "the button must sit inside a right-to-left layout so it hugs the right edge"
    );
    assert!(CONTEXT_HEADER.contains("CommandId::ToggleScene3d"));
}

#[test]
fn the_new_window_button_sits_in_the_left_group_beside_open() {
    use policy_sources::CONTEXT_HEADER;

    let toolbar = CONTEXT_HEADER
        .find("commands.extend(show_toolbar(ui));")
        .expect("the header should render the global plot toolbar");
    let button = CONTEXT_HEADER
        .find("crate::ui::icons::app_window()")
        .expect("the header should offer a new-window button");

    assert!(
        button < toolbar,
        "the new-window button belongs in the left group, before the toolbar"
    );
    assert!(CONTEXT_HEADER.contains("CommandId::NewPlotWindow"));
}

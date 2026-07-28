#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::{APP, CORE_INGEST, SYNC_WINDOW};

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("start marker should exist");
    let rest = &source[start..];
    let end = rest.find(end).expect("end marker should exist");
    &rest[..end]
}

#[test]
fn sync_window_is_modeless_private_and_atomically_applied() {
    assert_eq!(
        SYNC_WINDOW
            .matches("egui::Window::new(\"Sync Sources\")")
            .count(),
        1
    );
    assert_eq!(
        APP.matches("egui::Button::new(\"Sync Sources\")").count(),
        1
    );
    for source in [APP, SYNC_WINDOW] {
        assert!(!source.contains("Synchronize Data Sources"));
    }
    assert!(APP.contains("session.set_source_offsets"));
    assert!(SYNC_WINDOW.contains("Discard changes"));
    assert!(SYNC_WINDOW.contains("Keep editing"));
    assert!(CORE_INGEST.contains("SetSourceOffsets"));
    assert!(!SYNC_WINDOW.contains("session.set_source_offset("));

    assert!(SYNC_WINDOW.contains("CompareMode::Overlay, \"Overlay\""));
    assert!(SYNC_WINDOW.contains("CompareMode::Stacked, \"Stacked\""));
    assert_eq!(SYNC_WINDOW.matches("pub field: Option<FieldId>").count(), 1);

    assert!(SYNC_WINDOW.contains("movable: self.is_movable(id)"));
    assert!(SYNC_WINDOW.contains("ui.add_enabled_ui(movable"));
    assert!(SYNC_WINDOW.contains("if !self.is_movable(id) {\n            return Err(());"));

    let integration = between(
        APP,
        "if let Some(mut sync_window) = self.sync_window.take()",
        "// Floating windows/dialogs + overlays",
    );
    assert_eq!(integration.matches("session.set_source_offsets").count(), 1);
    assert!(!integration.contains("session.set_source_offset("));
    assert!(integration.contains("if let Some(offsets) = response.apply"));
}

#[test]
fn sync_window_renders_after_workspace_gpu_setup_and_repaints_cache_builds() {
    let begin = APP
        .find("self.gpu.begin_plot_frame(frame)")
        .expect("workspace should initialize the plot frame");
    let retain = APP
        .find("self.gpu.retain_plotted_buffers(frame, &plotted)")
        .expect("workspace should retain its plotted buffers");
    let sync = APP
        .find("sync_window.show(")
        .expect("synchronization window should be rendered");

    assert!(
        sync > begin,
        "sync uniforms must be allocated after frame reset"
    );
    assert!(
        sync > retain,
        "sync buffers must be uploaded after workspace retention"
    );
    assert!(SYNC_WINDOW.contains("ui.ctx().request_repaint();"));
    assert!(SYNC_WINDOW.contains("caches.is_building(field)"));
}

#[test]
fn sync_controls_use_cumulative_drag_and_topic_scoped_field_selection() {
    assert!(SYNC_WINDOW.contains("interaction.total_drag_delta()"));
    assert!(!SYNC_WINDOW.contains("egui::Slider::new"));
    assert!(SYNC_WINDOW.contains("(\"sync-topic\", id.0)"));
    assert!(SYNC_WINDOW.contains("(\"sync-field\", id.0)"));
    assert!(SYNC_WINDOW.contains("plottable_fields(snapshot, id, topic)"));
}

#[test]
fn sync_preview_uses_double_click_fit_primary_pan_and_middle_alignment_drag() {
    assert!(SYNC_WINDOW.contains("interaction.double_clicked()"));
    assert!(SYNC_WINDOW.contains("fit_selected_plots(snapshot)"));
    assert!(SYNC_WINDOW.contains("interaction.dragged_by(egui::PointerButton::Primary)"));
    assert!(SYNC_WINDOW.contains("interaction.dragged_by(egui::PointerButton::Middle)"));
    assert!(SYNC_WINDOW.contains("gpu::apply_pan"));
    assert!(
        !SYNC_WINDOW.contains("self.view = snapshot.global_time_range().map(ViewX::from_range)")
    );
}

#[test]
fn sync_source_rows_offer_combined_topic_field_fuzzy_search() {
    assert!(SYNC_WINDOW.contains("Find topic/field…"));
    assert!(SYNC_WINDOW.contains("field_search_results(snapshot, id, search.trim())"));
    assert!(SYNC_WINDOW.contains("egui::Key::ArrowDown"));
    assert!(SYNC_WINDOW.contains("egui::Key::ArrowUp"));
    assert!(SYNC_WINDOW.contains("egui::Key::Enter"));
    assert!(SYNC_WINDOW.contains("select_search_result"));
}

#[test]
fn picker_live_status_is_rendered_after_plot_updates_hover_state() {
    let plot = SYNC_WINDOW
        .find("self.plot(ui, snapshot, gpu, frame, caches);")
        .expect("sync plot should be rendered");
    let status = SYNC_WINDOW
        .find("self.picker_status(ui, snapshot);")
        .expect("picker live status should be rendered");

    assert!(
        status > plot,
        "live hover detail must follow the plot update"
    );
}

#[test]
fn sync_anchor_toolbar_picker_and_standard_palette_remain_wired() {
    for label in [
        "First to First",
        "Last to Last",
        "Back to back",
        "First change",
        "Pick samples",
    ] {
        assert!(SYNC_WINDOW.contains(label), "missing sync action {label}");
    }
    assert!(SYNC_WINDOW.contains("crate::icons::arrow_left_right()"));
    assert!(SYNC_WINDOW.contains("align_and_begin_apply(snapshot"));
    assert!(SYNC_WINDOW.contains("begin_sample_pick"));
    assert!(SYNC_WINDOW.contains("sample_neighborhood"));
    assert!(SYNC_WINDOW.contains("egui::Key::Escape"));
    assert!(SYNC_WINDOW.contains("delog_render::palette"));
    assert!(SYNC_WINDOW.contains("color: palette::trace_color(index).to_srgb_f32()"));
    assert!(SYNC_WINDOW.contains("let color = palette::trace_color(index);"));
    assert!(!SYNC_WINDOW.contains("const COLORS: [egui::Color32; 6]"));
    assert!(!SYNC_WINDOW.contains("Affine"));
    assert!(!SYNC_WINDOW.contains("clock drift"));
}

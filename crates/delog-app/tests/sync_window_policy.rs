const APP_SOURCE: &str = include_str!("../src/app.rs");
const SYNC_SOURCE: &str = include_str!("../src/sync_window.rs");
const INGEST_SOURCE: &str = include_str!("../../delog-core/src/ingest.rs");

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("start marker should exist");
    let rest = &source[start..];
    let end = rest.find(end).expect("end marker should exist");
    &rest[..end]
}

#[test]
fn sync_window_is_modeless_private_and_atomically_applied() {
    assert_eq!(
        SYNC_SOURCE
            .matches("egui::Window::new(\"Sync Sources\")")
            .count(),
        1
    );
    assert_eq!(
        APP_SOURCE
            .matches("egui::Button::new(\"Sync Sources\")")
            .count(),
        1
    );
    for source in [APP_SOURCE, SYNC_SOURCE] {
        assert!(!source.contains("Synchronize Data Sources"));
    }
    assert!(APP_SOURCE.contains("session.set_source_offsets"));
    assert!(SYNC_SOURCE.contains("Discard changes"));
    assert!(SYNC_SOURCE.contains("Keep editing"));
    assert!(INGEST_SOURCE.contains("SetSourceOffsets"));
    assert!(!SYNC_SOURCE.contains("session.set_source_offset("));

    assert!(SYNC_SOURCE.contains("CompareMode::Overlay, \"Overlay\""));
    assert!(SYNC_SOURCE.contains("CompareMode::Stacked, \"Stacked\""));
    assert_eq!(SYNC_SOURCE.matches("pub field: Option<FieldId>").count(), 1);

    assert!(SYNC_SOURCE.contains("movable: self.is_movable(id)"));
    assert!(SYNC_SOURCE.contains("ui.add_enabled_ui(movable"));
    assert!(SYNC_SOURCE.contains("if !self.is_movable(id) {\n            return Err(());"));

    let integration = between(
        APP_SOURCE,
        "if let Some(mut sync_window) = self.sync_window.take()",
        "// Floating windows/dialogs + overlays",
    );
    assert_eq!(integration.matches("session.set_source_offsets").count(), 1);
    assert!(!integration.contains("session.set_source_offset("));
    assert!(integration.contains("if let Some(offsets) = response.apply"));
}

#[test]
fn sync_window_renders_after_workspace_gpu_setup_and_repaints_cache_builds() {
    let begin = APP_SOURCE
        .find("self.gpu.begin_plot_frame(frame)")
        .expect("workspace should initialize the plot frame");
    let retain = APP_SOURCE
        .find("self.gpu.retain_plotted_buffers(frame, &plotted)")
        .expect("workspace should retain its plotted buffers");
    let sync = APP_SOURCE
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
    assert!(SYNC_SOURCE.contains("ui.ctx().request_repaint();"));
    assert!(SYNC_SOURCE.contains("caches.is_building(field)"));
}

#[test]
fn sync_controls_use_cumulative_drag_and_topic_scoped_field_selection() {
    assert!(SYNC_SOURCE.contains("interaction.total_drag_delta()"));
    assert!(!SYNC_SOURCE.contains("egui::Slider::new"));
    assert!(SYNC_SOURCE.contains("(\"sync-topic\", id.0)"));
    assert!(SYNC_SOURCE.contains("(\"sync-field\", id.0)"));
    assert!(SYNC_SOURCE.contains("plottable_fields(snapshot, id, topic)"));
}

#[test]
fn sync_preview_uses_double_click_fit_primary_pan_and_middle_alignment_drag() {
    assert!(SYNC_SOURCE.contains("interaction.double_clicked()"));
    assert!(SYNC_SOURCE.contains("fit_selected_plots(snapshot)"));
    assert!(SYNC_SOURCE.contains("interaction.dragged_by(egui::PointerButton::Primary)"));
    assert!(SYNC_SOURCE.contains("interaction.dragged_by(egui::PointerButton::Middle)"));
    assert!(SYNC_SOURCE.contains("gpu::apply_pan"));
    assert!(
        !SYNC_SOURCE.contains("self.view = snapshot.global_time_range().map(ViewX::from_range)")
    );
}

#[test]
fn sync_source_rows_offer_combined_topic_field_fuzzy_search() {
    assert!(SYNC_SOURCE.contains("Find topic/field…"));
    assert!(SYNC_SOURCE.contains("field_search_results(snapshot, id, search.trim())"));
    assert!(SYNC_SOURCE.contains("egui::Key::ArrowDown"));
    assert!(SYNC_SOURCE.contains("egui::Key::ArrowUp"));
    assert!(SYNC_SOURCE.contains("egui::Key::Enter"));
    assert!(SYNC_SOURCE.contains("select_search_result"));
}

#[test]
fn picker_live_status_is_rendered_after_plot_updates_hover_state() {
    let plot = SYNC_SOURCE
        .find("self.plot(ui, snapshot, gpu, frame, caches);")
        .expect("sync plot should be rendered");
    let status = SYNC_SOURCE
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
        assert!(SYNC_SOURCE.contains(label), "missing sync action {label}");
    }
    assert!(SYNC_SOURCE.contains("crate::icons::arrow_left_right()"));
    assert!(SYNC_SOURCE.contains("align_and_begin_apply(snapshot"));
    assert!(SYNC_SOURCE.contains("begin_sample_pick"));
    assert!(SYNC_SOURCE.contains("sample_neighborhood"));
    assert!(SYNC_SOURCE.contains("egui::Key::Escape"));
    assert!(SYNC_SOURCE.contains("delog_render::palette"));
    assert!(SYNC_SOURCE.contains("color: palette::trace_color(index).to_srgb_f32()"));
    assert!(SYNC_SOURCE.contains("let color = palette::trace_color(index);"));
    assert!(!SYNC_SOURCE.contains("const COLORS: [egui::Color32; 6]"));
    assert!(!SYNC_SOURCE.contains("Affine"));
    assert!(!SYNC_SOURCE.contains("clock drift"));
}

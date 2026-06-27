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

fn occurrence_count(needle: &str) -> usize {
    POPUP_SOURCES
        .iter()
        .map(|source| source.matches(needle).count())
        .sum()
}

#[test]
fn tools_menu_exposes_custom_parser_actions() {
    let app = include_str!("../src/app.rs");

    assert!(app.contains("ui.menu_button(\"Parsers\""));
    assert!(app.contains("Add new parser..."));
    assert!(app.contains("crate::icons::pencil()"));
    assert!(app.contains(".on_hover_text(\"Edit\")"));
}

#[test]
fn browser_exposes_field_metadata_inspector() {
    let app = include_str!("../src/app.rs");
    let browser = include_str!("../src/browser.rs");

    assert!(browser.contains("inspect_field_metadata"));
    assert!(browser.contains("Field metadata"));
    assert!(app.contains("show_field_metadata_window"));
}

#[test]
fn layout_menu_exposes_clear_current_layout() {
    let app = include_str!("../src/app.rs");

    assert!(app.contains("ui.menu_button(\"Layout\""));
    assert!(app.contains("Clear current layout"));
    assert!(app.contains("self.clear_current_layout();"));
}

#[test]
fn removed_workspace_fields_are_pruned_before_cache_requests() {
    let app = include_str!("../src/app.rs");
    let prune = app
        .find("self.workspace.prune_removed_fields(&snapshot)")
        .expect("workspace should prune removed fields on epoch changes");
    let request = app
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

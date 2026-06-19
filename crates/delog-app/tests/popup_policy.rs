const POPUP_SOURCES: &[&str] = &[
    include_str!("../src/app.rs"),
    include_str!("../src/browser.rs"),
    include_str!("../src/generate_markers.rs"),
    include_str!("../src/live.rs"),
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

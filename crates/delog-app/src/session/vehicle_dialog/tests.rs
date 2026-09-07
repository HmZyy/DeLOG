use super::*;
use crate::scene3d::vehicle::ModelKind;

const MOD_SOURCE: &str = include_str!("mod.rs");
const DRAFT_SOURCE: &str = include_str!("draft.rs");
const PROFILE_DRAFT_SOURCE: &str = include_str!("profile_draft.rs");
const PROFILES_SOURCE: &str = include_str!("profiles.rs");
const PROFILES_TAB_SOURCE: &str = include_str!("profiles_tab.rs");
const VEHICLES_TAB_SOURCE: &str = include_str!("vehicles_tab.rs");
const WIDGETS_SOURCE: &str = include_str!("widgets.rs");

fn whole() -> String {
    [
        MOD_SOURCE,
        DRAFT_SOURCE,
        PROFILE_DRAFT_SOURCE,
        PROFILES_SOURCE,
        PROFILES_TAB_SOURCE,
        VEHICLES_TAB_SOURCE,
        WIDGETS_SOURCE,
    ]
    .concat()
}

#[test]
fn custom_glb_path_uses_file_picker_not_text_edit() {
    let source = whole();

    assert!(source.contains(".set_title(\"Choose custom GLB\")"));
    assert!(source.contains(".add_filter(\"GLB models\", &[\"glb\", \"GLB\"])"));
    let text_edit = concat!("text_edit_singleline", "(&mut draft.custom_path)");
    assert!(!source.contains(text_edit));
}

#[test]
fn searchable_topic_combo_keeps_scrollbar_at_popup_edge_without_fighting_mouse_scroll() {
    let combo = WIDGETS_SOURCE
        .split("fn searchable_combo")
        .nth(1)
        .expect("searchable combo should exist");

    assert!(
        combo.contains(".auto_shrink([false, true])"),
        "topic dropdown list should keep the horizontal space reserved by the popup"
    );
    assert!(
        combo.contains("let scroll_to_highlight")
            && combo.contains("highlight_changed_by_keyboard || initialized_highlight"),
        "highlighted topic should only request scrolling after an explicit highlight move"
    );
    assert!(
        combo.contains("if scroll_to_highlight && i == highlighted"),
        "mouse-wheel frames must not re-scroll to the highlighted topic"
    );
    assert!(
        !combo.contains(
            "if i == highlighted {\n                                response.scroll_to_me"
        ),
        "unconditional scroll_to_me fights normal mouse-wheel scrolling"
    );
}

#[test]
fn new_vehicle_draft_defaults_to_fixed_wing_model() {
    assert_eq!(Draft::default().model, ModelKind::FixedWing);
}

#[test]
fn vehicle_dialog_has_profile_tab_and_auto_apply_dropdown() {
    let source = whole();

    assert!(source.contains("VehicleDialogTab"));
    assert!(source.contains("Vehicle Config"));
    assert!(source.contains("Profiles"));
    assert!(source.contains("Profile"));
    assert!(source.contains("draft.selected_profile != before"));
    assert!(source.contains("ProfileAction::Apply"));
    assert!(source.contains("Add Profile"));
    assert!(source.contains("Update Profile"));
    assert!(source.contains("profile_editor_form"));
    assert!(source.contains("Lat/Lon units"));
    assert!(source.contains("Angle Unit"));
    assert!(source.contains("Delete profile?"));
    assert!(!source.contains(concat!("open_vehicle", "_profile")));
}

#[test]
fn vehicle_dialog_tabs_use_egui_dock() {
    let source = whole();

    assert!(MOD_SOURCE.contains("egui_dock::DockArea::new"));
    assert!(MOD_SOURCE.contains("impl egui_dock::TabViewer for VehicleDialogTabViewer"));
    assert!(!source.contains(concat!("selected", "_tab: VehicleDialogTab")));
}

#[test]
fn vehicle_dialog_default_width_is_wider_than_original_config() {
    const { assert!(DIALOG_WIDTH > 240.0) };
}

#[test]
fn add_vehicle_button_is_only_in_vehicle_config_tab() {
    let window_body = MOD_SOURCE
        .split(".show(ctx, |ui| {")
        .nth(1)
        .expect("vehicle window body should exist");

    assert!(!window_body.contains("\"Add Vehicle\""));
    assert!(VEHICLES_TAB_SOURCE.contains("\"Add Vehicle\""));
}

#[test]
fn profile_delete_uses_confirmation_window() {
    let source = whole();

    assert!(source.contains("pending_profile_delete"));
    assert!(source.contains("Delete profile?"));
    assert!(source.contains(concat!("egui::Window", "::new(\"Delete ", "profile?\")")));
    assert!(!source.contains(concat!("ui.", "group(|ui|")));
    assert!(source.contains(".delete("));
}

#[test]
fn profile_editor_state_is_stored_on_dialog() {
    let source = whole();
    let draft = DRAFT_SOURCE
        .split("struct Draft")
        .nth(1)
        .expect("Draft should exist")
        .split("impl Default for Draft")
        .next()
        .expect("Draft fields should precede default impl");
    let dialog = MOD_SOURCE
        .split("pub struct VehicleDialog")
        .nth(1)
        .expect("VehicleDialog should exist")
        .split("impl Default for VehicleDialog")
        .next()
        .expect("VehicleDialog fields should precede default impl");

    assert!(draft.contains("selected_profile: Option<String>"));
    assert!(!draft.contains("save_profile_name: String"));
    assert!(!dialog.contains("selected_profile: Option<String>"));
    assert!(dialog.contains("profile_editor_selected: Option<String>"));
    assert!(dialog.contains("profile_editor_name: String"));
    assert!(dialog.contains("profile_editor_draft: ProfileDraft"));
    assert!(!source.contains(concat!("Add a vehicle before ", "creating a profile.")));
    assert!(!source.contains(concat!("profile_editor", "_json")));
}

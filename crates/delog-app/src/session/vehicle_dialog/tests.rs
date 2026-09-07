use super::draft::{OriMode, PosMode, general_summary, orientation_summary, position_summary};
use super::profiles::unique_profile_name;
use super::*;
use crate::scene3d::vehicle::ModelKind;
use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;

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
fn vehicle_actions_live_in_one_menu_reachable_from_the_header_and_the_list() {
    let menu = VEHICLES_TAB_SOURCE
        .split("fn action_menu")
        .nth(1)
        .expect("a single shared action menu should exist")
        .split("\nfn ")
        .next()
        .expect("the action menu should be one function");

    assert!(menu.contains("ProfileAction::SaveAs"));

    let mut previous = 0;
    for item in [
        "\"Duplicate Vehicle\"",
        "\"Save As Profile\"",
        "\"Remove Vehicle\"",
    ] {
        let at = menu
            .find(item)
            .unwrap_or_else(|| panic!("{item} should be in the menu"));
        assert!(
            at >= previous,
            "{item} is out of order; the destructive action belongs last"
        );
        previous = at;
    }

    let header = VEHICLES_TAB_SOURCE
        .split("fn show_detail")
        .nth(1)
        .expect("the detail pane should exist");
    assert!(header.contains("menu_image_button(icon(ui, crate::ui::icons::ellipsis())"));
    assert!(header.contains("egui::Layout::right_to_left(egui::Align::Center)"));
    assert!(header.contains("action_menu(ui, index, pending);"));

    let rail = VEHICLES_TAB_SOURCE
        .split("fn show_rail")
        .nth(1)
        .expect("the vehicle rail should exist");
    assert!(rail.contains(concat!("response.context", "_menu(|ui| {")));
    assert!(rail.contains("action_menu(ui, i, pending);"));
}

#[test]
fn both_tabs_use_a_list_rail_beside_a_detail_pane() {
    for (name, source) in [
        ("vehicles", VEHICLES_TAB_SOURCE),
        ("profiles", PROFILES_TAB_SOURCE),
    ] {
        assert!(
            source.contains("egui::Panel::left(") && source.contains(".show_inside(ui,"),
            "the {name} tab should put its list in a resizable left panel"
        );
        assert!(
            source.contains("egui::CentralPanel::default().show_inside(ui,"),
            "the {name} tab should render its editor in the remaining space"
        );
        assert!(
            source.contains("fn show_rail"),
            "the {name} tab should build its list rail in one place"
        );
    }
}

#[test]
fn the_detail_pane_explains_why_a_vehicle_is_not_rendered() {
    assert!(VEHICLES_TAB_SOURCE.contains("status_banner(ui, &draft.missing());"));
    assert!(VEHICLES_TAB_SOURCE.contains("\"incomplete\""));
    assert!(WIDGETS_SOURCE.contains("Not rendered yet - set"));
}

#[test]
fn form_rows_replace_hand_written_grid_triples() {
    for (name, source) in [
        ("vehicles", VEHICLES_TAB_SOURCE),
        ("profiles", PROFILES_TAB_SOURCE),
    ] {
        assert!(
            !source.contains("ui.end_row();"),
            "the {name} tab should lay rows out with form_row, not raw end_row calls"
        );
        assert!(
            source.contains("form_row(ui,"),
            "the {name} tab should use the shared form row helper"
        );
    }
}

#[test]
fn saving_a_vehicle_as_a_profile_never_overwrites_an_existing_name() {
    let existing = vec!["Vehicle".to_owned(), "Vehicle 2".to_owned()];

    assert_eq!(unique_profile_name(&existing, "Vehicle"), "Vehicle 3");
    assert_eq!(unique_profile_name(&existing, "  Rover "), "Rover");
    assert_eq!(unique_profile_name(&existing, "   "), "Vehicle 3");
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

#[test]
fn profile_draft_round_trips_through_a_document() {
    let draft = ProfileDraft::default();
    let doc = draft.to_doc("round-trip").expect("default draft is valid");
    let restored = ProfileDraft::from_doc(&doc);

    assert_eq!(restored.pos_topic, draft.pos_topic);
    assert_eq!(restored.lat, draft.lat);
    assert_eq!(restored.lat_lon_dege7, draft.lat_lon_dege7);
    assert_eq!(restored.alt_mm, draft.alt_mm);
    assert_eq!(restored.scale, draft.scale);
}

fn mapped_gps_draft() -> Draft {
    Draft {
        source: Some(SourceId(0)),
        pos_mode: PosMode::Gps,
        pos_topic: Some(TopicId(0)),
        lat: Some(FieldId(0)),
        lon: Some(FieldId(1)),
        alt: Some(FieldId(2)),
        ..Draft::default()
    }
}

#[test]
fn a_draft_reports_every_mapping_that_blocks_rendering() {
    assert_eq!(Draft::default().missing(), vec!["a data source"]);

    let mut draft = mapped_gps_draft();
    assert!(draft.missing().is_empty());

    draft.lon = None;
    assert_eq!(draft.missing(), vec!["Longitude"]);

    draft.pos_topic = None;
    draft.lat = None;
    draft.alt = None;
    assert_eq!(draft.missing(), vec!["a position topic"]);
}

#[test]
fn missing_mappings_agree_with_whether_the_vehicle_builds() {
    let mut cases = vec![Draft::default(), mapped_gps_draft()];

    let mut no_orientation_topic = mapped_gps_draft();
    no_orientation_topic.ori_mode = OriMode::Euler;
    cases.push(no_orientation_topic);

    let mut partial_euler = mapped_gps_draft();
    partial_euler.ori_mode = OriMode::Euler;
    partial_euler.ori_topic = Some(TopicId(1));
    partial_euler.roll = Some(FieldId(3));
    cases.push(partial_euler);

    let mut full_quat = mapped_gps_draft();
    full_quat.ori_mode = OriMode::Quat;
    full_quat.ori_topic = Some(TopicId(1));
    full_quat.qw = Some(FieldId(3));
    full_quat.qx = Some(FieldId(4));
    full_quat.qy = Some(FieldId(5));
    full_quat.qz = Some(FieldId(6));
    cases.push(full_quat);

    let mut ned = mapped_gps_draft();
    ned.pos_mode = PosMode::Ned;
    cases.push(ned);

    for (index, draft) in cases.iter().enumerate() {
        assert_eq!(
            draft.missing().is_empty(),
            draft.build().is_some(),
            "case {index} disagrees about whether the vehicle renders"
        );
    }
}

#[test]
fn section_summaries_describe_a_collapsed_vehicle() {
    let mut draft = mapped_gps_draft();
    draft.lat_lon_dege7 = true;
    draft.alt_mm = true;

    assert_eq!(
        position_summary(&draft, Some("GLOBAL_POSITION_INT")),
        "Global GPS \u{b7} GLOBAL_POSITION_INT \u{b7} degE7 \u{b7} mm"
    );

    draft.pos_mode = PosMode::Ned;
    draft.ned_has_ref = true;
    assert_eq!(
        position_summary(&draft, Some("LOCAL_POSITION_NED")),
        "Local NED \u{b7} LOCAL_POSITION_NED \u{b7} georeferenced"
    );

    assert_eq!(orientation_summary(&draft, None), "Static");

    draft.ori_mode = OriMode::Euler;
    draft.euler_degrees = true;
    assert_eq!(
        orientation_summary(&draft, Some("ATTITUDE")),
        "Euler \u{b7} ATTITUDE \u{b7} degrees"
    );

    draft.ori_mode = OriMode::Quat;
    assert_eq!(
        orientation_summary(&draft, Some("ATTITUDE_QUATERNION")),
        "Quaternion \u{b7} ATTITUDE_QUATERNION"
    );

    assert_eq!(
        general_summary(&draft, Some("flight.bin")),
        "flight.bin \u{b7} Fixed-wing"
    );
    assert_eq!(general_summary(&draft, None), "No source \u{b7} Fixed-wing");
}

fn test_snapshot() -> StoreSnapshot {
    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;
    use std::sync::Arc;

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight.bin");
    let topic = identity.add_topic(source, "GLOBAL_POSITION_INT").unwrap();
    for name in ["lat", "lon", "alt"] {
        identity.add_field(topic, name).unwrap();
    }
    let schema = Arc::new(
        TopicSchema::new(
            "GLOBAL_POSITION_INT",
            ["lat", "lon", "alt"]
                .map(|name| FieldSchema::new(name, DataType::Float64, None::<&str>, 1.0).unwrap()),
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = (0..3)
        .map(|_| Arc::new(Float64Array::from(vec![1.0, 2.0])) as ArrayRef)
        .collect();
    let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), columns, &schema).unwrap());
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    StoreSnapshot::from_registry(&identity, [(topic, store)], 1).unwrap()
}

fn headless_frame(ctx: &egui::Context, mut draw: impl FnMut(&mut egui::Ui)) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run_ui(input, &mut draw);
}

#[test]
fn the_dialog_lays_out_headlessly_with_complete_and_incomplete_vehicles() {
    let snapshot = test_snapshot();
    let mut vehicles = vec![crate::scene3d::vehicle::VehicleConfig {
        source: SourceId(0),
        label: "Quadcopter".into(),
        show: true,
        show_path: true,
        pos: crate::scene3d::vehicle::PosMapping::Gps {
            lat: FieldId(0),
            lon: FieldId(1),
            alt: FieldId(2),
            lat_lon_dege7: true,
            alt_mm: true,
            alt_offset_m: 0.0,
        },
        ori: crate::scene3d::vehicle::OriMapping::Static,
        model: ModelKind::Quad,
        color: egui::Color32::WHITE,
        path_color: egui::Color32::WHITE,
        scale: 1.0,
    }];

    let ctx = egui::Context::default();
    let mut dialog = VehicleDialog {
        open: true,
        ..VehicleDialog::default()
    };

    headless_frame(&ctx, |ui| {
        show(ui.ctx(), &mut dialog, &mut vehicles, &snapshot);
    });
    assert_eq!(dialog.drafts.len(), 1);
    assert!(dialog.drafts[0].missing().is_empty());

    dialog.drafts.push(Draft::default());
    dialog.selected_vehicle = 1;
    for _ in 0..2 {
        headless_frame(&ctx, |ui| {
            show(ui.ctx(), &mut dialog, &mut vehicles, &snapshot);
        });
    }
    assert_eq!(dialog.drafts[1].missing(), vec!["a data source"]);

    dialog.drafts.clear();
    dialog.clamp_selection();
    headless_frame(&ctx, |ui| {
        show(ui.ctx(), &mut dialog, &mut vehicles, &snapshot);
    });
    assert!(vehicles.is_empty());
}

#[test]
fn the_profiles_tab_lays_out_headlessly_for_a_new_and_a_saved_profile() {
    let snapshot = test_snapshot();
    let ctx = egui::Context::default();
    let mut dialog = VehicleDialog {
        profiles: vec!["arducopter".to_owned(), "px4-quad".to_owned()],
        ..VehicleDialog::default()
    };

    for selected in [None, Some("arducopter".to_owned())] {
        dialog.profile_editor_selected = selected;
        headless_frame(&ctx, |ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                super::profiles_tab::show_profiles_tab(ui, &mut dialog, &snapshot);
            });
        });
    }
    assert_eq!(dialog.profiles.len(), 2);
}

fn combo_list_area_height(ctx: &egui::Context, items: &[(TopicId, String)], filter: &str) -> f32 {
    let area_id = egui::Id::new("combo-list-probe");
    let filter_id = egui::Id::new("combo-list-filter");
    let highlight_id = egui::Id::new("combo-list-highlight");
    let mut sel = Some(TopicId(0));
    ctx.memory_mut(|m| m.data.insert_temp(filter_id, filter.to_owned()));
    for _ in 0..4 {
        headless_frame(ctx, |ui| {
            egui::Area::new(area_id).show(ui.ctx(), |ui| {
                ui.set_min_width(240.0);
                super::widgets::combo_list(ui, filter_id, highlight_id, &mut sel, items);
            });
        });
    }
    egui::AreaState::load(ctx, area_id)
        .and_then(|state| state.size)
        .map(|size| size.y)
        .expect("the probe area should have a size")
}

#[test]
fn the_searchable_list_grows_back_after_a_narrowing_filter_is_cleared() {
    let items: Vec<(TopicId, String)> = (0..30)
        .map(|i| (TopicId(i), format!("TOPIC_{i:02}")))
        .collect();
    let ctx = egui::Context::default();

    let unfiltered = combo_list_area_height(&ctx, &items, "");
    assert!(
        unfiltered > 100.0,
        "30 items should open a tall list, got {unfiltered}"
    );

    let narrowed = combo_list_area_height(&ctx, &items, "TOPIC_07");
    assert!(
        narrowed < unfiltered,
        "a one-result filter should shrink the list ({narrowed} vs {unfiltered})"
    );

    let cleared = combo_list_area_height(&ctx, &items, "");
    assert_eq!(
        cleared, unfiltered,
        "clearing the filter must restore the full list height"
    );
}

#[test]
fn the_searchable_combo_popup_matches_the_button_width() {
    let combo = WIDGETS_SOURCE
        .split("fn searchable_combo")
        .nth(1)
        .expect("searchable combo should exist");

    assert!(
        combo.contains("let popup_width = button.rect.width();")
            && combo.contains(".width(popup_width)")
            && combo.contains("ui.set_min_width(popup_width);"),
        "the topic popup should be as wide as its button, like every other dropdown"
    );
    assert!(
        !combo.contains("170.0"),
        "the topic popup should not pin itself to a hardcoded width"
    );
}

#[test]
fn the_searchable_list_is_laid_out_below_the_search_box() {
    let items: Vec<(TopicId, String)> = (0..30)
        .map(|i| (TopicId(i), format!("TOPIC_{i:02}")))
        .collect();
    let ctx = egui::Context::default();
    let filter_id = egui::Id::new("combo-list-filter");

    combo_list_area_height(&ctx, &items, "");

    let search = ctx
        .read_response(filter_id.with("edit"))
        .expect("the search box should be laid out")
        .rect;
    let area = egui::AreaState::load(&ctx, egui::Id::new("combo-list-probe"))
        .expect("the probe area should exist")
        .rect();

    assert!(
        area.bottom() >= search.bottom() + super::widgets::LIST_MAX_HEIGHT,
        "the list must sit below the search box, not on top of it \
         (area bottom {}, search bottom {}, list height {})",
        area.bottom(),
        search.bottom(),
        super::widgets::LIST_MAX_HEIGHT
    );
}

fn combo_list_frames(
    ctx: &egui::Context,
    items: &[(TopicId, String)],
    sel: &mut Option<TopicId>,
    filter_id: egui::Id,
    events: Vec<egui::Event>,
    frames: usize,
) {
    let highlight_id = egui::Id::new("combo-list-highlight");
    for frame in 0..frames {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events: if frame + 1 == frames {
                events.clone()
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            egui::Area::new(egui::Id::new("combo-list-probe")).show(ui.ctx(), |ui| {
                ui.set_min_width(240.0);
                super::widgets::combo_list(ui, filter_id, highlight_id, sel, items);
            });
        });
    }
}

#[test]
fn choosing_a_topic_clears_the_search_box_for_the_next_time() {
    let items: Vec<(TopicId, String)> = (0..30)
        .map(|i| (TopicId(i), format!("TOPIC_{i:02}")))
        .collect();
    let ctx = egui::Context::default();
    let filter_id = egui::Id::new("combo-list-filter");
    let mut sel = None;

    ctx.memory_mut(|m| m.data.insert_temp(filter_id, "TOPIC_07".to_owned()));
    combo_list_frames(&ctx, &items, &mut sel, filter_id, Vec::new(), 3);
    assert_eq!(
        ctx.memory(|m| m.data.get_temp::<String>(filter_id)),
        Some("TOPIC_07".to_owned()),
        "the filter should survive while the popup is open"
    );

    let enter = vec![egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }];
    combo_list_frames(&ctx, &items, &mut sel, filter_id, enter, 1);

    assert_eq!(sel, Some(TopicId(7)), "Enter should choose the only match");
    assert_eq!(
        ctx.memory(|m| m.data.get_temp::<String>(filter_id))
            .unwrap_or_default(),
        "",
        "the search box must be empty the next time the dropdown opens"
    );
}

#[test]
fn clicking_a_topic_also_clears_the_search_box() {
    let items: Vec<(TopicId, String)> = (0..30)
        .map(|i| (TopicId(i), format!("TOPIC_{i:02}")))
        .collect();
    let ctx = egui::Context::default();
    let filter_id = egui::Id::new("combo-list-filter");
    let mut sel = None;

    ctx.memory_mut(|m| m.data.insert_temp(filter_id, "TOPIC_07".to_owned()));
    combo_list_frames(&ctx, &items, &mut sel, filter_id, Vec::new(), 3);

    let search = ctx
        .read_response(filter_id.with("edit"))
        .expect("the search box should be laid out")
        .rect;
    let row = egui::pos2(search.left() + 20.0, search.bottom() + 12.0);
    let click = vec![
        egui::Event::PointerMoved(row),
        egui::Event::PointerButton {
            pos: row,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos: row,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ];
    combo_list_frames(&ctx, &items, &mut sel, filter_id, click, 2);

    assert_eq!(
        sel,
        Some(TopicId(7)),
        "clicking the only match should choose it"
    );
    assert_eq!(
        ctx.memory(|m| m.data.get_temp::<String>(filter_id))
            .unwrap_or_default(),
        "",
        "the search box must be empty the next time the dropdown opens"
    );
}

#[test]
fn the_topic_button_left_aligns_its_text_like_every_other_dropdown() {
    const LABEL: &str = "GLOBAL_POSITION_INT";
    const WIDTH: f32 = 300.0;
    let topics: Vec<(TopicId, String)> = vec![(TopicId(0), LABEL.to_owned())];
    let fields: Vec<(FieldId, String)> = vec![(FieldId(0), LABEL.to_owned())];
    let ctx = egui::Context::default();
    let mut topic = Some(TopicId(0));
    let mut field = Some(FieldId(0));

    let mut topic_x = None;
    let mut field_x = None;
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::Area::new(egui::Id::new("topic-area"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    ui.set_min_width(WIDTH);
                    ui.set_max_width(WIDTH);
                    super::widgets::searchable_combo(ui, "align-topic", &mut topic, &topics);
                });
            egui::Area::new(egui::Id::new("field-area"))
                .fixed_pos(egui::pos2(500.0, 0.0))
                .show(ui.ctx(), |ui| {
                    ui.set_min_width(WIDTH);
                    ui.set_max_width(WIDTH);
                    super::widgets::field_combo(ui, "align-field", &mut field, &fields);
                });
        });
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape
                && text.galley.text() == LABEL
            {
                if text.pos.x < 500.0 {
                    topic_x = Some(text.pos.x);
                } else {
                    field_x = Some(text.pos.x - 500.0);
                }
            }
        }
    }

    let topic_x = topic_x.expect("the topic button should paint its label");
    let field_x = field_x.expect("the field combo should paint its label");
    assert!(
        (topic_x - field_x).abs() < 2.0,
        "the topic label should start where a normal dropdown's does \
         (topic {topic_x}, field {field_x}); centering it puts it near {}",
        WIDTH / 2.0
    );
}

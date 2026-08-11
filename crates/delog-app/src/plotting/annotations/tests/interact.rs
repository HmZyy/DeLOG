use super::*;
use crate::plotting::annotations::interact;

#[test]
fn grabbing_a_handle_selects_the_annotation() {
    let (mut layer, id) = layer_with_box();
    layer.selected = Some(id);
    let tf = unit_transform();
    assert!(interact::begin_grab(
        &mut layer,
        &tf,
        egui::pos2(20.0, 80.0)
    ));
    assert_eq!(layer.selected, Some(id));
    assert_eq!(layer.grab, Some(Grab::Handle { id, index: 0 }));
}

#[test]
fn an_unselected_annotations_handles_do_not_steal_the_grab() {
    let (mut layer, id) = layer_with_box();
    let tf = unit_transform();
    assert!(interact::begin_grab(
        &mut layer,
        &tf,
        egui::pos2(20.0, 80.0)
    ));
    assert_eq!(layer.selected, Some(id));
    assert!(matches!(layer.grab, Some(Grab::Body { .. })));
}

#[test]
fn grabbing_the_body_records_the_original_geometry() {
    let (mut layer, id) = layer_with_box();
    let tf = unit_transform();
    assert!(interact::begin_grab(
        &mut layer,
        &tf,
        egui::pos2(20.0, 50.0)
    ));
    match layer.grab {
        Some(Grab::Body {
            id: got, origin, ..
        }) => {
            assert_eq!(got, id);
            assert_eq!(origin, box_geom());
        }
        other => panic!("expected a body grab, got {other:?}"),
    }
}

#[test]
fn grabbing_empty_space_clears_the_selection_and_does_not_consume() {
    let (mut layer, id) = layer_with_box();
    layer.selected = Some(id);
    let tf = unit_transform();
    assert!(!interact::begin_grab(
        &mut layer,
        &tf,
        egui::pos2(50.0, 50.0)
    ));
    assert_eq!(layer.selected, None);
    assert_eq!(layer.grab, None);
}

#[test]
fn dragging_a_handle_moves_only_that_corner() {
    let (mut layer, id) = layer_with_box();
    layer.selected = Some(id);
    let tf = unit_transform();
    interact::begin_grab(&mut layer, &tf, egui::pos2(20.0, 80.0));
    interact::apply_grab(&mut layer, tf.to_data(egui::pos2(10.0, 90.0)));
    let Geometry::Rect { a, b } = layer.get(id).expect("exists").geom else {
        panic!("expected a rect");
    };
    assert!((a.y - 10.0).abs() < 0.5);
    assert!((b.y - 80.0).abs() < 0.5);
}

#[test]
fn dragging_the_body_translates_the_whole_shape() {
    let (mut layer, id) = layer_with_box();
    let tf = unit_transform();
    interact::begin_grab(&mut layer, &tf, egui::pos2(20.0, 50.0));
    interact::apply_grab(&mut layer, tf.to_data(egui::pos2(30.0, 50.0)));
    let Geometry::Rect { a, b } = layer.get(id).expect("exists").geom else {
        panic!("expected a rect");
    };
    assert_eq!(b.t_us - a.t_us, 60_000_000);
    assert!((a.t_us as f64 - 30_000_000.0).abs() < 1e6);
}

#[test]
fn body_drag_final_geometry_ignores_the_intermediate_path() {
    let (mut layer, id) = layer_with_box();
    let tf = unit_transform();
    interact::begin_grab(&mut layer, &tf, egui::pos2(20.0, 50.0));
    let from = match layer.grab {
        Some(Grab::Body { from, .. }) => from,
        other => panic!("expected a body grab, got {other:?}"),
    };
    for pos in [
        egui::pos2(25.0, 55.0),
        egui::pos2(60.0, 15.0),
        egui::pos2(5.0, 90.0),
    ] {
        interact::apply_grab(&mut layer, tf.to_data(pos));
    }
    let final_pos = egui::pos2(35.0, 45.0);
    interact::apply_grab(&mut layer, tf.to_data(final_pos));
    let Geometry::Rect { a, b } = layer.get(id).expect("exists").geom else {
        panic!("expected a rect");
    };
    assert_eq!(b.t_us - a.t_us, 60_000_000);
    assert!((b.y - a.y - 60.0).abs() < 1e-9);
    let at = tf.to_data(final_pos);
    let expected = box_geom().translated(at.t_us - from.t_us, at.y - from.y);
    assert_eq!(Geometry::Rect { a, b }, expected);
}

#[test]
fn apply_grab_without_a_grab_is_a_no_op() {
    let (mut layer, id) = layer_with_box();
    interact::apply_grab(&mut layer, DataPos { t_us: 0, y: 0.0 });
    assert_eq!(layer.get(id).expect("exists").geom, box_geom());
}

#[test]
fn delete_selected_removes_and_clears() {
    let (mut layer, id) = layer_with_box();
    layer.selected = Some(id);
    interact::delete_selected(&mut layer);
    assert!(layer.items().is_empty());
    assert_eq!(layer.selected, None);
}

#[test]
fn delete_selected_without_a_selection_is_a_no_op() {
    let (mut layer, _) = layer_with_box();
    interact::delete_selected(&mut layer);
    assert_eq!(layer.items().len(), 1);
}

#[test]
fn a_single_click_on_a_shape_selects_it_without_opening_the_editor() {
    let (mut layer, id) = layer_with_box();
    let tf = unit_transform();
    let consumed = interact::on_click(&mut layer, &tf, egui::pos2(20.0, 50.0), false);
    assert!(!consumed);
    assert_eq!(layer.selected, Some(id));
    assert_eq!(layer.editing, None);
}

#[test]
fn a_double_click_on_a_shape_selects_it_opens_the_editor_and_is_consumed() {
    let (mut layer, id) = layer_with_box();
    let tf = unit_transform();
    let consumed = interact::on_click(&mut layer, &tf, egui::pos2(20.0, 50.0), true);
    assert!(consumed);
    assert_eq!(layer.selected, Some(id));
    assert_eq!(layer.editing, Some(id));
}

#[test]
fn a_single_click_on_empty_space_clears_the_selection_and_is_not_consumed() {
    let (mut layer, id) = layer_with_box();
    layer.selected = Some(id);
    let tf = unit_transform();
    let consumed = interact::on_click(&mut layer, &tf, egui::pos2(50.0, 50.0), false);
    assert!(!consumed);
    assert_eq!(layer.selected, None);
}

#[test]
fn a_double_click_on_empty_space_is_not_consumed_so_the_view_reset_still_happens() {
    let (mut layer, id) = layer_with_box();
    layer.selected = Some(id);
    let tf = unit_transform();
    let consumed = interact::on_click(&mut layer, &tf, egui::pos2(50.0, 50.0), true);
    assert!(!consumed);
    assert_eq!(layer.selected, None);
    assert_eq!(layer.editing, None);
}

use crate::plotting::annotations::place::ArmedTool;

const IPANE: u64 = 7;
const ISPAN_US: i64 = 100_000_000;
const IY_SPAN: f64 = 100.0;

#[test]
fn an_armed_one_click_tool_consumes_the_click_and_creates_a_shape() {
    let mut layer = AnnotationLayer::default();
    let mut armed = Some(ArmedTool::new(Kind::HLine));
    let consumed = interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos { t_us: 0, y: 40.0 },
        ISPAN_US,
        IY_SPAN,
    );
    assert!(consumed);
    assert_eq!(layer.items().len(), 1);
    assert_eq!(armed, None, "the tool disarms once a shape completes");
    assert_eq!(layer.items()[0].geom, Geometry::HLine { y: 40.0 });
}

#[test]
fn an_armed_two_click_tool_consumes_both_clicks_and_creates_one_shape() {
    let mut layer = AnnotationLayer::default();
    let mut armed = Some(ArmedTool::new(Kind::Rect));
    let first = interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos {
            t_us: 10_000_000,
            y: 10.0,
        },
        ISPAN_US,
        IY_SPAN,
    );
    assert!(first);
    assert!(
        layer.items().is_empty(),
        "nothing is created until the second click"
    );
    assert!(armed.is_some_and(|t| t.pending.is_some()));

    let second = interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos {
            t_us: 40_000_000,
            y: 60.0,
        },
        ISPAN_US,
        IY_SPAN,
    );
    assert!(second);
    assert_eq!(layer.items().len(), 1);
    assert_eq!(armed, None);
}

#[test]
fn a_completed_placement_selects_the_new_annotation() {
    let mut layer = AnnotationLayer::default();
    let mut armed = Some(ArmedTool::new(Kind::HLine));
    interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos { t_us: 0, y: 5.0 },
        ISPAN_US,
        IY_SPAN,
    );
    let id = layer.items()[0].id;
    assert_eq!(layer.selected, Some(id));
}

#[test]
fn placing_text_opens_its_editor() {
    let mut layer = AnnotationLayer::default();
    let mut armed = Some(ArmedTool::new(Kind::Text));
    interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos { t_us: 0, y: 5.0 },
        ISPAN_US,
        IY_SPAN,
    );
    assert_eq!(layer.editing, Some(layer.items()[0].id));
}

#[test]
fn no_armed_tool_means_the_click_is_not_consumed_for_placement() {
    let mut layer = AnnotationLayer::default();
    let mut armed = None;
    let consumed = interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos { t_us: 0, y: 5.0 },
        ISPAN_US,
        IY_SPAN,
    );
    assert!(!consumed);
    assert!(layer.items().is_empty());
}

#[test]
fn cancel_clears_a_pending_anchor_before_disarming() {
    let mut armed = Some(ArmedTool::new(Kind::Rect));
    let mut layer = AnnotationLayer::default();
    interact::on_armed_click(
        &mut layer,
        &mut armed,
        IPANE,
        DataPos { t_us: 0, y: 5.0 },
        ISPAN_US,
        IY_SPAN,
    );
    assert!(
        interact::cancel_armed(&mut armed),
        "first cancel clears the anchor"
    );
    assert!(armed.is_some_and(|t| t.pending.is_none()));
    assert!(interact::cancel_armed(&mut armed), "second cancel disarms");
    assert_eq!(armed, None);
    assert!(
        !interact::cancel_armed(&mut armed),
        "nothing left to cancel"
    );
}

fn armed_test_view() -> PaneView {
    PaneView {
        rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
        x_range: (0.0, 100.0),
        y_range: (0.0, 100.0),
    }
}

fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn drive_interact(
    ctx: &egui::Context,
    view: PaneView,
    layer: &mut AnnotationLayer,
    armed: &mut Option<ArmedTool>,
    pane_key: u64,
    events: Vec<egui::Event>,
) -> bool {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(200.0, 200.0),
        )),
        events,
        ..Default::default()
    };
    let mut consumed = false;
    let _ = ctx.run_ui(input, |ui| {
        let response = ui.interact(
            view.rect,
            egui::Id::new("interact_test_pane"),
            egui::Sense::click_and_drag(),
        );
        consumed = interact::interact(ui, &response, view, 0, layer, armed, pane_key);
    });
    consumed
}

fn prime_interact(ctx: &egui::Context, view: PaneView) {
    for _ in 0..2 {
        let mut layer = AnnotationLayer::default();
        let mut armed = None;
        drive_interact(ctx, view, &mut layer, &mut armed, IPANE, vec![]);
    }
}

#[test]
fn an_unarmed_drag_over_empty_space_is_not_consumed_so_panning_still_runs() {
    let ctx = egui::Context::default();
    let view = armed_test_view();
    prime_interact(&ctx, view);
    let mut layer = AnnotationLayer::default();
    let mut armed = None;
    let start = egui::pos2(50.0, 50.0);

    drive_interact(
        &ctx,
        view,
        &mut layer,
        &mut armed,
        IPANE,
        vec![pointer_button(start, true)],
    );
    let consumed = drive_interact(
        &ctx,
        view,
        &mut layer,
        &mut armed,
        IPANE,
        vec![egui::Event::PointerMoved(start + egui::vec2(20.0, 0.0))],
    );

    assert!(!consumed, "a drag over empty space must not be consumed");
    assert_eq!(layer.grab, None);
}

#[test]
fn an_armed_click_inside_the_plot_is_consumed_and_creates_the_annotation() {
    let ctx = egui::Context::default();
    let view = armed_test_view();
    prime_interact(&ctx, view);
    let mut layer = AnnotationLayer::default();
    let mut armed = Some(ArmedTool::new(Kind::HLine));
    let pos = egui::pos2(50.0, 50.0);

    drive_interact(
        &ctx,
        view,
        &mut layer,
        &mut armed,
        IPANE,
        vec![pointer_button(pos, true)],
    );
    let consumed = drive_interact(
        &ctx,
        view,
        &mut layer,
        &mut armed,
        IPANE,
        vec![pointer_button(pos, false)],
    );

    assert!(consumed);
    assert_eq!(layer.items().len(), 1);
    assert_eq!(armed, None);
}

#[test]
fn an_armed_drag_over_an_existing_annotation_does_not_move_it_and_is_not_consumed() {
    let ctx = egui::Context::default();
    let view = armed_test_view();
    prime_interact(&ctx, view);
    let (mut layer, id) = layer_with_box();
    let mut armed = Some(ArmedTool::new(Kind::Rect));
    let start = egui::pos2(20.0, 50.0);

    drive_interact(
        &ctx,
        view,
        &mut layer,
        &mut armed,
        IPANE,
        vec![pointer_button(start, true)],
    );
    let mut drag_consumed = Vec::new();
    for offset in [10.0, 15.0, 20.0] {
        let pos = start + egui::vec2(0.0, offset);
        drag_consumed.push(drive_interact(
            &ctx,
            view,
            &mut layer,
            &mut armed,
            IPANE,
            vec![egui::Event::PointerMoved(pos)],
        ));
    }

    assert!(
        !drag_consumed.iter().any(|&c| c),
        "an armed drag over an existing annotation must not be consumed, so panning still runs"
    );
    assert_eq!(
        layer.grab, None,
        "an armed drag must never grab an existing annotation"
    );
    assert_eq!(
        layer.get(id).expect("exists").geom,
        box_geom(),
        "the existing annotation must not move while a tool is armed"
    );
}

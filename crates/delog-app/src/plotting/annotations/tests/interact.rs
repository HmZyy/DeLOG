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

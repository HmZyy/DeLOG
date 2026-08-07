use super::*;
use crate::plotting::annotations::edit;

#[test]
fn creating_text_selects_it_and_opens_the_editor() {
    let mut layer = AnnotationLayer::default();
    let id = edit::create_at(
        &mut layer,
        Kind::Text,
        DataPos { t_us: 5, y: 1.0 },
        1_000_000,
        10.0,
    );
    assert_eq!(layer.selected, Some(id));
    assert_eq!(layer.editing, Some(id));
}

#[test]
fn creating_a_shape_selects_it_without_opening_the_editor() {
    let mut layer = AnnotationLayer::default();
    let id = edit::create_at(
        &mut layer,
        Kind::Rect,
        DataPos { t_us: 5, y: 1.0 },
        1_000_000,
        10.0,
    );
    assert_eq!(layer.selected, Some(id));
    assert_eq!(layer.editing, None);
}

#[test]
fn creation_places_the_shape_at_the_requested_anchor() {
    let mut layer = AnnotationLayer::default();
    let id = edit::create_at(
        &mut layer,
        Kind::HLine,
        DataPos { t_us: 5, y: 120.0 },
        1_000_000,
        10.0,
    );
    assert_eq!(
        layer.get(id).expect("exists").geom,
        Geometry::HLine { y: 120.0 }
    );
}

#[test]
fn closing_the_editor_on_an_empty_text_removes_it() {
    let mut layer = AnnotationLayer::default();
    let id = edit::create_at(
        &mut layer,
        Kind::Text,
        DataPos { t_us: 0, y: 0.0 },
        1_000_000,
        10.0,
    );
    edit::close_editor(&mut layer);
    assert!(layer.get(id).is_none());
    assert_eq!(layer.editing, None);
}

#[test]
fn closing_the_editor_keeps_a_labelled_text() {
    let mut layer = AnnotationLayer::default();
    let id = edit::create_at(
        &mut layer,
        Kind::Text,
        DataPos { t_us: 0, y: 0.0 },
        1_000_000,
        10.0,
    );
    layer.get_mut(id).expect("exists").label = "spike".to_string();
    edit::close_editor(&mut layer);
    assert!(layer.get(id).is_some());
    assert_eq!(layer.editing, None);
}

#[test]
fn closing_the_editor_keeps_an_unlabelled_shape() {
    let mut layer = AnnotationLayer::default();
    let id = edit::create_at(
        &mut layer,
        Kind::Rect,
        DataPos { t_us: 0, y: 0.0 },
        1_000_000,
        10.0,
    );
    layer.editing = Some(id);
    edit::close_editor(&mut layer);
    assert!(layer.get(id).is_some());
}

#[test]
fn creating_a_new_shape_sweeps_a_stale_empty_text_editor() {
    let mut layer = AnnotationLayer::default();
    let text_id = edit::create_at(
        &mut layer,
        Kind::Text,
        DataPos { t_us: 0, y: 0.0 },
        1_000_000,
        10.0,
    );
    let rect_id = edit::create_at(
        &mut layer,
        Kind::Rect,
        DataPos { t_us: 5, y: 1.0 },
        1_000_000,
        10.0,
    );
    assert!(layer.get(text_id).is_none());
    assert!(layer.get(rect_id).is_some());
    assert_eq!(layer.selected, Some(rect_id));
}

#[test]
fn creating_a_new_shape_does_not_sweep_a_labelled_stale_text() {
    let mut layer = AnnotationLayer::default();
    let text_id = edit::create_at(
        &mut layer,
        Kind::Text,
        DataPos { t_us: 0, y: 0.0 },
        1_000_000,
        10.0,
    );
    layer.get_mut(text_id).expect("exists").label = "spike".to_string();
    let rect_id = edit::create_at(
        &mut layer,
        Kind::Rect,
        DataPos { t_us: 5, y: 1.0 },
        1_000_000,
        10.0,
    );
    assert!(layer.get(text_id).is_some());
    assert!(layer.get(rect_id).is_some());
    assert_eq!(layer.selected, Some(rect_id));
}

#[test]
fn anchor_rows_cover_each_geometry() {
    let origin = 1_000_000;
    assert_eq!(edit::anchor_seconds(&Geometry::HLine { y: 12.0 }, origin).len(), 1);
    assert_eq!(
        edit::anchor_seconds(&Geometry::Text { at: DataPos { t_us: 0, y: 0.0 } }, origin).len(),
        2
    );
    assert_eq!(
        edit::anchor_seconds(
            &Geometry::Segment {
                from: DataPos { t_us: 0, y: 0.0 },
                to: DataPos { t_us: 1_000_000, y: 1.0 },
            },
            origin
        )
        .len(),
        4
    );
    assert_eq!(edit::anchor_seconds(&box_geom(), origin).len(), 4);
    assert_eq!(
        edit::anchor_seconds(
            &Geometry::Ellipse { a: DataPos { t_us: 0, y: 0.0 }, b: DataPos { t_us: 1_000_000, y: 1.0 } },
            origin
        )
        .len(),
        4
    );
}

#[test]
fn anchor_seconds_are_relative_to_the_origin() {
    let rows = edit::anchor_seconds(
        &Geometry::Text { at: DataPos { t_us: 3_500_000, y: 7.5 } },
        1_500_000,
    );
    assert!((rows[0].1 - 2.0).abs() < 1e-9);
    assert!((rows[1].1 - 7.5).abs() < 1e-9);
}

#[test]
fn setting_an_anchor_second_writes_absolute_time() {
    let mut geom = Geometry::Text { at: DataPos { t_us: 0, y: 0.0 } };
    edit::set_anchor_seconds(&mut geom, 0, 2.5, 1_000_000);
    assert_eq!(geom, Geometry::Text { at: DataPos { t_us: 3_500_000, y: 0.0 } });
}

#[test]
fn setting_an_anchor_value_writes_the_y_field() {
    let mut geom = Geometry::HLine { y: 0.0 };
    edit::set_anchor_seconds(&mut geom, 0, 120.0, 0);
    assert_eq!(geom, Geometry::HLine { y: 120.0 });
}

#[test]
fn setting_an_out_of_range_anchor_is_ignored() {
    let geom = Geometry::HLine { y: 5.0 };
    let mut moved = geom;
    edit::set_anchor_seconds(&mut moved, 9, 1.0, 0);
    assert_eq!(moved, geom);
}

#[test]
fn rect_anchor_rows_round_trip_through_their_setters() {
    let origin = 500_000;
    let mut geom = box_geom();
    let rows = edit::anchor_seconds(&geom, origin);
    for (index, (_, value)) in rows.iter().enumerate() {
        edit::set_anchor_seconds(&mut geom, index, *value, origin);
    }
    assert_eq!(geom, box_geom());
}

#[test]
fn segment_anchor_rows_round_trip_through_their_setters() {
    let origin = 500_000;
    let segment = Geometry::Segment {
        from: DataPos { t_us: 20_000_000, y: 20.0 },
        to: DataPos { t_us: 80_000_000, y: 80.0 },
    };
    let mut geom = segment;
    let rows = edit::anchor_seconds(&geom, origin);
    for (index, (_, value)) in rows.iter().enumerate() {
        edit::set_anchor_seconds(&mut geom, index, *value, origin);
    }
    assert_eq!(geom, segment);
}

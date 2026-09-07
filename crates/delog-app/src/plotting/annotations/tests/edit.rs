use super::*;
use crate::plotting::annotations::edit;

#[test]
fn closing_the_editor_on_an_empty_text_removes_it() {
    let mut layer = AnnotationLayer::default();
    let id = layer.add(Kind::Text, DataPos { t_us: 0, y: 0.0 }, 1_000_000, 10.0);
    layer.editing = Some(id);
    edit::close_editor(&mut layer);
    assert!(layer.get(id).is_none());
    assert_eq!(layer.editing, None);
}

#[test]
fn closing_the_editor_keeps_a_labelled_text() {
    let mut layer = AnnotationLayer::default();
    let id = layer.add(Kind::Text, DataPos { t_us: 0, y: 0.0 }, 1_000_000, 10.0);
    layer.editing = Some(id);
    layer.get_mut(id).expect("exists").label = "spike".to_string();
    edit::close_editor(&mut layer);
    assert!(layer.get(id).is_some());
    assert_eq!(layer.editing, None);
}

#[test]
fn closing_the_editor_keeps_an_unlabelled_shape() {
    let mut layer = AnnotationLayer::default();
    let id = layer.add(Kind::Rect, DataPos { t_us: 0, y: 0.0 }, 1_000_000, 10.0);
    layer.editing = Some(id);
    edit::close_editor(&mut layer);
    assert!(layer.get(id).is_some());
}

#[test]
fn anchor_rows_cover_each_geometry() {
    let origin = 1_000_000;
    assert_eq!(
        edit::anchor_seconds(&Geometry::HLine { y: 12.0 }, origin).len(),
        1
    );
    assert_eq!(
        edit::anchor_seconds(
            &Geometry::Text {
                at: DataPos { t_us: 0, y: 0.0 }
            },
            origin
        )
        .len(),
        2
    );
    assert_eq!(
        edit::anchor_seconds(
            &Geometry::Segment {
                from: DataPos { t_us: 0, y: 0.0 },
                to: DataPos {
                    t_us: 1_000_000,
                    y: 1.0
                },
            },
            origin
        )
        .len(),
        4
    );
    assert_eq!(edit::anchor_seconds(&box_geom(), origin).len(), 4);
    assert_eq!(
        edit::anchor_seconds(
            &Geometry::Ellipse {
                a: DataPos { t_us: 0, y: 0.0 },
                b: DataPos {
                    t_us: 1_000_000,
                    y: 1.0
                }
            },
            origin
        )
        .len(),
        4
    );
}

#[test]
fn multi_point_anchor_rows_are_individually_named() {
    let origin = 1_000_000;
    let names: Vec<&str> = edit::anchor_seconds(&box_geom(), origin)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["t1", "y1", "t2", "y2"]);

    let segment = Geometry::Segment {
        from: DataPos { t_us: 0, y: 0.0 },
        to: DataPos {
            t_us: 1_000_000,
            y: 1.0,
        },
    };
    let names: Vec<&str> = edit::anchor_seconds(&segment, origin)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["t1", "y1", "t2", "y2"]);
}

#[test]
fn anchor_seconds_are_relative_to_the_origin() {
    let rows = edit::anchor_seconds(
        &Geometry::Text {
            at: DataPos {
                t_us: 3_500_000,
                y: 7.5,
            },
        },
        1_500_000,
    );
    assert!((rows[0].1 - 2.0).abs() < 1e-9);
    assert!((rows[1].1 - 7.5).abs() < 1e-9);
}

#[test]
fn setting_an_anchor_second_writes_absolute_time() {
    let mut geom = Geometry::Text {
        at: DataPos { t_us: 0, y: 0.0 },
    };
    edit::set_anchor_seconds(&mut geom, 0, 2.5, 1_000_000);
    assert_eq!(
        geom,
        Geometry::Text {
            at: DataPos {
                t_us: 3_500_000,
                y: 0.0
            }
        }
    );
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
        from: DataPos {
            t_us: 20_000_000,
            y: 20.0,
        },
        to: DataPos {
            t_us: 80_000_000,
            y: 80.0,
        },
    };
    let mut geom = segment;
    let rows = edit::anchor_seconds(&geom, origin);
    for (index, (_, value)) in rows.iter().enumerate() {
        edit::set_anchor_seconds(&mut geom, index, *value, origin);
    }
    assert_eq!(geom, segment);
}

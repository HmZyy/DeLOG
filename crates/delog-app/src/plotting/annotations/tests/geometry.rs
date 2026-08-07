use super::*;

#[test]
fn screen_round_trip_returns_the_same_pixel() {
    let tf = PlotTransform::new(view(), 1_000_000);
    for pos in [
        egui::pos2(100.0, 50.0),
        egui::pos2(350.0, 200.0),
        egui::pos2(600.0, 350.0),
        egui::pos2(137.0, 291.0),
    ] {
        let back = tf.to_screen(tf.to_data(pos));
        assert!(
            (back.x - pos.x).abs() < 0.01 && (back.y - pos.y).abs() < 0.01,
            "{pos:?} round-tripped to {back:?}"
        );
    }
}

#[test]
fn data_round_trip_stays_within_a_pixel() {
    let tf = PlotTransform::new(view(), 1_000_000);
    let us_per_px = (10.0 * 1e6) / 500.0;
    for p in [
        DataPos { t_us: 1_000_000, y: -5.0 },
        DataPos { t_us: 4_500_000, y: 3.25 },
        DataPos { t_us: 11_000_000, y: 15.0 },
    ] {
        let back = tf.to_data(tf.to_screen(p));
        assert!(
            ((back.t_us - p.t_us) as f64).abs() <= us_per_px,
            "{p:?} round-tripped to {back:?}"
        );
        assert!((back.y - p.y).abs() <= 20.0 / 300.0, "{p:?} round-tripped to {back:?}");
    }
}

#[test]
fn transform_maps_the_view_corners_to_the_rect_corners() {
    let tf = PlotTransform::new(view(), 0);
    let bottom_left = tf.to_screen(DataPos { t_us: 0, y: -5.0 });
    let top_right = tf.to_screen(DataPos { t_us: 10_000_000, y: 15.0 });
    assert!((bottom_left.x - 100.0).abs() < 0.01);
    assert!((bottom_left.y - 350.0).abs() < 0.01);
    assert!((top_right.x - 600.0).abs() < 0.01);
    assert!((top_right.y - 50.0).abs() < 0.01);
}

#[test]
fn degenerate_view_spans_do_not_divide_by_zero() {
    let flat = PaneView {
        rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
        x_range: (2.0, 2.0),
        y_range: (7.0, 7.0),
    };
    let tf = PlotTransform::new(flat, 0);
    let p = tf.to_screen(DataPos { t_us: 2_000_000, y: 7.0 });
    assert!(p.x.is_finite() && p.y.is_finite());
    let d = tf.to_data(egui::pos2(50.0, 50.0));
    assert!(d.y.is_finite());
}

#[test]
fn default_geometry_scales_to_the_visible_span() {
    let at = DataPos { t_us: 5_000_000, y: 2.0 };
    let geom = default_geometry(Kind::Rect, at, 10_000_000, 20.0);
    let Geometry::Rect { a, b } = geom else {
        panic!("expected a rect, got {geom:?}");
    };
    assert_eq!(b.t_us - a.t_us, 1_200_000);
    assert!((b.y - a.y - 3.0).abs() < 1e-9);
    assert_eq!((a.t_us + b.t_us) / 2, 5_000_000);
}

#[test]
fn default_geometry_stays_grabbable_on_a_degenerate_view() {
    let at = DataPos { t_us: 0, y: 0.0 };
    let Geometry::Rect { a, b } = default_geometry(Kind::Rect, at, 0, 0.0) else {
        panic!("expected a rect");
    };
    assert!(b.t_us > a.t_us);
    assert!(b.y > a.y);
}

#[test]
fn default_geometry_covers_every_kind() {
    let at = DataPos { t_us: 1_000, y: 1.0 };
    for kind in Kind::ALL {
        assert_eq!(default_geometry(kind, at, 1_000_000, 10.0).kind(), kind);
    }
}

#[test]
fn hline_default_takes_the_anchor_value() {
    let geom = default_geometry(Kind::HLine, DataPos { t_us: 42, y: 120.0 }, 1_000_000, 10.0);
    assert_eq!(geom, Geometry::HLine { y: 120.0 });
}

#[test]
fn body_translation_preserves_extents() {
    let geom = Geometry::Rect {
        a: DataPos { t_us: 100, y: 1.0 },
        b: DataPos { t_us: 300, y: 5.0 },
    };
    let moved = geom.translated(50, 2.0);
    let Geometry::Rect { a, b } = moved else {
        panic!("expected a rect");
    };
    assert_eq!((a.t_us, b.t_us), (150, 350));
    assert!((a.y - 3.0).abs() < 1e-9 && (b.y - 7.0).abs() < 1e-9);
}

#[test]
fn translation_is_computed_from_the_origin_not_accumulated() {
    let origin = Geometry::Segment {
        from: DataPos { t_us: 0, y: 0.0 },
        to: DataPos { t_us: 1_000, y: 1.0 },
    };
    let once = origin.translated(7_000, 0.5);
    assert_eq!(once, origin.translated(7_000, 0.5));
    let Geometry::Segment { from, to } = once else {
        panic!("expected a segment");
    };
    assert_eq!(from.t_us, 7_000);
    assert_eq!(to.t_us, 8_000);
    assert!((from.y - 0.5).abs() < 1e-12);
}

#[test]
fn hline_translation_ignores_the_time_delta() {
    let moved = Geometry::HLine { y: 10.0 }.translated(5_000, -2.0);
    assert_eq!(moved, Geometry::HLine { y: 8.0 });
}

#[test]
fn rect_handles_move_only_their_own_corner() {
    let mut geom = Geometry::Rect {
        a: DataPos { t_us: 0, y: 0.0 },
        b: DataPos { t_us: 100, y: 10.0 },
    };
    geom.set_handle(1, DataPos { t_us: 250, y: -4.0 });
    let Geometry::Rect { a, b } = geom else {
        panic!("expected a rect");
    };
    assert_eq!((a.t_us, b.t_us), (0, 250));
    assert!((a.y - -4.0).abs() < 1e-9);
    assert!((b.y - 10.0).abs() < 1e-9);
}

#[test]
fn handle_positions_match_the_handle_indices() {
    let geom = Geometry::Ellipse {
        a: DataPos { t_us: 0, y: 0.0 },
        b: DataPos { t_us: 100, y: 10.0 },
    };
    let positions = geom.handle_positions();
    assert_eq!(positions.len(), 4);
    for (index, expected) in positions.iter().enumerate() {
        let mut moved = geom;
        moved.set_handle(index, *expected);
        assert_eq!(moved, geom, "handle {index} moved the geometry when set to its own position");
    }
}

#[test]
fn text_and_hline_have_no_handles() {
    assert!(Geometry::Text { at: DataPos { t_us: 0, y: 0.0 } }.handle_positions().is_empty());
    assert!(Geometry::HLine { y: 0.0 }.handle_positions().is_empty());
}

#[test]
fn out_of_range_handle_index_is_ignored() {
    let geom = Geometry::HLine { y: 3.0 };
    let mut moved = geom;
    moved.set_handle(9, DataPos { t_us: 0, y: 99.0 });
    assert_eq!(moved, geom);
}

#[test]
fn ids_are_never_reused_after_removal() {
    let mut layer = AnnotationLayer::default();
    let at = DataPos { t_us: 0, y: 0.0 };
    let first = layer.add(Kind::Rect, at, 1_000_000, 10.0);
    layer.remove(first);
    let second = layer.add(Kind::Rect, at, 1_000_000, 10.0);
    assert_ne!(first, second);
    assert_eq!(layer.items().len(), 1);
}

#[test]
fn removing_the_selected_annotation_clears_selection_and_editor() {
    let mut layer = AnnotationLayer::default();
    let id = layer.add(Kind::Text, DataPos { t_us: 0, y: 0.0 }, 1_000_000, 10.0);
    layer.selected = Some(id);
    layer.editing = Some(id);
    layer.remove(id);
    assert!(layer.items().is_empty());
    assert_eq!(layer.selected, None);
    assert_eq!(layer.editing, None);
}

#[test]
fn removing_an_annotation_clears_a_grab_on_it() {
    let mut layer = AnnotationLayer::default();
    let id = layer.add(Kind::Rect, DataPos { t_us: 0, y: 0.0 }, 1_000_000, 10.0);
    layer.grab = Some(Grab::Handle { id, index: 0 });
    layer.remove(id);
    assert_eq!(layer.grab, None);
}

#[test]
fn added_annotations_take_distinct_palette_colors() {
    let mut layer = AnnotationLayer::default();
    let at = DataPos { t_us: 0, y: 0.0 };
    let first = layer.add(Kind::Rect, at, 1_000_000, 10.0);
    let second = layer.add(Kind::Rect, at, 1_000_000, 10.0);
    let a = layer.get(first).expect("first exists").style.color;
    let b = layer.get(second).expect("second exists").style.color;
    assert_ne!(a, b);
}

#[test]
fn default_style_is_outline_only_at_the_trace_stroke_width() {
    let style = default_style(0);
    assert_eq!(style.fill_opacity, 0.0);
    assert_eq!(style.stroke_px, 1.5);
    assert_eq!(style.font_px, 11.0);
    assert!(!style.arrow);
}

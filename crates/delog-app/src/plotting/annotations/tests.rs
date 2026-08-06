use super::*;

fn view() -> PaneView {
    PaneView {
        rect: egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(600.0, 350.0)),
        x_range: (0.0, 10.0),
        y_range: (-5.0, 15.0),
    }
}

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

fn unit_transform() -> PlotTransform {
    PlotTransform::new(
        PaneView {
            rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
            x_range: (0.0, 100.0),
            y_range: (0.0, 100.0),
        },
        0,
    )
}

fn annot(id: u64, geom: Geometry) -> Annotation {
    Annotation {
        id,
        geom,
        label: String::new(),
        style: default_style(id),
    }
}

fn filled(id: u64, geom: Geometry) -> Annotation {
    let mut a = annot(id, geom);
    a.style.fill_opacity = 0.5;
    a
}

fn box_geom() -> Geometry {
    Geometry::Rect {
        a: DataPos { t_us: 20_000_000, y: 20.0 },
        b: DataPos { t_us: 80_000_000, y: 80.0 },
    }
}

#[test]
fn rect_outline_hits_near_the_border_and_misses_the_interior() {
    let tf = unit_transform();
    let a = annot(0, box_geom());
    assert!(hit::contains(&a, &tf, egui::pos2(20.0, 50.0)));
    assert!(hit::contains(&a, &tf, egui::pos2(23.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(50.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(12.0, 50.0)));
}

#[test]
fn filled_rect_hits_its_interior() {
    let tf = unit_transform();
    assert!(hit::contains(&filled(0, box_geom()), &tf, egui::pos2(50.0, 50.0)));
}

#[test]
fn ellipse_outline_hits_near_the_rim_and_misses_the_centre() {
    let tf = unit_transform();
    let a = annot(0, Geometry::Ellipse {
        a: DataPos { t_us: 20_000_000, y: 20.0 },
        b: DataPos { t_us: 80_000_000, y: 80.0 },
    });
    assert!(hit::contains(&a, &tf, egui::pos2(20.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(50.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(5.0, 50.0)));
}

#[test]
fn segment_hits_along_its_length_only() {
    let tf = unit_transform();
    let a = annot(0, Geometry::Segment {
        from: DataPos { t_us: 0, y: 0.0 },
        to: DataPos { t_us: 100_000_000, y: 100.0 },
    });
    assert!(hit::contains(&a, &tf, egui::pos2(50.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(50.0, 80.0)));
}

#[test]
fn hline_hits_within_tolerance_across_the_pane() {
    let tf = unit_transform();
    let a = annot(0, Geometry::HLine { y: 40.0 });
    assert!(hit::contains(&a, &tf, egui::pos2(5.0, 60.0)));
    assert!(hit::contains(&a, &tf, egui::pos2(95.0, 60.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(50.0, 40.0)));
}

#[test]
fn text_hits_inside_its_approximate_galley() {
    let tf = unit_transform();
    let mut a = annot(0, Geometry::Text { at: DataPos { t_us: 10_000_000, y: 50.0 } });
    a.label = "abcd".to_string();
    a.style.font_px = 20.0;
    assert!(hit::contains(&a, &tf, egui::pos2(20.0, 45.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(90.0, 10.0)));
}

#[test]
fn empty_text_is_not_hittable() {
    let tf = unit_transform();
    let a = annot(0, Geometry::Text { at: DataPos { t_us: 10_000_000, y: 50.0 } });
    assert!(!hit::contains(&a, &tf, egui::pos2(10.0, 50.0)));
}

#[test]
fn topmost_prefers_the_most_recently_created_overlap() {
    let tf = unit_transform();
    let items = vec![filled(0, box_geom()), filled(7, box_geom())];
    assert_eq!(hit::topmost(&items, &tf, egui::pos2(50.0, 50.0)), Some(7));
}

#[test]
fn topmost_returns_none_on_empty_space() {
    let tf = unit_transform();
    let items = vec![annot(0, box_geom())];
    assert_eq!(hit::topmost(&items, &tf, egui::pos2(50.0, 50.0)), None);
}

#[test]
fn handles_are_returned_in_data_index_order() {
    let tf = unit_transform();
    let handles = hit::handles(&box_geom(), &tf);
    assert_eq!(handles.len(), 4);
    assert!((handles[0].x - 20.0).abs() < 0.01 && (handles[0].y - 80.0).abs() < 0.01);
    assert!((handles[2].x - 80.0).abs() < 0.01 && (handles[2].y - 20.0).abs() < 0.01);
}

#[test]
fn handle_at_finds_a_corner_and_prefers_the_top_annotation() {
    let tf = unit_transform();
    let items = vec![annot(0, box_geom()), annot(3, box_geom())];
    assert_eq!(hit::handle_at(&items, &tf, egui::pos2(20.0, 80.0)), Some((3, 0)));
    assert_eq!(hit::handle_at(&items, &tf, egui::pos2(50.0, 50.0)), None);
}

#[test]
fn handle_at_ignores_kinds_without_handles() {
    let tf = unit_transform();
    let items = vec![annot(0, Geometry::HLine { y: 40.0 })];
    assert_eq!(hit::handle_at(&items, &tf, egui::pos2(50.0, 60.0)), None);
}

#[test]
fn screen_rect_of_an_hline_spans_the_pane_width() {
    let tf = unit_transform();
    let r = hit::screen_rect(&Geometry::HLine { y: 40.0 }, &tf);
    assert!((r.left() - 0.0).abs() < 0.01);
    assert!((r.right() - 100.0).abs() < 0.01);
}

#[test]
fn geometry_inside_the_view_is_visible() {
    let tf = unit_transform();
    assert!(draw::is_visible(&annot(0, box_geom()), &tf));
}

#[test]
fn geometry_entirely_off_screen_is_skipped() {
    let tf = unit_transform();
    let far = Geometry::Rect {
        a: DataPos { t_us: 500_000_000, y: 500.0 },
        b: DataPos { t_us: 600_000_000, y: 600.0 },
    };
    assert!(!draw::is_visible(&annot(0, far), &tf));
}

#[test]
fn partially_visible_geometry_is_not_skipped() {
    let tf = unit_transform();
    let straddling = Geometry::Rect {
        a: DataPos { t_us: -50_000_000, y: -50.0 },
        b: DataPos { t_us: 30_000_000, y: 30.0 },
    };
    assert!(draw::is_visible(&annot(0, straddling), &tf));
}

#[test]
fn hline_outside_the_y_range_is_skipped() {
    let tf = unit_transform();
    assert!(draw::is_visible(&annot(0, Geometry::HLine { y: 50.0 }), &tf));
    assert!(!draw::is_visible(&annot(0, Geometry::HLine { y: 900.0 }), &tf));
}

#[test]
fn text_with_its_label_reaching_into_view_is_visible() {
    let tf = unit_transform();
    let mut a = annot(0, Geometry::Text { at: DataPos { t_us: -5_000_000, y: 50.0 } });
    a.label = "abcd".to_string();
    a.style.font_px = 20.0;
    assert!(draw::is_visible(&a, &tf));
}

#[test]
fn label_anchors_sit_on_the_expected_edge() {
    let tf = unit_transform();
    let (pos, align) = draw::label_anchor(&Geometry::HLine { y: 40.0 }, &tf);
    assert_eq!(align, egui::Align2::RIGHT_BOTTOM);
    assert!((pos.x - 97.0).abs() < 0.01);

    let (pos, align) = draw::label_anchor(&box_geom(), &tf);
    assert_eq!(align, egui::Align2::LEFT_BOTTOM);
    assert!((pos.x - 20.0).abs() < 0.01 && (pos.y - 20.0).abs() < 0.01);
}

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

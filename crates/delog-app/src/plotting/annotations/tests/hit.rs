use super::*;
use crate::plotting::annotations::hit;

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
    assert!(hit::contains(
        &filled(0, box_geom()),
        &tf,
        egui::pos2(50.0, 50.0)
    ));
}

#[test]
fn ellipse_outline_hits_near_the_rim_and_misses_the_centre() {
    let tf = unit_transform();
    let a = annot(
        0,
        Geometry::Ellipse {
            a: DataPos {
                t_us: 20_000_000,
                y: 20.0,
            },
            b: DataPos {
                t_us: 80_000_000,
                y: 80.0,
            },
        },
    );
    assert!(hit::contains(&a, &tf, egui::pos2(20.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(50.0, 50.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(5.0, 50.0)));
}

#[test]
fn segment_hits_along_its_length_only() {
    let tf = unit_transform();
    let a = annot(
        0,
        Geometry::Segment {
            from: DataPos { t_us: 0, y: 0.0 },
            to: DataPos {
                t_us: 100_000_000,
                y: 100.0,
            },
        },
    );
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
    let mut a = annot(
        0,
        Geometry::Text {
            at: DataPos {
                t_us: 10_000_000,
                y: 50.0,
            },
        },
    );
    a.label = "abcd".to_string();
    a.style.font_px = 20.0;
    assert!(hit::contains(&a, &tf, egui::pos2(20.0, 45.0)));
    assert!(!hit::contains(&a, &tf, egui::pos2(90.0, 10.0)));
}

#[test]
fn empty_text_is_not_hittable() {
    let tf = unit_transform();
    let a = annot(
        0,
        Geometry::Text {
            at: DataPos {
                t_us: 10_000_000,
                y: 50.0,
            },
        },
    );
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
    assert_eq!(
        hit::handle_at(&items, &tf, egui::pos2(20.0, 80.0)),
        Some((3, 0))
    );
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

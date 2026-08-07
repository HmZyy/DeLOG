use super::*;
use crate::plotting::annotations::draw;

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

fn clip_rect() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0))
}

#[test]
fn an_ordinary_ellipse_rect_is_returned_unchanged() {
    let clip = clip_rect();
    let rect = egui::Rect::from_min_max(egui::pos2(20.0, 20.0), egui::pos2(80.0, 80.0));
    assert_eq!(draw::clamped_ellipse(rect, clip), Some(rect));
}

#[test]
fn a_screen_rect_that_dwarfs_the_clip_rect_is_bounded() {
    let clip = clip_rect();
    let rect = egui::Rect::from_min_max(egui::pos2(-1.0e9, 40.0), egui::pos2(1.0e9, 60.0));
    let bounded = draw::clamped_ellipse(rect, clip).expect("still overlaps the clip rect");
    let bound = clip.expand2(clip.size() * 4.0);
    assert_eq!(
        bounded,
        egui::Rect::from_min_max(egui::pos2(bound.min.x, 40.0), egui::pos2(bound.max.x, 60.0))
    );
    assert!(bounded.width() <= bound.width() + 0.01);
    assert!(bounded.width() < 1_000.0, "bounded width was {}", bounded.width());
}

#[test]
fn an_ellipse_that_fully_encloses_the_clip_rect_has_no_rim_to_draw() {
    let clip = clip_rect();
    let rect = egui::Rect::from_center_size(egui::pos2(50.0, 50.0), egui::Vec2::splat(4_000.0));
    assert_eq!(draw::clamped_ellipse(rect, clip), None);
}

#[test]
fn an_enclosing_outline_only_ellipse_paints_nothing() {
    let clip = clip_rect();
    let rect = egui::Rect::from_center_size(egui::pos2(50.0, 50.0), egui::Vec2::splat(4_000.0));
    assert_eq!(draw::ellipse_paint(rect, clip, 0.0), draw::EllipsePaint::Skip);
}

#[test]
fn an_enclosing_filled_ellipse_still_paints_its_fill() {
    let clip = clip_rect();
    let rect = egui::Rect::from_center_size(egui::pos2(50.0, 50.0), egui::Vec2::splat(4_000.0));
    assert_eq!(draw::ellipse_paint(rect, clip, 0.5), draw::EllipsePaint::FillOnly);
}

#[test]
fn an_ordinary_ellipse_still_paints_its_shape() {
    let clip = clip_rect();
    let rect = egui::Rect::from_min_max(egui::pos2(20.0, 20.0), egui::pos2(80.0, 80.0));
    assert_eq!(draw::ellipse_paint(rect, clip, 0.5), draw::EllipsePaint::Shape(rect));
}

#[test]
fn an_off_screen_ellipse_is_not_visible() {
    let tf = unit_transform();
    let far = Geometry::Ellipse {
        a: DataPos { t_us: 500_000_000, y: 500.0 },
        b: DataPos { t_us: 600_000_000, y: 600.0 },
    };
    assert!(!draw::is_visible(&annot(0, far), &tf));
}

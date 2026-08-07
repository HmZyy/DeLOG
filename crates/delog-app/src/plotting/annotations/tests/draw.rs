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

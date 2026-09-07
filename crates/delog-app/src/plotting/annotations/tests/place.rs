use super::*;
use crate::plotting::annotations::hit;
use crate::plotting::annotations::place::{self, ArmedTool, Placed};

const PANE_A: u64 = 1;
const PANE_B: u64 = 2;
const SPAN_US: i64 = 10_000_000;
const Y_SPAN: f64 = 20.0;

fn pos(t_us: i64, y: f64) -> DataPos {
    DataPos { t_us, y }
}

fn click(tool: &mut ArmedTool, pane: u64, at: DataPos) -> Placed {
    place::on_plot_click(tool, pane, at, SPAN_US, Y_SPAN)
}

#[test]
fn text_completes_on_a_single_click() {
    let mut tool = ArmedTool::new(Kind::Text);
    let at = pos(5_000_000, 3.0);
    match click(&mut tool, PANE_A, at) {
        Placed::Complete(geom) => {
            assert_eq!(geom, default_geometry(Kind::Text, at, SPAN_US, Y_SPAN));
        }
        other => panic!("expected Complete, got {other:?}"),
    }
    assert_eq!(tool.pending, None);
}

#[test]
fn hline_completes_on_a_single_click() {
    let mut tool = ArmedTool::new(Kind::HLine);
    let at = pos(1_000, 120.0);
    match click(&mut tool, PANE_A, at) {
        Placed::Complete(geom) => assert_eq!(geom, Geometry::HLine { y: 120.0 }),
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn two_click_kinds_pend_then_complete() {
    for kind in [Kind::Segment, Kind::Rect, Kind::Ellipse] {
        let mut tool = ArmedTool::new(kind);
        let first = pos(1_000_000, 1.0);
        assert!(
            matches!(click(&mut tool, PANE_A, first), Placed::Pending),
            "{kind:?}"
        );
        assert_eq!(tool.pending, Some((PANE_A, first)), "{kind:?}");
        let second = pos(4_000_000, 9.0);
        match click(&mut tool, PANE_A, second) {
            Placed::Complete(geom) => assert_eq!(geom.kind(), kind),
            other => panic!("{kind:?}: expected Complete, got {other:?}"),
        }
        assert_eq!(tool.pending, None, "{kind:?}");
    }
}

#[test]
fn segment_maps_click_order_to_from_and_to() {
    let mut tool = ArmedTool::new(Kind::Segment);
    let first = pos(1_000_000, 1.0);
    let second = pos(4_000_000, 9.0);
    click(&mut tool, PANE_A, first);
    match click(&mut tool, PANE_A, second) {
        Placed::Complete(Geometry::Segment { from, to }) => {
            assert_eq!(from, first);
            assert_eq!(to, second);
        }
        other => panic!("expected a segment, got {other:?}"),
    }
}

#[test]
fn box_kinds_accept_their_corners_in_either_order() {
    for kind in [Kind::Rect, Kind::Ellipse] {
        let low = pos(1_000_000, 1.0);
        let high = pos(4_000_000, 9.0);

        let mut forward = ArmedTool::new(kind);
        click(&mut forward, PANE_A, low);
        let Placed::Complete(a) = click(&mut forward, PANE_A, high) else {
            panic!("{kind:?}: expected Complete");
        };

        let mut backward = ArmedTool::new(kind);
        click(&mut backward, PANE_A, high);
        let Placed::Complete(b) = click(&mut backward, PANE_A, low) else {
            panic!("{kind:?}: expected Complete");
        };

        let tf = unit_transform();
        assert_eq!(
            hit::screen_rect(&a, &tf),
            hit::screen_rect(&b, &tf),
            "{kind:?}: corner order changed the box"
        );
    }
}

#[test]
fn a_click_in_another_pane_restarts_instead_of_completing() {
    let mut tool = ArmedTool::new(Kind::Rect);
    let first = pos(1_000_000, 1.0);
    let other = pos(4_000_000, 9.0);
    click(&mut tool, PANE_A, first);
    assert!(
        matches!(click(&mut tool, PANE_B, other), Placed::Pending),
        "a second click in a different pane must not complete a shape"
    );
    assert_eq!(tool.pending, Some((PANE_B, other)));
}

#[test]
fn preview_is_none_without_a_pending_anchor() {
    let tool = ArmedTool::new(Kind::Rect);
    assert_eq!(place::preview(&tool, PANE_A, pos(0, 0.0)), None);
}

#[test]
fn preview_is_none_for_a_pane_that_does_not_own_the_anchor() {
    let mut tool = ArmedTool::new(Kind::Rect);
    click(&mut tool, PANE_A, pos(1_000_000, 1.0));
    assert_eq!(place::preview(&tool, PANE_B, pos(4_000_000, 9.0)), None);
}

#[test]
fn preview_matches_what_the_second_click_would_produce() {
    let mut tool = ArmedTool::new(Kind::Segment);
    let first = pos(1_000_000, 1.0);
    let cursor = pos(4_000_000, 9.0);
    click(&mut tool, PANE_A, first);
    let previewed = place::preview(&tool, PANE_A, cursor);
    let Placed::Complete(committed) = click(&mut tool, PANE_A, cursor) else {
        panic!("expected Complete");
    };
    assert_eq!(previewed, Some(committed));
}

#[test]
fn one_click_kinds_never_preview() {
    for kind in [Kind::Text, Kind::HLine] {
        let tool = ArmedTool::new(kind);
        assert_eq!(place::preview(&tool, PANE_A, pos(0, 0.0)), None, "{kind:?}");
    }
}

#[test]
fn commit_adds_selects_and_opens_the_editor_only_for_text() {
    let mut layer = AnnotationLayer::default();
    let text = place::commit(&mut layer, Geometry::Text { at: pos(0, 0.0) });
    assert_eq!(layer.selected, Some(text));
    assert_eq!(layer.editing, Some(text));

    let mut layer = AnnotationLayer::default();
    let line = place::commit(&mut layer, Geometry::HLine { y: 1.0 });
    assert_eq!(layer.selected, Some(line));
    assert_eq!(layer.editing, None);
}

#[test]
fn commit_sweeps_a_pending_empty_label_text_left_over_from_before() {
    let mut layer = AnnotationLayer::default();
    let abandoned = place::commit(&mut layer, Geometry::Text { at: pos(0, 0.0) });
    assert_eq!(layer.editing, Some(abandoned));
    assert_eq!(layer.items().len(), 1);

    let created = place::commit(&mut layer, Geometry::HLine { y: 1.0 });

    assert!(
        layer.get(abandoned).is_none(),
        "the abandoned empty-label text annotation must be swept"
    );
    assert_eq!(layer.items().len(), 1);
    assert_eq!(layer.selected, Some(created));
    assert_eq!(layer.editing, None);
    assert_eq!(
        layer.get(created).expect("exists").geom,
        Geometry::HLine { y: 1.0 }
    );
}

#[test]
fn commit_leaves_a_pending_text_with_a_label_alone() {
    let mut layer = AnnotationLayer::default();
    let labelled = place::commit(&mut layer, Geometry::Text { at: pos(0, 0.0) });
    layer.get_mut(labelled).expect("exists").label = "keep me".to_string();

    place::commit(&mut layer, Geometry::HLine { y: 1.0 });

    assert!(
        layer.get(labelled).is_some(),
        "a text annotation with a label must not be swept"
    );
    assert_eq!(layer.items().len(), 2);
}

#[test]
fn add_geometry_stores_the_exact_geometry_given() {
    let mut layer = AnnotationLayer::default();
    let geom = Geometry::Rect {
        a: pos(10, 1.0),
        b: pos(20, 2.0),
    };
    let id = layer.add_geometry(geom);
    assert_eq!(layer.get(id).expect("exists").geom, geom);
}

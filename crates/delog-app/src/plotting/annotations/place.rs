use super::{AnnotationLayer, DataPos, Geometry, Kind, default_geometry};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArmedTool {
    pub kind: Kind,
    pub pending: Option<(u64, DataPos)>,
}

impl ArmedTool {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            pending: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placed {
    Pending,
    Complete(Geometry),
}

fn needs_two_clicks(kind: Kind) -> bool {
    matches!(kind, Kind::Segment | Kind::Rect | Kind::Ellipse)
}

fn from_two_points(kind: Kind, first: DataPos, second: DataPos) -> Geometry {
    match kind {
        Kind::Segment => Geometry::Segment {
            from: first,
            to: second,
        },
        Kind::Ellipse => Geometry::Ellipse {
            a: first,
            b: second,
        },
        _ => Geometry::Rect {
            a: first,
            b: second,
        },
    }
}

pub fn on_plot_click(
    tool: &mut ArmedTool,
    pane: u64,
    at: DataPos,
    span_us: i64,
    y_span: f64,
) -> Placed {
    if !needs_two_clicks(tool.kind) {
        tool.pending = None;
        return Placed::Complete(default_geometry(tool.kind, at, span_us, y_span));
    }
    match tool.pending {
        Some((owner, first)) if owner == pane => {
            tool.pending = None;
            Placed::Complete(from_two_points(tool.kind, first, at))
        }
        _ => {
            tool.pending = Some((pane, at));
            Placed::Pending
        }
    }
}

pub fn preview(tool: &ArmedTool, pane: u64, cursor: DataPos) -> Option<Geometry> {
    let (owner, first) = tool.pending?;
    (owner == pane && needs_two_clicks(tool.kind))
        .then(|| from_two_points(tool.kind, first, cursor))
}

pub fn commit(layer: &mut AnnotationLayer, geom: Geometry) -> u64 {
    super::edit::close_editor(layer);
    let id = layer.add_geometry(geom);
    layer.selected = Some(id);
    layer.editing = (geom.kind() == Kind::Text).then_some(id);
    id
}

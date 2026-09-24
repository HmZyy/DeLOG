use super::ScriptOwner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Text,
    Segment,
    Rect,
    Ellipse,
    HLine,
}

impl AnnotationKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "segment" => Some(Self::Segment),
            "rect" => Some(Self::Rect),
            "ellipse" => Some(Self::Ellipse),
            "hline" => Some(Self::HLine),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationGeometry {
    Text { at: (i64, f64) },
    Segment { from: (i64, f64), to: (i64, f64) },
    Rect { a: (i64, f64), b: (i64, f64) },
    Ellipse { a: (i64, f64), b: (i64, f64) },
    HLine { y: f64 },
}

impl AnnotationGeometry {
    pub fn kind(&self) -> AnnotationKind {
        match self {
            Self::Text { .. } => AnnotationKind::Text,
            Self::Segment { .. } => AnnotationKind::Segment,
            Self::Rect { .. } => AnnotationKind::Rect,
            Self::Ellipse { .. } => AnnotationKind::Ellipse,
            Self::HLine { .. } => AnnotationKind::HLine,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnnotationStylePatch {
    pub color: Option<[f32; 4]>,
    pub stroke_px: Option<f32>,
    pub fill_opacity: Option<f32>,
    pub font_px: Option<f32>,
    pub arrow: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationFilter {
    Index(usize),
    Id(u64),
    Kind(AnnotationKind),
    Label(String),
    Owner(String),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationInfo {
    pub window: u64,
    pub tile: u64,
    pub id: u64,
    pub index: usize,
    pub kind: AnnotationKind,
    pub geometry: AnnotationGeometry,
    pub label: String,
    pub color: [f32; 4],
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationRequest {
    Add {
        window: u64,
        tile: u64,
        geometry: AnnotationGeometry,
        label: String,
        style: AnnotationStylePatch,
        owner: Option<ScriptOwner>,
    },
    List {
        target: Option<(u64, u64)>,
    },
    Remove {
        target: Option<(u64, u64)>,
        filter: AnnotationFilter,
    },
    Set {
        window: u64,
        tile: u64,
        id: u64,
        label: Option<String>,
        geometry: Option<AnnotationGeometry>,
        style: AnnotationStylePatch,
    },
}

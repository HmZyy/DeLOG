use delog_core::identity::FieldId;

use super::ScriptOwner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Line,
    Scatter,
    Step,
}

impl TraceMode {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "line" => Some(Self::Line),
            "scatter" => Some(Self::Scatter),
            "step" => Some(Self::Step),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceInfo {
    pub index: usize,
    pub field_id: FieldId,
    pub field: String,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceRequest {
    List {
        window: u64,
        tile: u64,
    },
    Add {
        window: u64,
        tile: u64,
        field_id: FieldId,
        field: String,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: TraceMode,
        owner: Option<ScriptOwner>,
    },
    Remove {
        window: u64,
        tile: u64,
        index: Option<usize>,
        field_id: Option<FieldId>,
        field: Option<String>,
    },
    Clear {
        window: u64,
        tile: u64,
    },
    Set {
        window: u64,
        tile: u64,
        index: usize,
        field_id: FieldId,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: Option<TraceMode>,
        visible: Option<bool>,
    },
}

use crate::markers::PendingMarker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOrigin {
    Manual,
    Script,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkerInfo {
    pub id: u64,
    pub index: usize,
    pub t_us: i64,
    pub label: String,
    pub color: [f32; 4],
    pub note: String,
    pub origin: MarkerOrigin,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarkerPatch {
    pub t_us: Option<i64>,
    pub label: Option<String>,
    pub color: Option<[f32; 4]>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerFilter {
    Id(u64),
    Index(usize),
    Owner(String),
    Origin(MarkerOrigin),
    ScriptLabel(String),
    ScriptTimeRange {
        after: Option<i64>,
        before: Option<i64>,
    },
    ScriptAll,
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerRequest {
    Append {
        owner: String,
        generation: u64,
        markers: Vec<PendingMarker>,
    },
    RemoveOwned {
        owner: String,
    },
    List,
    Set {
        id: u64,
        patch: MarkerPatch,
    },
    Remove(MarkerFilter),
}

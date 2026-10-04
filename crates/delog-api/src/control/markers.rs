use crate::markers::PendingMarker;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOrigin {
    Manual,
    Script,
}

impl MarkerOrigin {
    pub fn parse(origin: &str) -> Result<Self> {
        match origin {
            "manual" => Ok(Self::Manual),
            "script" => Ok(Self::Script),
            _ => Err(Error::invalid_input(format!(
                "marker origin must be 'manual' or 'script', got {origin:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Script => "script",
        }
    }
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

impl MarkerPatch {
    pub fn validate(&self) -> Result<()> {
        if self.label.as_deref() == Some("") {
            return Err(Error::invalid_input("marker label must not be empty"));
        }
        if let Some(color) = self.color
            && !color
                .iter()
                .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
        {
            return Err(Error::invalid_input(
                "marker color components must be finite and between 0 and 1",
            ));
        }
        Ok(())
    }
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

impl MarkerFilter {
    pub fn owner(owner: String) -> Result<Self> {
        if owner.is_empty() {
            return Err(Error::invalid_input("marker owner must not be empty"));
        }
        Ok(Self::Owner(owner))
    }

    pub fn script_label(label: String) -> Result<Self> {
        if label.is_empty() {
            return Err(Error::invalid_input("marker label must not be empty"));
        }
        Ok(Self::ScriptLabel(label))
    }

    pub fn time_range(after: Option<i64>, before: Option<i64>) -> Result<Self> {
        if matches!((after, before), (Some(after), Some(before)) if after > before) {
            return Err(Error::invalid_input(
                "marker time range requires after <= before",
            ));
        }
        Ok(Self::ScriptTimeRange { after, before })
    }
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

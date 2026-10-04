use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutFieldIssue {
    pub field: String,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadReport {
    pub ambiguous: Vec<LayoutFieldIssue>,
    pub unresolved: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutRequest {
    List,
    Save { name: String },
    Load { name: String },
    Delete { name: String },
    Rename { from: String, to: String },
    Duplicate { from: String, to: String },
    ImportFile { path: String },
    ExportFile { name: String, path: String },
    Clear,
    Current,
    Apply { json: String },
}

pub fn validate_layout_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(Error::invalid_input(
            "layout names may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(name.to_owned())
}

pub fn validate_layout_path(path: &str) -> Result<String> {
    if path.trim().is_empty() {
        return Err(Error::invalid_input("layout path must not be empty"));
    }
    Ok(path.to_owned())
}

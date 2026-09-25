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

impl LayoutRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::List | Self::Clear | Self::Current => Ok(()),
            Self::Save { name } | Self::Load { name } | Self::Delete { name } => {
                validate_layout_name(name).map(|_| ())
            }
            Self::Rename { from, to } | Self::Duplicate { from, to } => {
                validate_layout_name(from)?;
                validate_layout_name(to).map(|_| ())
            }
            Self::ImportFile { path } => validate_layout_path(path).map(|_| ()),
            Self::ExportFile { name, path } => {
                validate_layout_name(name)?;
                validate_layout_path(path).map(|_| ())
            }
            Self::Apply { json } => {
                if json.trim().is_empty() {
                    Err(Error::invalid_input("layout JSON must not be empty"))
                } else {
                    Ok(())
                }
            }
        }
    }
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

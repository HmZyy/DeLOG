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

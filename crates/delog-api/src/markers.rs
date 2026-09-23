use crate::color::parse_hex_color;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct PendingMarker {
    pub time_us: i64,
    pub label: String,
    pub color: Option<[f32; 4]>,
    pub note: String,
}

impl PendingMarker {
    pub fn new(
        time_us: i64,
        label: String,
        color: Option<&str>,
        note: Option<String>,
    ) -> Result<Self> {
        if label.is_empty() {
            return Err(Error::invalid_input("marker label must not be empty"));
        }
        Ok(Self {
            time_us,
            label,
            color: color.map(parse_hex_color).transpose()?,
            note: note.unwrap_or_default(),
        })
    }
}

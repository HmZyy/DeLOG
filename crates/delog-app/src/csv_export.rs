//! CSV export: field-picker dialog state and the streaming writer.
//!
//! The row engine is `delog_core::export`; this module formats its `Cell` rows as
//! CSV and drives the off-thread write. egui wiring lives in `app.rs`.

// Public API consumed by Task 6 (dialog + app wiring); not yet called.
#![allow(dead_code)]

use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use delog_core::export::{Cell, ExportError, ResampleMode, RowCursor};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::browser::BrowserModel;

pub const MODES: [&str; 3] = ["None (union)", "Previous-fill", "Linear @ dt"];

#[derive(Default)]
pub struct CsvExportState {
    pub open: bool,
    pub search: String,
    pub checked: HashSet<FieldId>,
    pub visible_range: bool,
    pub mode_ix: usize,
    pub dt_s: f64,
}

impl CsvExportState {
    pub fn mode(&self) -> ResampleMode {
        match self.mode_ix {
            1 => ResampleMode::PrevFill,
            2 => ResampleMode::Linear {
                dt_us: ((self.dt_s.max(1e-6)) * 1e6) as i64,
            },
            _ => ResampleMode::None,
        }
    }
}

pub struct CsvField {
    pub id: FieldId,
    pub label: String,
    pub unit: Option<String>,
}

#[derive(Debug)]
pub enum CsvExportError {
    Export(ExportError),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for CsvExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Export(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Cancelled => write!(f, "export cancelled"),
        }
    }
}

/// Numeric/boolean fields across all sources, labelled "source / topic.field".
pub fn numeric_fields(snapshot: &StoreSnapshot, model: &BrowserModel) -> Vec<CsvField> {
    let mut out = Vec::new();
    for src in &model.sources {
        for topic in &src.topics {
            for field in &topic.fields {
                if let Ok(view) = delog_core::field_view::FieldView::new(snapshot, field.id) {
                    let sf = view.schema_field();
                    if sf.is_plottable() {
                        out.push(CsvField {
                            id: field.id,
                            label: format!("{} / {}.{}", src.label, topic.name, field.name),
                            unit: sf.unit.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

fn csv_escape(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains([',', '"', '\n', '\r']) {
        std::borrow::Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

pub fn header_line(time_cols: &[&str], fields: &[CsvField]) -> String {
    let mut parts: Vec<String> = time_cols.iter().map(|s| s.to_string()).collect();
    for f in fields {
        let name = match &f.unit {
            Some(u) if !u.is_empty() => format!("{} [{}]", f.label, u),
            _ => f.label.clone(),
        };
        parts.push(csv_escape(&name).into_owned());
    }
    parts.join(",")
}

pub fn format_row(t_us: i64, t_s: f64, cells: &[Cell]) -> String {
    let mut s = format!("{t_us},{t_s}");
    for c in cells {
        s.push(',');
        if let Cell::Num(v) = c {
            s.push_str(&v.to_string());
        }
    }
    s
}

#[allow(clippy::too_many_arguments)]
pub fn write_csv<W: Write>(
    w: &mut W,
    snapshot: &StoreSnapshot,
    fields: &[CsvField],
    window: (i64, i64),
    mode: ResampleMode,
    origin_us: i64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<u64, CsvExportError> {
    let ids: Vec<FieldId> = fields.iter().map(|f| f.id).collect();
    let mut cursor =
        RowCursor::new(snapshot, &ids, window.0, window.1, mode).map_err(CsvExportError::Export)?;
    writeln!(w, "{}", header_line(&["t_us", "t_s"], fields))
        .map_err(|e| CsvExportError::Io(e.to_string()))?;

    let span = (window.1 - window.0).max(1) as f32;
    let mut out: Vec<Cell> = Vec::with_capacity(fields.len());
    let mut rows = 0u64;
    while let Some(t_us) = cursor.next_row(&mut out) {
        if cancel.load(Ordering::Relaxed) {
            return Err(CsvExportError::Cancelled);
        }
        let t_s = (t_us - origin_us) as f64 * 1e-6;
        writeln!(w, "{}", format_row(t_us, t_s, &out))
            .map_err(|e| CsvExportError::Io(e.to_string()))?;
        rows += 1;
        if rows.is_multiple_of(4096) {
            progress(((t_us - window.0) as f32 / span).clamp(0.0, 1.0));
        }
    }
    progress(1.0);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_core::export::Cell;

    #[test]
    fn header_line_joins_time_and_field_columns_with_units() {
        let fields = vec![
            CsvField {
                id: delog_core::identity::FieldId(1),
                label: "flight / BARO.Alt".into(),
                unit: Some("m".into()),
            },
            CsvField {
                id: delog_core::identity::FieldId(2),
                label: "flight / ATT.Roll".into(),
                unit: None,
            },
        ];
        assert_eq!(
            header_line(&["t_us", "t_s"], &fields),
            "t_us,t_s,flight / BARO.Alt [m],flight / ATT.Roll"
        );
    }

    #[test]
    fn format_row_blanks_empty_cells_and_keeps_precision() {
        let line = format_row(
            1500,
            0.0015,
            &[Cell::Num(1.25), Cell::Empty, Cell::Num(-3.0)],
        );
        assert_eq!(line, "1500,0.0015,1.25,,-3");
    }

    #[test]
    fn header_escapes_a_label_containing_a_comma() {
        let fields = vec![CsvField {
            id: delog_core::identity::FieldId(1),
            label: "a,b".into(),
            unit: None,
        }];
        assert_eq!(header_line(&["t_us"], &fields), "t_us,\"a,b\"");
    }
}

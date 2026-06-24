//! Streaming CSV-row production over an immutable snapshot.
//!
//! Yields `(effective_time_us, [Cell])` rows for a set of numeric fields under a
//! resample mode. Reads Arrow chunks in place; multipliers are
//! applied in f64 (engineering units); a null or NaN sample is an `Empty` cell
//! (NaN is a gap).

use std::error::Error;
use std::fmt;

use crate::field_view::{FieldView, FieldViewError};
use crate::identity::FieldId;
use crate::snapshot::StoreSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResampleMode {
    None,
    PrevFill,
    Linear { dt_us: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    Empty,
    Num(f64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    NoFields,
    InvalidWindow,
    InvalidDt,
    NotNumeric(FieldId),
    Field(FieldId, FieldViewError),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFields => write!(f, "no fields selected for export"),
            Self::InvalidWindow => write!(f, "export window end must be after start"),
            Self::InvalidDt => write!(f, "resample dt must be positive"),
            Self::NotNumeric(id) => write!(f, "field {id:?} is not numeric"),
            Self::Field(id, e) => write!(f, "field {id:?}: {e}"),
        }
    }
}
impl Error for ExportError {}

pub struct RowCursor<'a> {
    views: Vec<FieldView<'a>>,
    multipliers: Vec<f64>,
    t_start: i64,
    t_end: i64,
    mode: ResampleMode,
    // Per-mode iteration state filled in Tasks 2-4.
    started: bool,
}

impl fmt::Debug for RowCursor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RowCursor")
            .field("t_start", &self.t_start)
            .field("t_end", &self.t_end)
            .field("mode", &self.mode)
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

impl<'a> RowCursor<'a> {
    pub fn new(
        snapshot: &'a StoreSnapshot,
        fields: &[FieldId],
        t_start_us: i64,
        t_end_us: i64,
        mode: ResampleMode,
    ) -> Result<Self, ExportError> {
        if fields.is_empty() {
            return Err(ExportError::NoFields);
        }
        if t_end_us < t_start_us {
            return Err(ExportError::InvalidWindow);
        }
        if let ResampleMode::Linear { dt_us } = mode
            && dt_us <= 0
        {
            return Err(ExportError::InvalidDt);
        }
        let mut views = Vec::with_capacity(fields.len());
        let mut multipliers = Vec::with_capacity(fields.len());
        for &id in fields {
            let view = FieldView::new(snapshot, id).map_err(|e| ExportError::Field(id, e))?;
            if !view.schema_field().is_plottable() {
                return Err(ExportError::NotNumeric(id));
            }
            multipliers.push(view.schema_field().multiplier);
            views.push(view);
        }
        Ok(Self {
            views,
            multipliers,
            t_start: t_start_us,
            t_end: t_end_us,
            mode,
            started: false,
        })
    }

    /// Fill `out` with one row's cells (cleared then pushed, len == fields.len())
    /// and return the row's effective timestamp µs, or `None` at the end.
    pub fn next_row(&mut self, out: &mut Vec<Cell>) -> Option<i64> {
        let _ = (
            &self.views,
            &self.multipliers,
            self.t_start,
            self.t_end,
            self.mode,
            &mut self.started,
            out,
        );
        None // Tasks 2-4 implement per-mode iteration.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::identity::IdentityRegistry;
    use crate::schema::{FieldSchema, TopicSchema};
    use crate::snapshot::StoreSnapshot;
    use crate::store::TopicStore;
    use arrow::array::{Float64Array, Int64Array, StringArray};
    use std::sync::Arc;

    /// One source, one topic "T" with the given (name, dtype) fields and rows.
    /// `cols[i]` is the Arrow array for field i; `t` is raw µs.
    fn snapshot_with(
        fields: &[FieldSchema],
        t: Vec<i64>,
        cols: Vec<arrow::array::ArrayRef>,
        offset_us: i64,
    ) -> (StoreSnapshot, Vec<crate::identity::FieldId>) {
        let mut reg = IdentityRegistry::new();
        let source = reg.add_source("flight");
        reg.set_source_offset_us(source, offset_us);
        let schema = Arc::new(TopicSchema::new("T", fields.to_vec()).unwrap());
        let topic = reg.add_topic(source, "T").unwrap();
        let field_ids: Vec<_> = fields
            .iter()
            .map(|f| reg.add_field(topic, &f.name).unwrap())
            .collect();
        let chunk = Chunk::try_new(Int64Array::from(t), cols, &schema).unwrap();
        let store = TopicStore::from_chunks(schema, vec![Arc::new(chunk)]).unwrap();
        let snapshot = StoreSnapshot::from_registry(&reg, [(topic, Arc::new(store))], 1).unwrap();
        (snapshot, field_ids)
    }

    fn num(name: &str) -> FieldSchema {
        FieldSchema {
            name: name.into(),
            dtype: arrow::datatypes::DataType::Float64,
            unit: None,
            multiplier: 1.0,
            description: None,
        }
    }

    #[test]
    fn new_rejects_empty_field_list() {
        let (snap, _) = snapshot_with(
            &[num("a")],
            vec![0],
            vec![Arc::new(Float64Array::from(vec![1.0]))],
            0,
        );
        let err = RowCursor::new(&snap, &[], 0, 10, ResampleMode::None).unwrap_err();
        assert_eq!(err, ExportError::NoFields);
    }

    #[test]
    fn new_rejects_inverted_window() {
        let (snap, ids) = snapshot_with(
            &[num("a")],
            vec![0],
            vec![Arc::new(Float64Array::from(vec![1.0]))],
            0,
        );
        let err = RowCursor::new(&snap, &ids, 10, 0, ResampleMode::None).unwrap_err();
        assert_eq!(err, ExportError::InvalidWindow);
    }

    #[test]
    fn new_rejects_non_positive_dt() {
        let (snap, ids) = snapshot_with(
            &[num("a")],
            vec![0],
            vec![Arc::new(Float64Array::from(vec![1.0]))],
            0,
        );
        let err =
            RowCursor::new(&snap, &ids, 0, 10, ResampleMode::Linear { dt_us: 0 }).unwrap_err();
        assert_eq!(err, ExportError::InvalidDt);
    }

    #[test]
    fn new_rejects_string_field() {
        let s = FieldSchema {
            name: "msg".into(),
            dtype: arrow::datatypes::DataType::Utf8,
            unit: None,
            multiplier: 1.0,
            description: None,
        };
        let (snap, ids) = snapshot_with(
            &[s],
            vec![0],
            vec![Arc::new(StringArray::from(vec!["x"]))],
            0,
        );
        let err = RowCursor::new(&snap, &ids, 0, 10, ResampleMode::None).unwrap_err();
        assert_eq!(err, ExportError::NotNumeric(ids[0]));
    }
}

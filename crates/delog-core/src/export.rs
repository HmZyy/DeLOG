//! Streaming CSV-row production over an immutable snapshot.
//!
//! Yields `(effective_time_us, [Cell])` rows for a set of numeric fields under a
//! resample mode. Reads Arrow chunks in place; multipliers are
//! applied in f64 (engineering units); a null or NaN sample is an `Empty` cell
//! (NaN is a gap).

use std::error::Error;
use std::fmt;

use crate::field_view::{FieldView, FieldViewError, value_at};
use crate::identity::FieldId;
use crate::snapshot::StoreSnapshot;
use crate::time::effective_time_us;

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

/// Lazy ascending iterator over one field's (effective_time, Cell) samples within
/// [t_start, t_end]. Iterates chunks in spine order; assumes the common
/// sorted/non-overlapping spine — out-of-order spines export in stored order.
struct FieldCursor<'a> {
    chunks: Vec<&'a crate::chunk::Chunk>,
    col_index: usize,
    offset_us: i64,
    multiplier: f64,
    t_start: i64,
    t_end: i64,
    chunk_i: usize,
    row_i: usize,
}

impl<'a> FieldCursor<'a> {
    fn from_view(view: &FieldView<'a>, multiplier: f64, t_start: i64, t_end: i64) -> Self {
        let range = crate::time::TimeRange {
            min_us: t_start,
            max_us: t_end,
        };
        let chunks: Vec<_> = view.chunks_overlapping(range).collect();
        Self {
            chunks,
            col_index: view.col_index(),
            offset_us: view.offset_us_for_export(),
            multiplier,
            t_start,
            t_end,
            chunk_i: 0,
            row_i: 0,
        }
    }

    /// Peek the next in-range sample's effective time without consuming it.
    fn peek_time(&mut self) -> Option<i64> {
        self.advance_to_in_range().map(|(t, _)| t)
    }

    /// Consume and return the next in-range (effective_time, Cell).
    fn pop(&mut self) -> Option<(i64, Cell)> {
        let res = self.advance_to_in_range();
        if res.is_some() {
            self.row_i += 1;
        }
        res
    }

    /// Move (chunk_i, row_i) to the next sample whose effective time is within
    /// [t_start, t_end]; return it without consuming. Samples before t_start are
    /// skipped; the first sample past t_end ends iteration. Rows where this
    /// field's column is SQL-null (no measurement recorded) are skipped — they
    /// carry no information and do not update the prev-fill carry.
    fn advance_to_in_range(&mut self) -> Option<(i64, Cell)> {
        loop {
            let chunk = *self.chunks.get(self.chunk_i)?;
            if self.row_i >= chunk.len() {
                self.chunk_i += 1;
                self.row_i = 0;
                continue;
            }
            let raw = chunk.t.value(self.row_i);
            let Some(eff) = effective_time_us(raw, self.offset_us) else {
                self.row_i += 1;
                continue;
            };
            if eff < self.t_start {
                self.row_i += 1;
                continue;
            }
            if eff > self.t_end {
                return None;
            }
            let col = chunk.cols[self.col_index].as_ref();
            if col.is_null(self.row_i) {
                self.row_i += 1;
                continue;
            }
            let cell = cell_from(value_at(col, self.row_i), self.multiplier);
            return Some((eff, cell));
        }
    }
}

/// Map a sampled value to an export cell: null / non-numeric / NaN -> Empty,
/// else Num(value * multiplier). Bool -> 0/1.
fn cell_from(value: crate::field_view::SampleValue<'_>, multiplier: f64) -> Cell {
    use crate::field_view::SampleValue;
    let v = match value {
        SampleValue::Int(v) => v as f64,
        SampleValue::UInt(v) => v as f64,
        SampleValue::Float(v) if !v.is_nan() => v,
        SampleValue::Bool(b) => b as i64 as f64,
        SampleValue::Float(_) | SampleValue::Utf8(_) | SampleValue::Null => return Cell::Empty,
    };
    Cell::Num(v * multiplier)
}

pub struct RowCursor<'a> {
    views: Vec<FieldView<'a>>,
    multipliers: Vec<f64>,
    t_start: i64,
    t_end: i64,
    mode: ResampleMode,
    next_grid_t: i64,
    cursors: Vec<FieldCursor<'a>>,
    last: Vec<Cell>,
}

impl fmt::Debug for RowCursor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RowCursor")
            .field("t_start", &self.t_start)
            .field("t_end", &self.t_end)
            .field("mode", &self.mode)
            .field("next_grid_t", &self.next_grid_t)
            .field("cursors_len", &self.cursors.len())
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
        let n = fields.len();
        let cursors = views
            .iter()
            .zip(&multipliers)
            .map(|(v, &m)| FieldCursor::from_view(v, m, t_start_us, t_end_us))
            .collect();
        Ok(Self {
            views,
            multipliers,
            t_start: t_start_us,
            t_end: t_end_us,
            mode,
            next_grid_t: t_start_us,
            cursors,
            last: vec![Cell::Empty; n],
        })
    }

    /// Fill `out` with one row's cells (cleared then pushed, len == fields.len())
    /// and return the row's effective timestamp µs, or `None` at the end.
    pub fn next_row(&mut self, out: &mut Vec<Cell>) -> Option<i64> {
        match self.mode {
            ResampleMode::None => self.next_union_row(false, out),
            ResampleMode::PrevFill => self.next_union_row(true, out), // Task 3
            ResampleMode::Linear { dt_us } => self.next_linear_row(dt_us, out), // Task 4
        }
    }

    /// One row of the union timeline. `prev_fill=false` -> None mode (cell filled
    /// only on an exact sample); `prev_fill=true` -> PrevFill mode (emit last
    /// seen value for fields with no sample at this timestamp).
    fn next_union_row(&mut self, prev_fill: bool, out: &mut Vec<Cell>) -> Option<i64> {
        let t = self
            .cursors
            .iter_mut()
            .filter_map(|c| c.peek_time())
            .min()?;
        out.clear();
        for (i, c) in self.cursors.iter_mut().enumerate() {
            if c.peek_time() == Some(t) {
                let (_t, cell) = c.pop().expect("peeked");
                self.last[i] = cell;
                out.push(cell);
            } else if prev_fill {
                out.push(self.last[i]);
            } else {
                out.push(Cell::Empty);
            }
        }
        Some(t)
    }

    fn next_linear_row(&mut self, dt_us: i64, out: &mut Vec<Cell>) -> Option<i64> {
        if self.next_grid_t > self.t_end {
            return None;
        }
        let t = self.next_grid_t;
        out.clear();
        for (view, &mult) in self.views.iter().zip(&self.multipliers) {
            let cell = view
                .sample_at(t, crate::field_view::SampleMode::Linear)
                .and_then(|s| s.value.as_f64())
                .map(|v| Cell::Num(v * mult))
                .unwrap_or(Cell::Empty);
            out.push(cell);
        }
        self.next_grid_t = self.next_grid_t.saturating_add(dt_us);
        Some(t)
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
    use Cell::{Empty, Num};
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

    /// Collect all rows as (t_us, cells).
    fn collect(
        snap: &StoreSnapshot,
        ids: &[crate::identity::FieldId],
        a: i64,
        b: i64,
        mode: ResampleMode,
    ) -> Vec<(i64, Vec<Cell>)> {
        let mut cur = RowCursor::new(snap, ids, a, b, mode).unwrap();
        let mut rows = Vec::new();
        let mut out = Vec::new();
        while let Some(t) = cur.next_row(&mut out) {
            rows.push((t, out.clone()));
        }
        rows
    }

    #[test]
    fn none_mode_unions_two_fields_with_blanks() {
        let fields = vec![num("a"), num("b")];
        let cols: Vec<arrow::array::ArrayRef> = vec![
            Arc::new(Float64Array::from(vec![Some(10.0), None, Some(12.0)])),
            Arc::new(Float64Array::from(vec![None, Some(21.0), Some(22.0)])),
        ];
        let (snap, ids) = snapshot_with(&fields, vec![0, 1, 2], cols, 0);
        let rows = collect(&snap, &ids, 0, 2, ResampleMode::None);
        assert_eq!(
            rows,
            vec![
                (0, vec![Num(10.0), Empty]),
                (1, vec![Empty, Num(21.0)]),
                (2, vec![Num(12.0), Num(22.0)]),
            ]
        );
    }

    #[test]
    fn none_mode_respects_window_and_nan_is_gap() {
        let fields = vec![num("a")];
        let cols: Vec<arrow::array::ArrayRef> =
            vec![Arc::new(Float64Array::from(vec![1.0, f64::NAN, 3.0, 4.0]))];
        let (snap, ids) = snapshot_with(&fields, vec![0, 1, 2, 3], cols, 0);
        let rows = collect(&snap, &ids, 1, 2, ResampleMode::None);
        assert_eq!(
            rows,
            vec![
                (1, vec![Empty]), // NaN -> gap
                (2, vec![Num(3.0)]),
            ]
        );
    }

    #[test]
    fn none_mode_applies_multiplier_and_offset() {
        let mut f = num("a");
        f.multiplier = 0.01;
        let cols: Vec<arrow::array::ArrayRef> =
            vec![Arc::new(Float64Array::from(vec![100.0, 200.0]))];
        let (snap, ids) = snapshot_with(&[f], vec![0, 1000], cols, 500); // offset +500us
        let rows = collect(&snap, &ids, 0, 100_000, ResampleMode::None);
        assert_eq!(
            rows,
            vec![
                (500, vec![Num(1.0)]), // effective = raw + offset
                (1500, vec![Num(2.0)]),
            ]
        );
    }

    #[test]
    fn prevfill_holds_last_value_and_blanks_before_first() {
        let fields = vec![num("a"), num("b")];
        let cols: Vec<arrow::array::ArrayRef> = vec![
            Arc::new(Float64Array::from(vec![Some(10.0), None, Some(12.0)])),
            Arc::new(Float64Array::from(vec![None, Some(21.0), Some(22.0)])),
        ];
        let (snap, ids) = snapshot_with(&fields, vec![0, 1, 2], cols, 0);
        let rows = collect(&snap, &ids, 0, 2, ResampleMode::PrevFill);
        assert_eq!(
            rows,
            vec![
                (0, vec![Num(10.0), Empty]),     // b not seen yet
                (1, vec![Num(10.0), Num(21.0)]), // a held
                (2, vec![Num(12.0), Num(22.0)]),
            ]
        );
    }

    #[test]
    fn prevfill_nan_sample_clears_to_gap() {
        // a: t0=1.0, t1=NaN, t2=3.0 ; at t1 the held value becomes a gap.
        let fields = vec![num("a")];
        let cols: Vec<arrow::array::ArrayRef> =
            vec![Arc::new(Float64Array::from(vec![1.0, f64::NAN, 3.0]))];
        let (snap, ids) = snapshot_with(&fields, vec![0, 1, 2], cols, 0);
        let rows = collect(&snap, &ids, 0, 2, ResampleMode::PrevFill);
        assert_eq!(
            rows,
            vec![
                (0, vec![Num(1.0)]),
                (1, vec![Empty]), // NaN is a gap, holds the gap
                (2, vec![Num(3.0)]),
            ]
        );
    }

    #[test]
    fn linear_grid_interpolates_between_samples() {
        // a: t=0 ->0.0, t=10 ->10.0 ; grid dt=5 over [0,10].
        let fields = vec![num("a")];
        let cols: Vec<arrow::array::ArrayRef> = vec![Arc::new(Float64Array::from(vec![0.0, 10.0]))];
        let (snap, ids) = snapshot_with(&fields, vec![0, 10], cols, 0);
        let rows = collect(&snap, &ids, 0, 10, ResampleMode::Linear { dt_us: 5 });
        assert_eq!(
            rows,
            vec![
                (0, vec![Num(0.0)]),
                (5, vec![Num(5.0)]),
                (10, vec![Num(10.0)]),
            ]
        );
    }

    #[test]
    fn linear_outside_sample_span_is_gap() {
        let fields = vec![num("a")];
        let cols: Vec<arrow::array::ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0, 2.0]))];
        let (snap, ids) = snapshot_with(&fields, vec![10, 20], cols, 0);
        // grid starts before the first sample and ends after the last.
        let rows = collect(&snap, &ids, 0, 30, ResampleMode::Linear { dt_us: 10 });
        assert_eq!(
            rows,
            vec![
                (0, vec![Empty]), // before first sample, no bracket
                (10, vec![Num(1.0)]),
                (20, vec![Num(2.0)]),
                (30, vec![Empty]), // after last sample
            ]
        );
    }

    #[test]
    fn linear_multiplier_applied() {
        let mut f = num("a");
        f.multiplier = 2.0;
        let cols: Vec<arrow::array::ArrayRef> = vec![Arc::new(Float64Array::from(vec![0.0, 10.0]))];
        let (snap, ids) = snapshot_with(&[f], vec![0, 10], cols, 0);
        let rows = collect(&snap, &ids, 0, 10, ResampleMode::Linear { dt_us: 10 });
        assert_eq!(rows, vec![(0, vec![Num(0.0)]), (10, vec![Num(20.0)])]);
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

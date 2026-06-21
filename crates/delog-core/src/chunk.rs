//! Immutable Arrow chunk storage.

use std::error::Error;
use std::fmt;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
    Int64Array, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;

use crate::schema::TopicSchema;
use crate::time::TimestampUs;

/// Seal-time column statistics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColStats {
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub sum_sq: f64,
    pub nan_count: u64,
}

/// Immutable, sorted time chunk.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub t: Int64Array,
    pub cols: Vec<ArrayRef>,
    pub stats: Vec<ColStats>,
    pub t_min: TimestampUs,
    pub t_max: TimestampUs,
}

/// Chunk sealing/validation failures.
#[derive(Debug, Clone, PartialEq)]
pub enum ChunkError {
    EmptyChunk,
    NullTimestamp {
        index: usize,
    },
    TimestampRegression {
        index: usize,
        previous: TimestampUs,
        current: TimestampUs,
    },
    ColumnCountMismatch {
        expected: usize,
        actual: usize,
    },
    ColumnLengthMismatch {
        column: usize,
        expected: usize,
        actual: usize,
    },
    ColumnTypeMismatch {
        column: usize,
        expected: DataType,
        actual: DataType,
    },
}

impl Chunk {
    /// Validate and seal a chunk, computing per-column stats once.
    pub fn try_new(
        t: Int64Array,
        cols: Vec<ArrayRef>,
        schema: &TopicSchema,
    ) -> Result<Self, ChunkError> {
        validate_time(&t)?;
        validate_columns(&cols, schema, t.len())?;

        let t_min = t.value(0);
        let t_max = t.value(t.len() - 1);
        let stats = cols
            .iter()
            .map(|col| stats_for_array(col.as_ref()))
            .collect();

        Ok(Self {
            t,
            cols,
            stats,
            t_min,
            t_max,
        })
    }

    pub fn len(&self) -> usize {
        self.t.len()
    }

    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
}

impl ColStats {
    fn empty() -> Self {
        Self {
            min: f64::NAN,
            max: f64::NAN,
            sum: 0.0,
            sum_sq: 0.0,
            nan_count: 0,
        }
    }

    fn observe(&mut self, value: f64) {
        if value.is_nan() {
            self.nan_count += 1;
            return;
        }

        if self.min.is_nan() || value < self.min {
            self.min = value;
        }
        if self.max.is_nan() || value > self.max {
            self.max = value;
        }
        self.sum += value;
        self.sum_sq += value * value;
    }

    fn count_missing(&mut self) {
        self.nan_count += 1;
    }
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyChunk => write!(f, "chunk must contain at least one row"),
            Self::NullTimestamp { index } => write!(f, "timestamp at row {index} is null"),
            Self::TimestampRegression {
                index,
                previous,
                current,
            } => write!(
                f,
                "timestamp regression at row {index}: {current} < previous {previous}"
            ),
            Self::ColumnCountMismatch { expected, actual } => {
                write!(
                    f,
                    "column count mismatch: expected {expected}, got {actual}"
                )
            }
            Self::ColumnLengthMismatch {
                column,
                expected,
                actual,
            } => write!(
                f,
                "column {column} length mismatch: expected {expected}, got {actual}"
            ),
            Self::ColumnTypeMismatch {
                column,
                expected,
                actual,
            } => write!(
                f,
                "column {column} type mismatch: expected {expected:?}, got {actual:?}"
            ),
        }
    }
}

impl Error for ChunkError {}

fn validate_time(t: &Int64Array) -> Result<(), ChunkError> {
    if t.is_empty() {
        return Err(ChunkError::EmptyChunk);
    }

    let mut previous = None;
    for idx in 0..t.len() {
        if t.is_null(idx) {
            return Err(ChunkError::NullTimestamp { index: idx });
        }
        let current = t.value(idx);
        if let Some(previous) = previous
            && current < previous
        {
            return Err(ChunkError::TimestampRegression {
                index: idx,
                previous,
                current,
            });
        }
        previous = Some(current);
    }
    Ok(())
}

fn validate_columns(
    cols: &[ArrayRef],
    schema: &TopicSchema,
    expected_len: usize,
) -> Result<(), ChunkError> {
    if cols.len() != schema.len() {
        return Err(ChunkError::ColumnCountMismatch {
            expected: schema.len(),
            actual: cols.len(),
        });
    }

    for (idx, (col, field)) in cols.iter().zip(schema.fields()).enumerate() {
        if col.len() != expected_len {
            return Err(ChunkError::ColumnLengthMismatch {
                column: idx,
                expected: expected_len,
                actual: col.len(),
            });
        }
        if col.data_type() != &field.dtype {
            return Err(ChunkError::ColumnTypeMismatch {
                column: idx,
                expected: field.dtype.clone(),
                actual: col.data_type().clone(),
            });
        }
    }
    Ok(())
}

fn stats_for_array(array: &dyn Array) -> ColStats {
    match array.data_type() {
        DataType::Int8 => stats_i8(array.as_any().downcast_ref::<Int8Array>().unwrap()),
        DataType::Int16 => stats_i16(array.as_any().downcast_ref::<Int16Array>().unwrap()),
        DataType::Int32 => stats_i32(array.as_any().downcast_ref::<Int32Array>().unwrap()),
        DataType::Int64 => stats_i64(array.as_any().downcast_ref::<Int64Array>().unwrap()),
        DataType::UInt8 => stats_u8(array.as_any().downcast_ref::<UInt8Array>().unwrap()),
        DataType::UInt16 => stats_u16(array.as_any().downcast_ref::<UInt16Array>().unwrap()),
        DataType::UInt32 => stats_u32(array.as_any().downcast_ref::<UInt32Array>().unwrap()),
        DataType::UInt64 => stats_u64(array.as_any().downcast_ref::<UInt64Array>().unwrap()),
        DataType::Float32 => stats_f32(array.as_any().downcast_ref::<Float32Array>().unwrap()),
        DataType::Float64 => stats_f64(array.as_any().downcast_ref::<Float64Array>().unwrap()),
        DataType::Boolean => stats_bool(array.as_any().downcast_ref::<BooleanArray>().unwrap()),
        DataType::Utf8 | DataType::LargeUtf8 => stats_non_numeric(array),
        dtype => unreachable!("schema validation rejects unsupported dtype {dtype:?}"),
    }
}

macro_rules! int_stats_fn {
    ($fn_name:ident, $array_ty:ty) => {
        fn $fn_name(array: &$array_ty) -> ColStats {
            let mut stats = ColStats::empty();
            for idx in 0..array.len() {
                if array.is_null(idx) {
                    stats.count_missing();
                } else {
                    stats.observe(array.value(idx) as f64);
                }
            }
            stats
        }
    };
}

int_stats_fn!(stats_i8, Int8Array);
int_stats_fn!(stats_i16, Int16Array);
int_stats_fn!(stats_i32, Int32Array);
int_stats_fn!(stats_i64, Int64Array);
int_stats_fn!(stats_u8, UInt8Array);
int_stats_fn!(stats_u16, UInt16Array);
int_stats_fn!(stats_u32, UInt32Array);
int_stats_fn!(stats_u64, UInt64Array);

fn stats_f32(array: &Float32Array) -> ColStats {
    let mut stats = ColStats::empty();
    for idx in 0..array.len() {
        if array.is_null(idx) {
            stats.count_missing();
        } else {
            stats.observe(f64::from(array.value(idx)));
        }
    }
    stats
}

fn stats_f64(array: &Float64Array) -> ColStats {
    let mut stats = ColStats::empty();
    for idx in 0..array.len() {
        if array.is_null(idx) {
            stats.count_missing();
        } else {
            stats.observe(array.value(idx));
        }
    }
    stats
}

fn stats_bool(array: &BooleanArray) -> ColStats {
    let mut stats = ColStats::empty();
    for idx in 0..array.len() {
        if array.is_null(idx) {
            stats.count_missing();
        } else {
            stats.observe(if array.value(idx) { 1.0 } else { 0.0 });
        }
    }
    stats
}

fn stats_non_numeric(array: &dyn Array) -> ColStats {
    let mut stats = ColStats::empty();
    stats.nan_count = array.null_count() as u64;
    stats
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{Float64Array, Int32Array, StringArray};

    use super::*;
    use crate::schema::FieldSchema;

    fn schema() -> TopicSchema {
        TopicSchema::new(
            "BARO",
            [
                FieldSchema::new("Alt", DataType::Int32, Some("cm"), 0.01).unwrap(),
                FieldSchema::new("Temp", DataType::Float64, Some("C"), 1.0).unwrap(),
                FieldSchema::new("Msg", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn seals_sorted_chunk_with_range_and_stats() {
        let t = Int64Array::from(vec![100, 150, 200]);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![10, 20, 30])),
            Arc::new(Float64Array::from(vec![Some(1.5), Some(f64::NAN), None])),
            Arc::new(StringArray::from(vec![Some("ok"), None, Some("done")])),
        ];

        let chunk = Chunk::try_new(t, cols, &schema()).unwrap();

        assert_eq!(chunk.t_min, 100);
        assert_eq!(chunk.t_max, 200);
        assert_eq!(chunk.len(), 3);
        assert_eq!(
            chunk.stats[0],
            ColStats {
                min: 10.0,
                max: 30.0,
                sum: 60.0,
                sum_sq: 1400.0,
                nan_count: 0,
            }
        );
        assert_eq!(chunk.stats[1].min, 1.5);
        assert_eq!(chunk.stats[1].max, 1.5);
        assert_eq!(chunk.stats[1].sum, 1.5);
        assert_eq!(chunk.stats[1].sum_sq, 2.25);
        assert_eq!(chunk.stats[1].nan_count, 2);
        assert!(chunk.stats[2].min.is_nan());
        assert!(chunk.stats[2].max.is_nan());
        assert_eq!(chunk.stats[2].sum, 0.0);
        assert_eq!(chunk.stats[2].sum_sq, 0.0);
        assert_eq!(chunk.stats[2].nan_count, 1);
    }

    #[test]
    fn allows_duplicate_timestamps_but_rejects_regressions() {
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![10, 20, 30])),
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])),
            Arc::new(StringArray::from(vec!["a", "b", "c"])),
        ];
        assert!(
            Chunk::try_new(
                Int64Array::from(vec![100, 100, 200]),
                cols.clone(),
                &schema()
            )
            .is_ok()
        );

        let err =
            Chunk::try_new(Int64Array::from(vec![100, 90, 200]), cols, &schema()).unwrap_err();
        assert_eq!(
            err,
            ChunkError::TimestampRegression {
                index: 1,
                previous: 100,
                current: 90,
            }
        );
    }

    #[test]
    fn rejects_null_timestamps() {
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![10, 20])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(StringArray::from(vec!["a", "b"])),
        ];
        let err =
            Chunk::try_new(Int64Array::from(vec![Some(100), None]), cols, &schema()).unwrap_err();
        assert_eq!(err, ChunkError::NullTimestamp { index: 1 });
    }

    #[test]
    fn validates_column_shape_against_schema() {
        let short_col: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![10])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(StringArray::from(vec!["a", "b"])),
        ];
        let err =
            Chunk::try_new(Int64Array::from(vec![100, 200]), short_col, &schema()).unwrap_err();
        assert_eq!(
            err,
            ChunkError::ColumnLengthMismatch {
                column: 0,
                expected: 2,
                actual: 1,
            }
        );

        let wrong_type: Vec<ArrayRef> = vec![
            Arc::new(Float64Array::from(vec![10.0, 20.0])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(StringArray::from(vec!["a", "b"])),
        ];
        let err =
            Chunk::try_new(Int64Array::from(vec![100, 200]), wrong_type, &schema()).unwrap_err();
        assert_eq!(
            err,
            ChunkError::ColumnTypeMismatch {
                column: 0,
                expected: DataType::Int32,
                actual: DataType::Float64,
            }
        );
    }
}

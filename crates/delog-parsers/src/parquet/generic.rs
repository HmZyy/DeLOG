use std::sync::Arc;

use arrow::array::{Array, ArrayRef, BooleanArray, Int64Array, UInt32Array};
use arrow::compute::{cast, filter, take};
use arrow::datatypes::{DataType, Schema, TimeUnit};
use arrow::error::ArrowError;
use arrow::record_batch::RecordBatch;
use delog_core::diagnostics::Diag;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::TimeRange;
use delog_parquet_format::{FIELD_DESCRIPTION_KEY, FIELD_MULTIPLIER_KEY, FIELD_UNIT_KEY};
use parquet::arrow::arrow_reader::{ArrowReaderMetadata, ParquetRecordBatchReaderBuilder};

use super::{
    PARQUET_BATCH_ROWS, SeekChunkReader, cancellation_error, parquet_summary, parse_arrow_error,
};
use crate::parser::ParseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeAxis {
    Column { index: usize, unit: TimeUnit },
    RowIndex,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ValueColumn {
    pub index: usize,
    pub micros_from: Option<TimeUnit>,
    pub schema: FieldSchema,
}

#[derive(Debug)]
pub(super) struct ColumnPlan {
    pub axis: TimeAxis,
    pub values: Vec<ValueColumn>,
    pub unsupported: Vec<String>,
}

pub(super) fn timestamp_micros(value: i64, unit: TimeUnit) -> Option<i64> {
    match unit {
        TimeUnit::Second => value.checked_mul(1_000_000),
        TimeUnit::Millisecond => value.checked_mul(1_000),
        TimeUnit::Microsecond => Some(value),
        TimeUnit::Nanosecond => Some(value.div_euclid(1_000)),
    }
}

fn value_dtype(dtype: &DataType) -> Option<(DataType, Option<TimeUnit>)> {
    match dtype {
        DataType::Timestamp(unit, _) => Some((DataType::Int64, Some(*unit))),
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float32
        | DataType::Float64
        | DataType::Boolean
        | DataType::Utf8
        | DataType::LargeUtf8 => Some((dtype.clone(), None)),
        _ => None,
    }
}

pub(super) fn plan_columns(schema: &Schema) -> Result<ColumnPlan, ParseError> {
    let axis = schema
        .fields()
        .iter()
        .enumerate()
        .find_map(|(index, field)| match field.data_type() {
            DataType::Timestamp(unit, _) => Some(TimeAxis::Column { index, unit: *unit }),
            _ => None,
        })
        .unwrap_or(TimeAxis::RowIndex);
    let axis_index = match axis {
        TimeAxis::Column { index, .. } => Some(index),
        TimeAxis::RowIndex => None,
    };

    let mut values = Vec::new();
    let mut unsupported = Vec::new();
    for (index, field) in schema.fields().iter().enumerate() {
        if axis_index == Some(index) {
            continue;
        }
        let Some((dtype, micros_from)) = value_dtype(field.data_type()) else {
            unsupported.push(field.name().to_owned());
            continue;
        };
        let metadata = field.metadata();
        let unit = metadata
            .get(FIELD_UNIT_KEY)
            .filter(|unit| !unit.is_empty())
            .cloned();
        let multiplier = metadata
            .get(FIELD_MULTIPLIER_KEY)
            .and_then(|multiplier| multiplier.parse::<f64>().ok())
            .filter(|multiplier| multiplier.is_finite() && *multiplier != 0.0)
            .unwrap_or(1.0);
        let mut schema = FieldSchema::new(field.name().to_owned(), dtype, unit, multiplier)
            .map_err(|error| ParseError::Setup {
                detail: format!("invalid value field `{}`: {error}", field.name()),
            })?;
        if let Some(description) = metadata
            .get(FIELD_DESCRIPTION_KEY)
            .filter(|description| !description.is_empty())
        {
            schema = schema.with_description(description);
        }
        values.push(ValueColumn {
            index,
            micros_from,
            schema,
        });
    }

    if values.is_empty() {
        return Err(ParseError::Setup {
            detail: "Parquet schema has no supported value columns".to_owned(),
        });
    }

    Ok(ColumnPlan {
        axis,
        values,
        unsupported,
    })
}

struct AxisValues {
    timestamps: Int64Array,
    keep: Option<BooleanArray>,
    skipped: u64,
}

fn raw_i64(array: &dyn Array) -> Result<Int64Array, ArrowError> {
    Ok(cast(array, &DataType::Int64)?
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("cast to Int64 yields Int64")
        .clone())
}

fn axis_values(
    batch: &RecordBatch,
    axis: TimeAxis,
    first_row: u64,
) -> Result<AxisValues, ArrowError> {
    match axis {
        TimeAxis::RowIndex => {
            let timestamps = (0..batch.num_rows())
                .map(|row| (first_row as i64 + row as i64).saturating_mul(1_000_000))
                .collect::<Vec<_>>();
            Ok(AxisValues {
                timestamps: Int64Array::from(timestamps),
                keep: None,
                skipped: 0,
            })
        }
        TimeAxis::Column { index, unit } => {
            let raw = raw_i64(batch.column(index).as_ref())?;
            let mut timestamps = Vec::with_capacity(raw.len());
            let mut keep = Vec::with_capacity(raw.len());
            let mut skipped = 0;
            for row in 0..raw.len() {
                let micros = if raw.is_null(row) {
                    None
                } else {
                    timestamp_micros(raw.value(row), unit)
                };
                match micros {
                    Some(micros) => {
                        timestamps.push(micros);
                        keep.push(true);
                    }
                    None => {
                        keep.push(false);
                        skipped += 1;
                    }
                }
            }
            Ok(AxisValues {
                timestamps: Int64Array::from(timestamps),
                keep: Some(BooleanArray::from(keep)),
                skipped,
            })
        }
    }
}

fn value_array(batch: &RecordBatch, column: &ValueColumn) -> Result<ArrayRef, ArrowError> {
    let array = batch.column(column.index);
    match column.micros_from {
        None => Ok(Arc::clone(array)),
        Some(unit) => {
            let raw = raw_i64(array.as_ref())?;
            let converted = raw
                .iter()
                .map(|value| value.and_then(|value| timestamp_micros(value, unit)))
                .collect::<Int64Array>();
            Ok(Arc::new(converted) as ArrayRef)
        }
    }
}

fn is_sorted(timestamps: &Int64Array) -> bool {
    timestamps
        .values()
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
}

fn sort_by_time(
    timestamps: &Int64Array,
    columns: &[ArrayRef],
) -> Result<(Int64Array, Vec<ArrayRef>), ArrowError> {
    let len = timestamps.len();
    let mut indices: Vec<u32> = (0..len as u32).collect();

    indices.sort_by_key(|&i| (timestamps.value(i as usize), i));

    let indices = UInt32Array::from(indices);

    let sorted = take(timestamps, &indices, None)?
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("take preserves Int64")
        .clone();
    let sorted_columns = columns
        .iter()
        .map(|column| take(column.as_ref(), &indices, None))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sorted, sorted_columns))
}

pub(super) fn parse(
    reader: SeekChunkReader,
    metadata: ArrowReaderMetadata,
    sink: &mut dyn IngestSink,
    ctl: &ParseCtl,
) -> Result<ParseSummary, ParseError> {
    let plan = plan_columns(metadata.schema().as_ref())?;
    let topic_schema = Arc::new(
        TopicSchema::new(
            ctl.label(),
            plan.values
                .iter()
                .map(|value| value.schema.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| ParseError::Setup {
            detail: format!("invalid Parquet topic schema: {error}"),
        })?,
    );

    let total_rows = metadata.metadata().file_metadata().num_rows().max(0) as u64;
    let row_group_count = metadata.metadata().num_row_groups();

    let mut diagnostics = 0;
    if !plan.unsupported.is_empty() {
        diagnostics += 1;
        sink.diagnostic(
            Diag::warning(
                "parquet-unsupported-columns",
                format!(
                    "skipped unsupported Parquet column(s): {}",
                    plan.unsupported.join(", ")
                ),
            )
            .with_source(ctl.source()),
        );
    }

    let mut emitted_any = false;
    let mut row_count = 0u64;
    let mut skipped_rows = 0u64;
    let mut processed_rows = 0u64;
    let mut time_range: Option<TimeRange> = None;

    let parse_result = (|| {
        if ctl.is_cancelled() {
            return Err(cancellation_error(emitted_any));
        }
        for row_group in 0..row_group_count {
            let mut batch_reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                reader.clone(),
                metadata.clone(),
            )
            .with_batch_size(PARQUET_BATCH_ROWS)
            .with_row_groups(vec![row_group])
            .build()
            .map_err(|error| parse_arrow_error(error, emitted_any))?;

            for batch in &mut batch_reader {
                let batch = batch.map_err(|error| parse_arrow_error(error, emitted_any))?;
                let axis = axis_values(&batch, plan.axis, processed_rows)
                    .map_err(|error| parse_arrow_error(error, emitted_any))?;
                skipped_rows += axis.skipped;
                processed_rows += batch.num_rows() as u64;

                if axis.timestamps.is_empty() {
                    ctl.report_fraction(sink, processed_rows as f32 / total_rows.max(1) as f32);
                    if ctl.is_cancelled() {
                        return Err(cancellation_error(emitted_any));
                    }
                    continue;
                }

                let mut columns = plan
                    .values
                    .iter()
                    .map(|column| value_array(&batch, column))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| parse_arrow_error(error, emitted_any))?;
                if let Some(keep) = &axis.keep {
                    columns = columns
                        .iter()
                        .map(|column| filter(column.as_ref(), keep))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| parse_arrow_error(error, emitted_any))?;
                }

                let (timestamps, columns) = if is_sorted(&axis.timestamps) {
                    (axis.timestamps, columns)
                } else {
                    sort_by_time(&axis.timestamps, &columns)
                        .map_err(|error| parse_arrow_error(error, emitted_any))?
                };

                let first = timestamps.value(0);
                let last = timestamps.value(timestamps.len() - 1);
                let batch_range =
                    TimeRange::new(first, last).expect("timestamps are sorted ascending");
                time_range = Some(match time_range {
                    Some(range) => range.union(batch_range),
                    None => batch_range,
                });
                row_count += timestamps.len() as u64;
                sink.submit(ParsedBatch::new(
                    ctl.source(),
                    Arc::clone(&topic_schema),
                    timestamps,
                    columns,
                ));
                emitted_any = true;
                ctl.report_fraction(sink, processed_rows as f32 / total_rows.max(1) as f32);
                if ctl.is_cancelled() {
                    return Err(cancellation_error(emitted_any));
                }
            }
        }
        Ok(())
    })();

    if skipped_rows > 0 {
        diagnostics += 1;
        sink.diagnostic(
            Diag::warning(
                "parquet-skipped-timestamps",
                format!("skipped {skipped_rows} row(s) with unusable timestamps"),
            )
            .with_source(ctl.source()),
        );
    }

    let summary = parquet_summary(emitted_any, row_count, time_range, diagnostics);
    match parse_result {
        Ok(()) => {
            sink.close_source(ctl.source(), summary.clone());
            Ok(summary)
        }
        Err(error) => {
            if emitted_any {
                sink.close_source(ctl.source(), summary);
            }
            Err(error)
        }
    }
}


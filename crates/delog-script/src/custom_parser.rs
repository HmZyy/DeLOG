//! Compatibility conversion for legacy `Parse(raw_data)` file parsers.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
    Int64Array, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;
use numpy::PyReadonlyArray1;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PySequence, PySequenceMethods, PyTuple};

/// A raw file materialized using NumPy `fromfile(..., dtype=float32)` semantics.
pub struct RawFloatFile {
    pub values: Vec<f32>,
    pub input_bytes: u64,
    pub ignored_trailing_bytes: u8,
}

/// Read complete native-endian `f32` values and silently omit an incomplete suffix.
pub fn read_float32_file(path: impl AsRef<Path>) -> std::io::Result<RawFloatFile> {
    let bytes = fs::read(path)?;
    let input_bytes = bytes.len() as u64;
    let ignored_trailing_bytes = (bytes.len() % size_of::<f32>()) as u8;
    // ZC-EXCEPTION: custom Python parser compatibility input copy; off the UI hot
    // path and accounted by `input_bytes` (`python_parser_input_bytes` at emission).
    let values = bytes
        .chunks_exact(size_of::<f32>())
        .map(|chunk| f32::from_ne_bytes(chunk.try_into().expect("four-byte chunk")))
        .collect();
    Ok(RawFloatFile {
        values,
        input_bytes,
        ignored_trailing_bytes,
    })
}

pub struct ParserField {
    pub name: String,
    pub description: String,
    pub values: ArrayRef,
}

pub struct ParserTopic {
    pub name: String,
    pub timestamps: Int64Array,
    pub fields: Vec<ParserField>,
}

pub struct ParserOutput {
    pub topics: Vec<ParserTopic>,
    pub warnings: Vec<String>,
    /// Sum of Arrow's logical array buffers (`Array::get_buffer_memory_size`).
    pub arrow_bytes: u64,
}

struct PendingTopic {
    name: String,
    fields: Vec<PendingField>,
}

struct PendingField {
    full_name: String,
    field_name: String,
    description: String,
    values: ArrayRef,
}

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn numpy_array(py: Python<'_>, full_name: &str, value: &Bound<'_, PyAny>) -> PyResult<ArrayRef> {
    let numpy = py.import("numpy")?;
    let is_masked: bool = numpy
        .getattr("ma")?
        .call_method1("isMaskedArray", (value,))?
        .extract()?;
    if is_masked {
        return Err(value_error(format!(
            "field '{full_name}' values must not be a NumPy MaskedArray"
        )));
    }
    let array = numpy.call_method1("asarray", (value,)).map_err(|error| {
        value_error(format!(
            "field '{full_name}' values must be NumPy-compatible: {error}"
        ))
    })?;
    let ndim: usize = array.getattr("ndim")?.extract()?;
    if ndim != 1 {
        return Err(value_error(format!(
            "field '{full_name}' values must be one-dimensional, got ndim={ndim}"
        )));
    }
    let dtype = array.getattr("dtype")?;
    let kind: char = dtype.getattr("kind")?.extract()?;
    let itemsize: usize = dtype.getattr("itemsize")?.extract()?;
    let dtype_name: String = dtype.getattr("name")?.extract()?;
    let native_dtype = dtype.call_method1("newbyteorder", ("=",))?;
    let contiguous = numpy.call_method1("ascontiguousarray", (&array, native_dtype))?;

    macro_rules! primitive_array {
        ($native:ty, $arrow:ty) => {{
            let array: PyReadonlyArray1<'_, $native> = contiguous.extract().map_err(|_| {
                value_error(format!(
                    "field '{full_name}' could not be read as NumPy dtype {dtype_name}"
                ))
            })?;
            Arc::new(<$arrow>::from(array.as_slice()?.to_vec())) as ArrayRef
        }};
    }

    // ZC-EXCEPTION: custom Python parser compatibility output copy; NumPy owns
    // the source buffer, so Arrow materialization is required and is accounted
    // in `ParserOutput::arrow_bytes` (`python_parser_output_bytes` at emission).
    let output = match (kind, itemsize) {
        ('b', 1) => primitive_array!(bool, BooleanArray),
        ('i', 1) => primitive_array!(i8, Int8Array),
        ('i', 2) => primitive_array!(i16, Int16Array),
        ('i', 4) => primitive_array!(i32, Int32Array),
        ('i', 8) => primitive_array!(i64, Int64Array),
        ('u', 1) => primitive_array!(u8, UInt8Array),
        ('u', 2) => primitive_array!(u16, UInt16Array),
        ('u', 4) => primitive_array!(u32, UInt32Array),
        ('u', 8) => primitive_array!(u64, UInt64Array),
        ('f', 4) => primitive_array!(f32, Float32Array),
        ('f', 8) => primitive_array!(f64, Float64Array),
        _ => {
            return Err(value_error(format!(
                "field '{full_name}' has unsupported NumPy dtype {dtype_name}; expected bool, signed/unsigned 8/16/32/64-bit integer, or float32/float64"
            )));
        }
    };
    Ok(output)
}

fn clock_range_error(field: &PendingField, index: usize) -> PyErr {
    value_error(format!(
        "clock field '{}' value at index {index} is outside the i64 microsecond range",
        field.full_name
    ))
}

fn float_clock_micros(field: &PendingField, index: usize, seconds: f64) -> PyResult<i64> {
    if !seconds.is_finite() {
        return Err(value_error(format!(
            "clock field '{}' value at index {index} must be finite",
            field.full_name
        )));
    }
    let micros = (seconds * 1_000_000.0).round();
    if !micros.is_finite() || !(-(2_f64.powi(63))..2_f64.powi(63)).contains(&micros) {
        return Err(clock_range_error(field, index));
    }
    Ok(micros as i64)
}

fn clock_micros(field: &PendingField, index: usize) -> PyResult<i64> {
    macro_rules! signed {
        ($array:ty) => {{
            let seconds = field
                .values
                .as_any()
                .downcast_ref::<$array>()
                .expect("Arrow dtype and array agree")
                .value(index) as i64;
            seconds
                .checked_mul(1_000_000)
                .ok_or_else(|| clock_range_error(field, index))
        }};
    }
    macro_rules! unsigned {
        ($array:ty) => {{
            let seconds = field
                .values
                .as_any()
                .downcast_ref::<$array>()
                .expect("Arrow dtype and array agree")
                .value(index) as u64;
            seconds
                .checked_mul(1_000_000)
                .and_then(|micros| i64::try_from(micros).ok())
                .ok_or_else(|| clock_range_error(field, index))
        }};
    }

    match field.values.data_type() {
        DataType::Int8 => signed!(Int8Array),
        DataType::Int16 => signed!(Int16Array),
        DataType::Int32 => signed!(Int32Array),
        DataType::Int64 => signed!(Int64Array),
        DataType::UInt8 => unsigned!(UInt8Array),
        DataType::UInt16 => unsigned!(UInt16Array),
        DataType::UInt32 => unsigned!(UInt32Array),
        DataType::UInt64 => unsigned!(UInt64Array),
        DataType::Float32 => {
            let seconds = field
                .values
                .as_any()
                .downcast_ref::<Float32Array>()
                .expect("Arrow dtype and array agree")
                .value(index);
            float_clock_micros(field, index, f64::from(seconds))
        }
        DataType::Float64 => {
            let seconds = field
                .values
                .as_any()
                .downcast_ref::<Float64Array>()
                .expect("Arrow dtype and array agree")
                .value(index);
            float_clock_micros(field, index, seconds)
        }
        DataType::Boolean => Err(value_error(format!(
            "clock field '{}' must be numeric, not Boolean",
            field.full_name
        ))),
        _ => Err(value_error(format!(
            "clock field '{}' must be numeric",
            field.full_name
        ))),
    }
}

fn clock_timestamps(field: &PendingField) -> PyResult<Int64Array> {
    let mut timestamps = Vec::with_capacity(field.values.len());
    for index in 0..field.values.len() {
        let micros = clock_micros(field, index)?;
        if timestamps.last().is_some_and(|previous| *previous > micros) {
            return Err(value_error(format!(
                "clock field '{}' must be non-decreasing (index {index})",
                field.full_name
            )));
        }
        timestamps.push(micros);
    }
    Ok(Int64Array::from(timestamps))
}

/// Validate and convert the complete legacy Python parser return value.
pub fn parse_python_result(py: Python<'_>, result: &Bound<'_, PyAny>) -> PyResult<ParserOutput> {
    let sequence = result
        .cast::<PySequence>()
        .map_err(|_| value_error("parser result must be a sequence of three-item tuples"))?;
    if sequence.is_empty()? {
        return Err(value_error(
            "parser result must contain at least one field; empty results have no topics",
        ));
    }

    let mut topics: Vec<PendingTopic> = Vec::new();
    let mut warnings = Vec::new();
    for entry_index in 0..sequence.len()? {
        let entry = sequence.get_item(entry_index)?;
        let tuple = entry.cast::<PyTuple>().map_err(|_| {
            value_error(format!(
                "parser result entry {entry_index} must be an exact three-item tuple"
            ))
        })?;
        if tuple.len() != 3 {
            return Err(value_error(format!(
                "parser result entry {entry_index} must contain exactly 3 items, got {}",
                tuple.len()
            )));
        }
        let full_name: String = tuple.get_item(0)?.extract().map_err(|_| {
            value_error(format!(
                "parser result entry {entry_index} field name must be a string"
            ))
        })?;
        if full_name.is_empty() {
            return Err(value_error(format!(
                "parser result entry {entry_index} field name must be non-empty"
            )));
        }
        let (topic_name, field_name) = full_name.split_once('.').ok_or_else(|| {
            value_error(format!(
                "field '{full_name}' must contain a topic and field separated by a dot"
            ))
        })?;
        if topic_name.is_empty() || field_name.is_empty() {
            return Err(value_error(format!(
                "field '{full_name}' must have a non-empty topic and field"
            )));
        }
        let description: String = tuple
            .get_item(2)?
            .extract()
            .map_err(|_| value_error(format!("field '{full_name}' tooltip must be a string")))?;
        let values = numpy_array(py, &full_name, &tuple.get_item(1)?)?;

        let topic_index = topics
            .iter()
            .position(|topic| topic.name == topic_name)
            .unwrap_or_else(|| {
                topics.push(PendingTopic {
                    name: topic_name.to_owned(),
                    fields: Vec::new(),
                });
                topics.len() - 1
            });
        let topic = &mut topics[topic_index];
        let pending = PendingField {
            full_name: full_name.clone(),
            field_name: field_name.to_owned(),
            description,
            values,
        };
        if let Some(field_index) = topic
            .fields
            .iter()
            .position(|field| field.full_name == full_name)
        {
            topic.fields[field_index] = pending;
            warnings.push(format!(
                "duplicate parser field '{full_name}'; last occurrence replaced the previous value"
            ));
        } else {
            topic.fields.push(pending);
        }
    }

    let mut output_topics = Vec::with_capacity(topics.len());
    for topic in topics {
        let clock = topic
            .fields
            .iter()
            .find(|field| field.field_name == "rtc")
            .or_else(|| {
                topic
                    .fields
                    .iter()
                    .find(|field| field.field_name == "index")
            })
            .ok_or_else(|| {
                value_error(format!(
                    "topic '{}' is missing clock field '{}.rtc' or '{}.index'",
                    topic.name, topic.name, topic.name
                ))
            })?;
        let timestamps = clock_timestamps(clock)?;
        let expected = timestamps.len();
        for field in &topic.fields {
            if field.values.len() != expected {
                return Err(value_error(format!(
                    "field '{}' has {} values but topic '{}' clock has {expected}",
                    field.full_name,
                    field.values.len(),
                    topic.name
                )));
            }
        }
        output_topics.push(ParserTopic {
            name: topic.name,
            timestamps,
            fields: topic
                .fields
                .into_iter()
                .map(|field| ParserField {
                    name: field.field_name,
                    description: field.description,
                    values: field.values,
                })
                .collect(),
        });
    }

    let arrow_bytes = output_topics
        .iter()
        .map(|topic| {
            topic.timestamps.get_buffer_memory_size() as u64
                + topic
                    .fields
                    .iter()
                    .map(|field| field.values.get_buffer_memory_size() as u64)
                    .sum::<u64>()
        })
        .sum();
    Ok(ParserOutput {
        topics: output_topics,
        warnings,
        arrow_bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use arrow::array::{
        Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
        Int64Array, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
    };
    use arrow::datatypes::DataType;
    use pyo3::prelude::*;

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn temporary_file(bytes: &[u8]) -> std::path::PathBuf {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "delog-custom-parser-{}-{id}.bin",
            std::process::id()
        ));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn parse(source: &str) -> PyResult<ParserOutput> {
        Python::attach(|py| {
            let source = std::ffi::CString::new(source).unwrap();
            let result = py.eval(&source, None, None)?;
            parse_python_result(py, &result)
        })
    }

    fn error(source: &str) -> String {
        match parse(source) {
            Ok(_) => panic!("expected parser result to be rejected"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn reads_native_endian_float32_and_reports_original_bytes() {
        let mut bytes = Vec::new();
        for value in [1.5_f32, -2.25, f32::NAN] {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        let path = temporary_file(&bytes);

        let loaded = read_float32_file(&path).unwrap();

        assert_eq!(loaded.values[..2], [1.5, -2.25]);
        assert!(loaded.values[2].is_nan());
        assert_eq!(loaded.input_bytes, 12);
        assert_eq!(loaded.ignored_trailing_bytes, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn ignores_each_possible_incomplete_float32_suffix_and_accepts_empty_files() {
        for trailing in 0..=3_u8 {
            let mut bytes = 7.25_f32.to_ne_bytes().to_vec();
            bytes.extend(std::iter::repeat_n(0xAB, usize::from(trailing)));
            let path = temporary_file(&bytes);
            let loaded = read_float32_file(&path).unwrap();
            assert_eq!(loaded.values, [7.25]);
            assert_eq!(loaded.input_bytes, u64::from(4 + trailing));
            assert_eq!(loaded.ignored_trailing_bytes, trailing);
            fs::remove_file(path).unwrap();
        }

        let path = temporary_file(&[]);
        let loaded = read_float32_file(&path).unwrap();
        assert!(loaded.values.is_empty());
        assert_eq!(loaded.input_bytes, 0);
        assert_eq!(loaded.ignored_trailing_bytes, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn preserves_all_supported_numpy_dtypes() {
        let output = parse(
            "__import__('builtins').eval(\"[(f't.{n}', __import__('numpy').array([0, 1], dtype=d), '') for n, d in [('index','float64'),('b','bool'),('i8','int8'),('i16','int16'),('i32','int32'),('i64','int64'),('u8','uint8'),('u16','uint16'),('u32','uint32'),('u64','uint64'),('f32','float32'),('f64','float64')]]\")",
        )
        .unwrap();
        let fields = &output.topics[0].fields;
        let actual: Vec<_> = fields
            .iter()
            .map(|field| field.values.data_type())
            .collect();
        assert_eq!(
            actual,
            [
                &DataType::Float64,
                &DataType::Boolean,
                &DataType::Int8,
                &DataType::Int16,
                &DataType::Int32,
                &DataType::Int64,
                &DataType::UInt8,
                &DataType::UInt16,
                &DataType::UInt32,
                &DataType::UInt64,
                &DataType::Float32,
                &DataType::Float64,
            ]
        );
        assert!(
            fields[1]
                .values
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .value(1)
        );
        macro_rules! value_is_one {
            ($index:expr, $ty:ty, $one:expr) => {
                assert_eq!(
                    fields[$index]
                        .values
                        .as_any()
                        .downcast_ref::<$ty>()
                        .unwrap()
                        .value(1),
                    $one
                )
            };
        }
        value_is_one!(2, Int8Array, 1_i8);
        value_is_one!(3, Int16Array, 1_i16);
        value_is_one!(4, Int32Array, 1_i32);
        value_is_one!(5, Int64Array, 1_i64);
        value_is_one!(6, UInt8Array, 1_u8);
        value_is_one!(7, UInt16Array, 1_u16);
        value_is_one!(8, UInt32Array, 1_u32);
        value_is_one!(9, UInt64Array, 1_u64);
        value_is_one!(10, Float32Array, 1_f32);
        value_is_one!(11, Float64Array, 1_f64);
    }

    #[test]
    fn accepts_lists_views_slices_and_noncontiguous_arrays() {
        let output = parse(
            "__import__('builtins').eval(\"[(\'t.index\',[0,1],\'\'),(\'t.list\',[3,4],\'\'),(\'t.view\',a[3:1:-1],\'\'),(\'t.stride\',a[::2],\'\')]\", {\'a\': __import__(\'numpy\').array([10,20,30,40], dtype=\'int16\')})",
        )
        .unwrap();
        let fields = &output.topics[0].fields;
        assert_eq!(fields[1].values.data_type(), &DataType::Int64);
        assert_eq!(
            fields[2]
                .values
                .as_any()
                .downcast_ref::<Int16Array>()
                .unwrap()
                .values(),
            &[40, 30]
        );
        assert_eq!(
            fields[3]
                .values
                .as_any()
                .downcast_ref::<Int16Array>()
                .unwrap()
                .values(),
            &[10, 30]
        );
    }

    #[test]
    fn accepts_supported_non_native_endian_arrays() {
        let output = parse(
            "[('t.index',[0,1],''),('t.big',__import__('numpy').array([258,772],dtype='>i2'),'')]",
        )
        .unwrap();
        let values = output.topics[0].fields[1]
            .values
            .as_any()
            .downcast_ref::<Int16Array>()
            .unwrap();
        assert_eq!(values.values(), &[258, 772]);
    }

    #[test]
    fn groups_topics_and_preserves_first_seen_topic_and_field_order() {
        let output =
            parse("[('z.index',[0],''),('a.index',[0],''),('z.second',[2],''),('z.first',[1],'')]")
                .unwrap();
        assert_eq!(
            output
                .topics
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert_eq!(
            output.topics[0]
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["index", "second", "first"]
        );
    }

    #[test]
    fn prefers_rtc_converts_seconds_and_keeps_both_clocks_visible() {
        let output = parse(
            "[('gps.index',[100,101],''),('gps.v',[5,6],''),('gps.rtc',[1.25,1.250001],'' )]",
        )
        .unwrap();
        assert_eq!(
            output.topics[0].timestamps.values(),
            &[1_250_000, 1_250_001]
        );
        assert_eq!(
            output.topics[0]
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["index", "v", "rtc"]
        );
    }

    #[test]
    fn falls_back_to_index_and_rounds_seconds_to_microseconds() {
        let output = parse("[('t.value',[9,8],''),('t.index',[0.0000006,1.0000004],'')]").unwrap();
        assert_eq!(output.topics[0].timestamps.values(), &[1, 1_000_000]);
    }

    #[test]
    fn integer_clocks_preserve_exact_microseconds_at_large_valid_boundaries() {
        let output = parse(
            "[('signed.index',__import__('numpy').array([-1,0,9000000000001,9223372036854],dtype='int64'),''),('unsigned.index',__import__('numpy').array([9000000000001,9223372036854],dtype='uint64'),'')]",
        )
        .unwrap();
        assert_eq!(
            output.topics[0].timestamps.values(),
            &[
                -1_000_000,
                0,
                9_000_000_000_001_000_000,
                9_223_372_036_854_000_000,
            ]
        );
        assert_eq!(
            output.topics[1].timestamps.values(),
            &[9_000_000_000_001_000_000, 9_223_372_036_854_000_000]
        );
    }

    #[test]
    fn integer_clocks_reject_signed_and_unsigned_multiplication_overflow() {
        for source in [
            "[('t.index',__import__('numpy').array([9223372036855],dtype='int64'),'')]",
            "[('t.index',__import__('numpy').array([9223372036855],dtype='uint64'),'')]",
        ] {
            let message = error(source);
            assert!(message.contains("t.index") && message.contains("range"));
        }
    }

    #[test]
    fn duplicate_last_wins_in_place_and_warns_per_occurrence() {
        let output = parse(
            "[('t.index',[0,1],''),('t.v',[1,2],'old'),('t.x',[8,9],''),('t.v',[3.,4.],'new'),('t.v',[5,6],'newest')]",
        )
        .unwrap();
        assert_eq!(
            output.topics[0]
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["index", "v", "x"]
        );
        let field = &output.topics[0].fields[1];
        assert_eq!(field.description, "newest");
        assert_eq!(field.values.data_type(), &DataType::Int64);
        assert_eq!(output.warnings.len(), 2);
        assert!(output.warnings.iter().all(|w| w.contains("t.v")));
    }

    #[test]
    fn preserves_unicode_trailing_spaces_and_nan_in_ordinary_fields() {
        let output =
            parse("[('航法.index',[0,1],''),('航法.温度  ',[float('nan'),2.0],'説明')]").unwrap();
        assert_eq!(output.topics[0].name, "航法");
        assert_eq!(output.topics[0].fields[1].name, "温度  ");
        assert_eq!(output.topics[0].fields[1].description, "説明");
        assert!(
            output.topics[0].fields[1]
                .values
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(0)
                .is_nan()
        );
    }

    #[test]
    fn accounts_arrow_logical_buffer_bytes_deterministically() {
        let output = parse("[('t.index',[0.,1.,2.],''),('t.flag',[True,False,True],''),('t.v',__import__('numpy').array([1,2,3],dtype='int16'),'')]").unwrap();
        // timestamp i64 (24) + index f64 (24) + packed bool (1) + int16 (6).
        assert_eq!(output.arrow_bytes, 55);
    }

    #[test]
    fn rejects_empty_results_and_invalid_entries() {
        assert!(error("[]").contains("at least one field"));
        assert!(error("[('t.index',[0])]").contains("entry 0"));
        assert!(error("[123]").contains("entry 0"));
        assert!(error("[(1,[0],'')]").contains("entry 0 field name"));
        assert!(
            error("[('t.index',[0],1)]").contains("t.index")
                && error("[('t.index',[0],1)]").contains("tooltip")
        );
        assert!(error("[('',[0],'')]").contains("non-empty"));
    }

    #[test]
    fn rejects_names_without_nonempty_topic_and_field() {
        assert!(error("[('index',[0],'')]").contains("index"));
        assert!(error("[('.index',[0],'')]").contains(".index"));
        assert!(error("[('t.',[0],'')]").contains("t."));
    }

    #[test]
    fn rejects_missing_clock_and_unequal_lengths_with_field_names() {
        assert!(
            error("[('t.v',[1],'')]").contains("t") && error("[('t.v',[1],'')]").contains("clock")
        );
        let message = error("[('t.index',[0,1],''),('t.v',[1],'')]");
        assert!(message.contains("t.v") && message.contains('1') && message.contains('2'));
    }

    #[test]
    fn rejects_unsupported_dtypes_and_non_one_dimensional_values() {
        for (source, field) in [
            ("[('t.index',[0],''),('t.s',['x'],'')]", "t.s"),
            ("[('t.index',[0],''),('t.c',[1+2j],'')]", "t.c"),
            (
                "[('t.index',[0],''),('t.o',__import__('numpy').array([object()],dtype=object),'')]",
                "t.o",
            ),
            (
                "[('t.index',[0],''),('t.v',__import__('numpy').array([[1]]),'')]",
                "t.v",
            ),
        ] {
            assert!(error(source).contains(field));
        }
    }

    #[test]
    fn rejects_masked_arrays_before_exposing_hidden_payloads() {
        let ordinary = error(
            "[('t.index',[0,1],''),('t.v',__import__('numpy').ma.array([1.,float('nan')],mask=[False,True]),'')]",
        );
        assert!(ordinary.contains("t.v") && ordinary.contains("MaskedArray"));

        let clock = error(
            "[('t.index',__import__('numpy').ma.array([0.,float('nan')],mask=[False,True]),''),('t.v',[1,2],'')]",
        );
        assert!(clock.contains("t.index") && clock.contains("MaskedArray"));
    }

    #[test]
    fn rejects_boolean_nonfinite_overflow_and_decreasing_clocks() {
        assert!(error("[('t.index',[False,True],''),('t.v',[1,2],'')]").contains("t.index"));
        let preferred_rtc =
            error("[('t.index',[0,1],''),('t.rtc',[False,True],''),('t.v',[1,2],'')]");
        assert!(preferred_rtc.contains("t.rtc") && preferred_rtc.contains("Boolean"));
        for value in ["float('nan')", "float('inf')", "-float('inf')"] {
            let source = format!("[('t.index',[0,{value}],''),('t.v',[1,2],'')]");
            let message = error(&source);
            assert!(message.contains("t.index") && message.contains("finite"));
        }
        let overflow = error("[('t.index',[1e30],'')]");
        assert!(overflow.contains("t.index") && overflow.contains("range"));
        let decreasing = error("[('t.index',[2.,1.],'')]");
        assert!(decreasing.contains("t.index") && decreasing.contains("non-decreasing"));
    }
}

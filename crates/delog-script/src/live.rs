use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use delog_core::field_view::{array_row_as_f64, array_row_as_str};
use delog_core::identity::SourceId;
use delog_core::ingest::ParsedBatch;
use delog_core::schema::{FieldSchema, TopicSchema};

use numpy::{IntoPyArray, PyArray1, PyReadonlyArray1};
use pyo3::exceptions::{PyAttributeError, PyValueError};
use pyo3::prelude::*;

use crate::api::{PendingColumn, PendingField};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTransformSpec {
    pub script_name: String,
    /// The decorated function's `__name__`; set at registration.
    pub func_name: String,
    pub generation: u64,
    pub topic: String,
    pub fields: Vec<String>,
    /// `None` selects dynamic mode: the callback returns `{topic: {field: ...}}`.
    pub output_topic: Option<String>,
}

impl LiveTransformSpec {
    pub fn new(
        script_name: String,
        generation: u64,
        topic: String,
        fields: Vec<String>,
        output_topic: Option<String>,
    ) -> Result<Self, String> {
        if topic.is_empty() {
            return Err("live_transform topic must not be empty".into());
        }
        if fields.is_empty() {
            return Err("live_transform fields must not be empty".into());
        }
        if output_topic.as_deref() == Some("") {
            return Err("live_transform output_topic must not be empty".into());
        }
        Ok(Self {
            script_name,
            func_name: String::new(),
            generation,
            topic,
            fields,
            output_topic,
        })
    }

    /// Identity for error messages: `script.function`.
    pub fn label(&self) -> String {
        format!("{}.{}", self.script_name, self.func_name)
    }

    pub fn matches(&self, batch: &ParsedBatch) -> bool {
        batch.topic() == self.topic
            && self
                .fields
                .iter()
                .all(|field| batch.schema.field_index(field).is_some())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveColumn {
    F64(Vec<f64>),
    Str(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTransformBatch {
    pub times: Vec<i64>,
    pub values: HashMap<String, LiveColumn>,
}

impl LiveTransformBatch {
    pub fn from_parsed(spec: &LiveTransformSpec, batch: &ParsedBatch) -> Result<Self, String> {
        if !spec.matches(batch) {
            return Err(format!(
                "batch topic '{}' does not satisfy live transform '{}'",
                batch.topic(),
                spec.label()
            ));
        }
        let times: Vec<i64> = (0..batch.timestamps.len())
            .map(|row| batch.timestamps.value(row))
            .collect();
        let mut values = HashMap::new();
        for field in &spec.fields {
            let idx = batch
                .schema
                .field_index(field)
                .ok_or_else(|| format!("field '{field}' missing from {}", batch.topic()))?;
            let col = batch.columns[idx].as_ref();
            let vals = match col.data_type() {
                DataType::Utf8 | DataType::LargeUtf8 => LiveColumn::Str(
                    (0..batch.timestamps.len())
                        .map(|row| array_row_as_str(col, row).unwrap_or_default().to_owned())
                        .collect(),
                ),
                _ => LiveColumn::F64(
                    (0..batch.timestamps.len())
                        .map(|row| array_row_as_f64(col, row))
                        .collect(),
                ),
            };
            values.insert(field.clone(), vals);
        }
        Ok(Self { times, values })
    }
}

/// Exposes `.t` (int64 us) and one attribute per requested field: float64
/// arrays for numeric fields, numpy unicode arrays for string fields.
#[pyclass(unsendable, name = "LiveBatch")]
pub struct LiveBatchPy {
    #[pyo3(get)]
    pub t: Py<PyArray1<i64>>,
    fields: HashMap<String, Py<PyAny>>,
}

impl LiveBatchPy {
    /// Must be called under the GIL.
    pub fn from_materialized(py: Python<'_>, batch: LiveTransformBatch) -> PyResult<Self> {
        let t = batch.times.into_pyarray(py).unbind();
        let mut fields = HashMap::with_capacity(batch.values.len());
        for (name, col) in batch.values {
            let obj: Py<PyAny> = match col {
                LiveColumn::F64(vals) => vals.into_pyarray(py).into_any().unbind(),
                LiveColumn::Str(vals) => crate::api::numpy_str_array(py, vals)?,
            };
            fields.insert(name, obj);
        }
        Ok(Self { t, fields })
    }
}

#[pymethods]
impl LiveBatchPy {
    fn __getattr__(&self, name: &str) -> PyResult<Py<PyAny>> {
        self.fields
            .get(name)
            .map(|obj| Python::attach(|py| obj.clone_ref(py)))
            .ok_or_else(|| PyAttributeError::new_err(name.to_owned()))
    }
}

pub struct LiveTransformResult {
    pub topic: String,
    pub times: Vec<i64>,
    pub fields: Vec<PendingField>,
}

/// `length_context` names what `expected` counts, for the length-mismatch
/// message: callers pass `"the batch"` when `expected` is the input batch's
/// row count, or `"its times array"` when it is an explicit times array's
/// length (the dynamic 3-tuple form, where those can differ).
fn read_f64_array(
    py: Python<'_>,
    field: &str,
    obj: &Bound<'_, PyAny>,
    expected: usize,
    length_context: &str,
) -> PyResult<Vec<f64>> {
    let arr: PyReadonlyArray1<f64> = obj.extract().map_err(|_| {
        PyValueError::new_err(format!(
            "live transform field '{field}' must be a 1-D float array"
        ))
    })?;
    let vals = arr.as_slice()?.to_vec();
    if vals.len() != expected {
        return Err(PyValueError::new_err(format!(
            "live transform field '{field}' produced {} values but {length_context} has {expected}",
            vals.len()
        )));
    }
    let _ = py;
    Ok(vals)
}

enum ParsedTimes {
    /// Reuse the input batch's times.
    Default,
    Explicit(Vec<i64>),
}

fn extract_unit(name: &str, obj: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    obj.extract().map_err(|_| {
        PyValueError::new_err(format!(
            "live transform field '{name}' unit must be a string or None"
        ))
    })
}

/// One field entry: `values`, `(values, unit)`, or `(times, values, unit)`.
/// Static mode (`dynamic == false`) requires explicit times to equal the
/// input batch's; dynamic mode requires them sorted non-decreasing.
fn parse_field_entry(
    py: Python<'_>,
    label: &str,
    name: &str,
    value: &Bound<'_, PyAny>,
    input_times: &[i64],
    dynamic: bool,
) -> PyResult<(ParsedTimes, Vec<f64>, Option<String>)> {
    // A numpy array is never a tuple, so this distinguishes a tuple form from a bare array.
    if let Ok(tuple) = value.cast::<pyo3::types::PyTuple>() {
        match tuple.len() {
            2 => {
                let values = read_f64_array(
                    py,
                    name,
                    &tuple.get_item(0)?,
                    input_times.len(),
                    "the batch",
                )?;
                let unit = extract_unit(name, &tuple.get_item(1)?)?;
                Ok((ParsedTimes::Default, values, unit))
            }
            3 => {
                let times: Vec<i64> = tuple
                    .get_item(0)?
                    .extract::<PyReadonlyArray1<i64>>()
                    .map_err(|_| {
                        PyValueError::new_err(format!(
                            "live transform field '{name}' times must be a 1-D int64 array"
                        ))
                    })?
                    .as_slice()?
                    .to_vec();
                let length_context = if dynamic {
                    if times.windows(2).any(|w| w[0] > w[1]) {
                        return Err(PyValueError::new_err(format!(
                            "live transform '{label}' field '{name}' times must be sorted \
                             non-decreasing"
                        )));
                    }
                    "its times array"
                } else {
                    if times != input_times {
                        return Err(PyValueError::new_err(format!(
                            "live transform field '{name}' supplied times that differ from the \
                             input batch times (same-topic transforms must preserve timestamps)"
                        )));
                    }
                    "the batch"
                };
                let values =
                    read_f64_array(py, name, &tuple.get_item(1)?, times.len(), length_context)?;
                let unit = extract_unit(name, &tuple.get_item(2)?)?;
                Ok((ParsedTimes::Explicit(times), values, unit))
            }
            n => Err(PyValueError::new_err(format!(
                "live transform field '{name}' tuple must be (values, unit) or \
                 (times, values, unit), got a {n}-tuple"
            ))),
        }
    } else {
        Ok((
            ParsedTimes::Default,
            read_f64_array(py, name, value, input_times.len(), "the batch")?,
            None,
        ))
    }
}

/// Parse one topic's `{field: ...}` dict; all fields must resolve to
/// identical times.
fn parse_topic_fields(
    py: Python<'_>,
    label: &str,
    dict: &Bound<'_, pyo3::types::PyDict>,
    input_times: &[i64],
    dynamic: bool,
) -> PyResult<(Vec<i64>, Vec<PendingField>)> {
    let mut topic_times: Option<Vec<i64>> = None;
    let mut fields = Vec::with_capacity(dict.len());
    for (key, value) in dict.iter() {
        let name: String = key.extract().map_err(|_| {
            PyValueError::new_err(format!(
                "live transform '{label}' field names must be strings"
            ))
        })?;
        let (times, values, unit) =
            parse_field_entry(py, label, &name, &value, input_times, dynamic)?;
        let resolved = match times {
            ParsedTimes::Default => input_times.to_vec(),
            ParsedTimes::Explicit(t) => t,
        };
        match &topic_times {
            None => topic_times = Some(resolved),
            Some(existing) if *existing == resolved => {}
            Some(_) => {
                return Err(PyValueError::new_err(format!(
                    "live transform '{label}': all fields of one output topic must share \
                     identical times (field '{name}' differs)"
                )));
            }
        }
        fields.push(PendingField::numeric(name, values, unit));
    }
    // Defensive fallback only: both callers of `parse_topic_fields` reject an
    // empty dict before calling it, so `dict` always has at least one entry
    // and `topic_times` is always `Some` by this point.
    Ok((topic_times.unwrap_or_else(|| input_times.to_vec()), fields))
}

/// Static mode returns one result for `spec.output_topic`; dynamic mode
/// (`output_topic == None`) accepts `{topic: {field: ...}}` and returns one
/// result per non-empty topic. An empty outer dict in dynamic mode means
/// "nothing to emit for this batch".
pub fn parse_transform_result(
    py: Python<'_>,
    spec: &LiveTransformSpec,
    input_times: &[i64],
    obj: &Bound<'_, PyAny>,
) -> PyResult<Vec<LiveTransformResult>> {
    let label = spec.label();
    let shape = if spec.output_topic.is_some() {
        "{field: values}"
    } else {
        "{topic: {field: values}}"
    };
    let dict = obj.cast::<pyo3::types::PyDict>().map_err(|_| {
        PyValueError::new_err(format!(
            "live transform '{label}' must return a dict of {shape}"
        ))
    })?;

    match &spec.output_topic {
        Some(topic) => {
            if dict.is_empty() {
                return Err(PyValueError::new_err(format!(
                    "live transform '{label}' returned an empty result; expected at least \
                     one output field"
                )));
            }
            let (times, fields) = parse_topic_fields(py, &label, dict, input_times, false)?;
            Ok(vec![LiveTransformResult {
                topic: topic.clone(),
                times,
                fields,
            }])
        }
        None => {
            let mut results = Vec::with_capacity(dict.len());
            for (key, value) in dict.iter() {
                let topic: String = key.extract().map_err(|_| {
                    PyValueError::new_err(format!(
                        "live transform '{label}' topic names must be strings"
                    ))
                })?;
                if topic.is_empty() {
                    return Err(PyValueError::new_err(format!(
                        "live transform '{label}' topic names must not be empty"
                    )));
                }
                let inner = value.cast::<pyo3::types::PyDict>().map_err(|_| {
                    PyValueError::new_err(format!(
                        "live transform '{label}' must map topic '{topic}' to a dict of \
                         {{field: values}}"
                    ))
                })?;
                if inner.is_empty() {
                    return Err(PyValueError::new_err(format!(
                        "live transform '{label}' topic '{topic}' has no fields"
                    )));
                }
                let (times, fields) = parse_topic_fields(py, &label, inner, input_times, true)?;
                if times.is_empty() {
                    continue; // zero-row topic: nothing to append this batch
                }
                results.push(LiveTransformResult {
                    topic,
                    times,
                    fields,
                });
            }
            Ok(results)
        }
    }
}

pub fn result_to_batch(
    source: SourceId,
    result: LiveTransformResult,
) -> Result<ParsedBatch, String> {
    let fields = result
        .fields
        .iter()
        .map(|f| {
            let dtype = match &f.values {
                PendingColumn::F64(_) => DataType::Float64,
                PendingColumn::Utf8(_) => DataType::Utf8,
            };
            FieldSchema::new(f.name.clone(), dtype, f.unit.clone(), 1.0)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let schema = Arc::new(TopicSchema::new(result.topic, fields).map_err(|e| e.to_string())?);
    let timestamps = Int64Array::from(result.times);
    let columns: Vec<ArrayRef> = result
        .fields
        .into_iter()
        .map(|f| match f.values {
            PendingColumn::F64(values) => Arc::new(Float64Array::from(values)) as ArrayRef,
            PendingColumn::Utf8(values) => Arc::new(StringArray::from(values)) as ArrayRef,
        })
        .collect();
    Ok(ParsedBatch::new(source, schema, timestamps, columns))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float32Array, Int16Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::identity::SourceId;
    use delog_core::ingest::ParsedBatch;
    use delog_core::schema::{FieldSchema, TopicSchema};

    use super::*;

    fn nav_batch() -> ParsedBatch {
        let schema = Arc::new(
            TopicSchema::new(
                "NAV_CONTROLLER_OUTPUT",
                [
                    FieldSchema::new("nav_roll", DataType::Float32, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("nav_pitch", DataType::Float32, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("nav_bearing", DataType::Int16, Some("deg"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Float32Array::from(vec![0.0, 90.0])),
            Arc::new(Float32Array::from(vec![45.0, -45.0])),
            Arc::new(Int16Array::from(vec![180, -90])),
        ];
        ParsedBatch::new(
            SourceId(7),
            schema,
            Int64Array::from(vec![100, 200]),
            columns,
        )
    }

    fn named_batch() -> ParsedBatch {
        let schema = Arc::new(
            TopicSchema::new(
                "NAMED_VALUE_FLOAT",
                [
                    FieldSchema::new("name", DataType::Utf8, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("value", DataType::Float32, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(arrow::array::StringArray::from(vec![
                Some("airspd"),
                None,
                Some("clbrate"),
            ])),
            Arc::new(Float32Array::from(vec![1.5, 2.5, 3.5])),
        ];
        ParsedBatch::new(
            SourceId(7),
            schema,
            Int64Array::from(vec![100, 200, 300]),
            columns,
        )
    }

    #[test]
    fn spec_matches_topic_and_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            Some("NAV_RAD".into()),
        )
        .unwrap();

        assert!(spec.matches(&nav_batch()));
    }

    #[test]
    fn spec_rejects_missing_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["missing".into()],
            Some("NAV_RAD".into()),
        )
        .unwrap();

        assert!(!spec.matches(&nav_batch()));
    }

    #[test]
    fn materialize_batch_widens_numeric_fields_to_f64() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            Some("NAV_RAD".into()),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &nav_batch()).unwrap();

        assert_eq!(materialized.times, vec![100, 200]);
        assert_eq!(
            materialized.values["nav_roll"],
            LiveColumn::F64(vec![0.0, 90.0])
        );
        assert_eq!(
            materialized.values["nav_bearing"],
            LiveColumn::F64(vec![180.0, -90.0])
        );
    }

    #[test]
    fn spec_accepts_dynamic_mode_and_labels_by_function() {
        let mut spec = LiveTransformSpec::new(
            "named_values".into(),
            1,
            "NAMED_VALUE_FLOAT".into(),
            vec!["name".into(), "value".into()],
            None,
        )
        .unwrap();
        spec.func_name = "split_floats".into();
        assert_eq!(spec.output_topic, None);
        assert_eq!(spec.label(), "named_values.split_floats");

        assert!(
            LiveTransformSpec::new(
                "s".into(),
                1,
                "T".into(),
                vec!["v".into()],
                Some(String::new())
            )
            .is_err()
        );
    }

    fn dynamic_spec() -> LiveTransformSpec {
        let mut spec = LiveTransformSpec::new(
            "named_values".into(),
            1,
            "NAMED_VALUE_FLOAT".into(),
            vec!["name".into(), "value".into()],
            None,
        )
        .unwrap();
        spec.func_name = "split".into();
        spec
    }

    #[test]
    fn dynamic_result_parses_per_topic_row_subsets() {
        pyo3::Python::attach(|py| {
            use numpy::IntoPyArray;
            use pyo3::types::{IntoPyDict, PyDict};
            let inner = PyDict::new(py);
            inner
                .set_item(
                    "value",
                    (
                        vec![100_i64, 300].into_pyarray(py),
                        vec![1.5_f64, 3.5].into_pyarray(py),
                        Option::<String>::None,
                    ),
                )
                .unwrap();
            let outer = [("NAMED_VALUE_FLOAT/airspd", inner)]
                .into_py_dict(py)
                .unwrap();

            let results =
                parse_transform_result(py, &dynamic_spec(), &[100, 200, 300], outer.as_any())
                    .unwrap();

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].topic, "NAMED_VALUE_FLOAT/airspd");
            assert_eq!(results[0].times, vec![100, 300]);
            assert_eq!(
                results[0].fields[0].values,
                PendingColumn::F64(vec![1.5, 3.5])
            );
        });
    }

    #[test]
    fn dynamic_result_allows_empty_dict_and_skips_zero_row_topics() {
        pyo3::Python::attach(|py| {
            use numpy::IntoPyArray;
            use pyo3::types::{IntoPyDict, PyDict};
            let empty = PyDict::new(py);
            let results =
                parse_transform_result(py, &dynamic_spec(), &[100], empty.as_any()).unwrap();
            assert!(results.is_empty());

            let inner = PyDict::new(py);
            inner
                .set_item(
                    "value",
                    (
                        Vec::<i64>::new().into_pyarray(py),
                        Vec::<f64>::new().into_pyarray(py),
                        Option::<String>::None,
                    ),
                )
                .unwrap();
            let outer = [("NAMED_VALUE_FLOAT/quiet", inner)]
                .into_py_dict(py)
                .unwrap();
            let results =
                parse_transform_result(py, &dynamic_spec(), &[100], outer.as_any()).unwrap();
            assert!(results.is_empty());
        });
    }

    #[test]
    fn dynamic_result_rejects_unsorted_times_and_mismatched_topic_times() {
        pyo3::Python::attach(|py| {
            use numpy::IntoPyArray;
            use pyo3::types::{IntoPyDict, PyDict};
            // unsorted explicit times
            let inner = PyDict::new(py);
            inner
                .set_item(
                    "value",
                    (
                        vec![300_i64, 100].into_pyarray(py),
                        vec![1.0_f64, 2.0].into_pyarray(py),
                        Option::<String>::None,
                    ),
                )
                .unwrap();
            let outer = [("T/a", inner)].into_py_dict(py).unwrap();
            assert!(
                parse_transform_result(py, &dynamic_spec(), &[100, 300], outer.as_any()).is_err()
            );

            // two fields of one topic with different times
            let inner = PyDict::new(py);
            inner
                .set_item(
                    "a",
                    (
                        vec![100_i64].into_pyarray(py),
                        vec![1.0_f64].into_pyarray(py),
                        Option::<String>::None,
                    ),
                )
                .unwrap();
            inner
                .set_item(
                    "b",
                    (
                        vec![300_i64].into_pyarray(py),
                        vec![2.0_f64].into_pyarray(py),
                        Option::<String>::None,
                    ),
                )
                .unwrap();
            let outer = [("T/a", inner)].into_py_dict(py).unwrap();
            assert!(
                parse_transform_result(py, &dynamic_spec(), &[100, 300], outer.as_any()).is_err()
            );

            // inner value not a dict
            let outer = PyDict::new(py);
            outer.set_item("T/a", 42).unwrap();
            assert!(parse_transform_result(py, &dynamic_spec(), &[100], outer.as_any()).is_err());
        });
    }

    #[test]
    fn static_result_still_returns_single_topic() {
        pyo3::Python::attach(|py| {
            use numpy::IntoPyArray;
            use pyo3::types::IntoPyDict;
            let mut spec = dynamic_spec();
            spec.output_topic = Some("OUT".into());
            let dict = [("x", vec![1.0_f64, 2.0].into_pyarray(py))]
                .into_py_dict(py)
                .unwrap();
            let results = parse_transform_result(py, &spec, &[100, 200], dict.as_any()).unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].topic, "OUT");
            assert_eq!(results[0].times, vec![100, 200]);
        });
    }

    #[test]
    fn materialize_batch_extracts_string_fields_with_empty_for_null() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAMED_VALUE_FLOAT".into(),
            vec!["name".into(), "value".into()],
            Some("OUT".into()),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &named_batch()).unwrap();

        assert_eq!(
            materialized.values["name"],
            LiveColumn::Str(vec!["airspd".into(), "".into(), "clbrate".into()])
        );
        assert_eq!(
            materialized.values["value"],
            LiveColumn::F64(vec![1.5, 2.5, 3.5])
        );
    }
}

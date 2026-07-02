use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::field_view::{array_row_as_f64, array_row_as_str};
use delog_core::identity::SourceId;
use delog_core::ingest::ParsedBatch;
use delog_core::schema::{FieldSchema, TopicSchema};

use numpy::{IntoPyArray, PyArray1, PyReadonlyArray1};
use pyo3::exceptions::{PyAttributeError, PyValueError};
use pyo3::prelude::*;

use crate::api::PendingField;

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

fn read_f64_array(
    py: Python<'_>,
    field: &str,
    obj: &Bound<'_, PyAny>,
    expected: usize,
) -> PyResult<Vec<f64>> {
    let arr: PyReadonlyArray1<f64> = obj.extract().map_err(|_| {
        PyValueError::new_err(format!(
            "live transform field '{field}' must be a 1-D float array"
        ))
    })?;
    let vals = arr.as_slice()?.to_vec();
    if vals.len() != expected {
        return Err(PyValueError::new_err(format!(
            "live transform field '{field}' produced {} values but the batch has {expected}",
            vals.len()
        )));
    }
    let _ = py;
    Ok(vals)
}

/// Accepts a dict of field -> `values`, `(values, unit)`, or `(times, values, unit)`;
/// supplied times must equal `input_times`.
pub fn parse_transform_result(
    py: Python<'_>,
    spec: &LiveTransformSpec,
    input_times: &[i64],
    obj: &Bound<'_, PyAny>,
) -> PyResult<LiveTransformResult> {
    let dict = obj.cast::<pyo3::types::PyDict>().map_err(|_| {
        PyValueError::new_err(format!(
            "live transform '{}' must return a dict of {{field: values}}",
            spec.label()
        ))
    })?;

    if dict.is_empty() {
        return Err(PyValueError::new_err(format!(
            "live transform '{}' returned an empty result; expected at least one output field",
            spec.label()
        )));
    }

    let expected = input_times.len();
    let mut fields = Vec::with_capacity(dict.len());

    for (key, value) in dict.iter() {
        let name: String = key.extract().map_err(|_| {
            PyValueError::new_err(format!(
                "live transform '{}' field names must be strings",
                spec.label()
            ))
        })?;

        // A numpy array is never a tuple, so this distinguishes a tuple form from a bare array.
        let (values, unit) = if let Ok(tuple) = value.cast::<pyo3::types::PyTuple>() {
            match tuple.len() {
                2 => {
                    let values = read_f64_array(py, &name, &tuple.get_item(0)?, expected)?;
                    let unit: Option<String> = tuple.get_item(1)?.extract().map_err(|_| {
                        PyValueError::new_err(format!(
                            "live transform field '{name}' unit must be a string or None"
                        ))
                    })?;
                    (values, unit)
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
                    if times != input_times {
                        return Err(PyValueError::new_err(format!(
                            "live transform field '{name}' supplied times that differ from the \
                             input batch times (same-topic transforms must preserve timestamps)"
                        )));
                    }
                    let values = read_f64_array(py, &name, &tuple.get_item(1)?, expected)?;
                    let unit: Option<String> = tuple.get_item(2)?.extract().map_err(|_| {
                        PyValueError::new_err(format!(
                            "live transform field '{name}' unit must be a string or None"
                        ))
                    })?;
                    (values, unit)
                }
                n => {
                    return Err(PyValueError::new_err(format!(
                        "live transform field '{name}' tuple must be (values, unit) or \
                         (times, values, unit), got a {n}-tuple"
                    )));
                }
            }
        } else {
            (read_f64_array(py, &name, &value, expected)?, None)
        };

        fields.push(PendingField { name, values, unit });
    }

    Ok(LiveTransformResult {
        // Dynamic mode (spec.output_topic == None) is parsed by a later task;
        // this function still assumes a static output topic.
        topic: spec.output_topic.clone().unwrap_or_default(),
        times: input_times.to_vec(),
        fields,
    })
}

pub fn result_to_batch(
    source: SourceId,
    result: LiveTransformResult,
) -> Result<ParsedBatch, String> {
    let fields = result
        .fields
        .iter()
        .map(|f| FieldSchema::new(f.name.clone(), DataType::Float64, f.unit.clone(), 1.0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let schema = Arc::new(TopicSchema::new(result.topic, fields).map_err(|e| e.to_string())?);
    let timestamps = Int64Array::from(result.times);
    let columns: Vec<ArrayRef> = result
        .fields
        .into_iter()
        .map(|f| Arc::new(Float64Array::from(f.values)) as ArrayRef)
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
            LiveTransformSpec::new("s".into(), 1, "T".into(), vec!["v".into()], Some(String::new()))
                .is_err()
        );
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

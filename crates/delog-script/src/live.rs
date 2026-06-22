//! Live Python transform support: same-topic batch transforms for scripts.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::field_view::array_row_as_f64;
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
    pub generation: u64,
    pub topic: String,
    pub fields: Vec<String>,
    pub output_topic: String,
}

impl LiveTransformSpec {
    pub fn new(
        script_name: String,
        generation: u64,
        topic: String,
        fields: Vec<String>,
        output_topic: String,
    ) -> Result<Self, String> {
        if topic.is_empty() {
            return Err("live_transform topic must not be empty".into());
        }
        if fields.is_empty() {
            return Err("live_transform fields must not be empty".into());
        }
        if output_topic.is_empty() {
            return Err("live_transform output_topic must not be empty".into());
        }
        Ok(Self {
            script_name,
            generation,
            topic,
            fields,
            output_topic,
        })
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
pub struct LiveTransformBatch {
    pub times: Vec<i64>,
    pub values: HashMap<String, Vec<f64>>,
}

impl LiveTransformBatch {
    /// Copies the incoming live batch into contiguous f64 buffers for
    /// CPython/numpy; off the render hot path.
    pub fn from_parsed(spec: &LiveTransformSpec, batch: &ParsedBatch) -> Result<Self, String> {
        if !spec.matches(batch) {
            return Err(format!(
                "batch topic '{}' does not satisfy live transform '{}'",
                batch.topic(),
                spec.output_topic
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
            let vals = (0..batch.timestamps.len())
                .map(|row| array_row_as_f64(col, row))
                .collect();
            values.insert(field.clone(), vals);
        }
        Ok(Self { times, values })
    }
}

/// The `batch` object handed to a live transform callable. Exposes `.t`
/// (int64 µs numpy array) and a dynamic float64 numpy attribute per requested
/// field (e.g. `batch.nav_roll`).
#[pyclass(unsendable, name = "LiveBatch")]
pub struct LiveBatchPy {
    #[pyo3(get)]
    pub t: Py<PyArray1<i64>>,
    fields: HashMap<String, Py<PyArray1<f64>>>,
}

impl LiveBatchPy {
    /// Build the Python batch object from a materialized live batch. Must be
    /// called under the GIL.
    pub fn from_materialized(py: Python<'_>, batch: LiveTransformBatch) -> Self {
        let t = batch.times.into_pyarray(py).unbind();
        let fields = batch
            .values
            .into_iter()
            .map(|(name, vals)| (name, vals.into_pyarray(py).unbind()))
            .collect();
        Self { t, fields }
    }
}

#[pymethods]
impl LiveBatchPy {
    fn __getattr__(&self, name: &str) -> PyResult<Py<PyArray1<f64>>> {
        self.fields
            .get(name)
            .map(|arr| Python::attach(|py| arr.clone_ref(py)))
            .ok_or_else(|| PyAttributeError::new_err(name.to_owned()))
    }
}

/// The parsed return value of one live transform call: a derived topic with
/// its timestamps and float64 fields, ready for emission.
pub struct LiveTransformResult {
    pub topic: String,
    pub times: Vec<i64>,
    pub fields: Vec<PendingField>,
}

/// Read a Python object as a contiguous f64 vector (any array-like numpy can
/// view as float64), validating its length against `expected`.
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

/// Parse a live transform's return value. Accepts a dict mapping output field
/// name -> one of:
///   * `values` (array-like, length N) — unit None, times = `input_times`
///   * `(values, unit)` — explicit unit, times = `input_times`
///   * `(times, values, unit)` — explicit times (must equal `input_times`)
pub fn parse_transform_result(
    py: Python<'_>,
    spec: &LiveTransformSpec,
    input_times: &[i64],
    obj: &Bound<'_, PyAny>,
) -> PyResult<LiveTransformResult> {
    let dict = obj.cast::<pyo3::types::PyDict>().map_err(|_| {
        PyValueError::new_err(format!(
            "live transform '{}' must return a dict of {{field: values}}",
            spec.output_topic
        ))
    })?;

    if dict.is_empty() {
        return Err(PyValueError::new_err(format!(
            "live transform '{}' returned an empty result; expected at least one output field",
            spec.output_topic
        )));
    }

    let expected = input_times.len();
    let mut fields = Vec::with_capacity(dict.len());

    for (key, value) in dict.iter() {
        let name: String = key.extract().map_err(|_| {
            PyValueError::new_err(format!(
                "live transform '{}' field names must be strings",
                spec.output_topic
            ))
        })?;

        // Distinguish a tuple `(values, unit)` / `(times, values, unit)` from a
        // bare array. A numpy array is not a tuple, so this is unambiguous.
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
        topic: spec.output_topic.clone(),
        times: input_times.to_vec(),
        fields,
    })
}

/// Turn one parsed live transform result into a `ParsedBatch` on the given
/// derived source.
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

    #[test]
    fn spec_matches_topic_and_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            "NAV_RAD".into(),
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
            "NAV_RAD".into(),
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
            "NAV_RAD".into(),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &nav_batch()).unwrap();

        assert_eq!(materialized.times, vec![100, 200]);
        assert_eq!(materialized.values["nav_roll"], vec![0.0, 90.0]);
        assert_eq!(materialized.values["nav_bearing"], vec![180.0, -90.0]);
    }
}

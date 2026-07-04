use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use delog_core::field_view::FieldView;
use delog_core::field_view::array_row_as_f64;
use delog_core::field_view::array_row_as_str;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use numpy::{IntoPyArray, PyArray1, PyArrayMethods};
use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::live::LiveTransformSpec;
use crate::params::{ParamKind, ParamSpec, ParamValue, SharedParams};
use pyo3::types::{PyBool, PyInt};

pub struct PendingLiveTransform {
    pub spec: LiveTransformSpec,
    pub callable: Py<PyAny>,
}

pub type LiveTransformBuffer = Rc<RefCell<Vec<PendingLiveTransform>>>;

/// For each `base` time, the source value at the latest source timestamp
/// `<= base` (NaN before the first sample). `src_t` must be sorted ascending.
pub fn resample_prev(src_t: &[i64], src_v: &[f64], base: &[i64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(base.len());
    for &bt in base {
        let idx = match src_t.binary_search(&bt) {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        };
        out.push(idx.map(|i| src_v[i]).unwrap_or(f64::NAN));
    }
    out
}

/// `(times_us, values, strings)`; `strings` is `Some` only for Utf8/LargeUtf8
/// fields, with null cells materialized as `""`.
pub type MaterializedField = (Vec<i64>, Vec<f64>, Option<Vec<String>>);

/// Materialize a field as `(times_us, values, strings)` by walking its chunks
/// in time order. Concatenates chunk buffers — the one copy for script
/// consumption.
pub fn materialize_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
) -> Result<MaterializedField, String> {
    let view = FieldView::new(snapshot, field).map_err(|e| e.to_string())?;
    let col = view.col_index();
    let range = snapshot
        .global_time_range()
        .ok_or_else(|| "field has no data".to_string())?;
    let mut times = Vec::new();
    let mut values = Vec::new();
    let mut strings = view.schema_field().is_string().then(Vec::new);
    for chunk in view.chunks_overlapping(range) {
        for row in 0..chunk.len() {
            times.push(chunk.t.value(row));
            values.push(array_row_as_f64(chunk.cols[col].as_ref(), row));
            if let Some(s) = &mut strings {
                s.push(
                    array_row_as_str(chunk.cols[col].as_ref(), row)
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
        }
    }
    Ok((times, values, strings))
}

/// A numpy unicode ('<U...') array from owned strings, so scripts get
/// vectorized comparisons like `batch.name == "airspd"`.
pub(crate) fn numpy_str_array(py: Python<'_>, vals: Vec<String>) -> PyResult<Py<PyAny>> {
    use pyo3::types::IntoPyDict;
    let kwargs = [("dtype", "str")].into_py_dict(py)?;
    Ok(py
        .import("numpy")?
        .call_method("array", (vals,), Some(&kwargs))?
        .unbind())
}

pub struct PendingField {
    pub name: String,
    pub values: Vec<f64>,
    pub unit: Option<String>,
}

/// One derived topic the script is building. Every field shares `times`.
pub struct PendingTopic {
    pub name: String,
    pub times: Vec<i64>,
    pub fields: Vec<PendingField>,
}

impl PendingTopic {
    pub fn new(name: String, times: Vec<i64>) -> Self {
        Self {
            name,
            times,
            fields: Vec::new(),
        }
    }

    pub fn add_field(
        &mut self,
        name: String,
        values: Vec<f64>,
        unit: Option<String>,
    ) -> Result<(), String> {
        if values.len() != self.times.len() {
            return Err(format!(
                "field '{name}': {} values but topic '{}' has {} timestamps",
                values.len(),
                self.name,
                self.times.len()
            ));
        }
        self.fields.push(PendingField { name, values, unit });
        Ok(())
    }
}

pub type EmitBuffer = Rc<RefCell<Vec<PendingTopic>>>;

/// `unsendable`: lives only on the worker thread under the GIL.
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
    live: LiveTransformBuffer,
    script_name: String,
    generation: u64,
    params: SharedParams,
}

impl Delog {
    pub fn new(
        snapshot: Arc<StoreSnapshot>,
        emit: EmitBuffer,
        live: LiveTransformBuffer,
        script_name: String,
        generation: u64,
        params: SharedParams,
    ) -> Self {
        Self {
            snapshot,
            emit,
            live,
            script_name,
            generation,
            params,
        }
    }

    pub fn emit_buffer(&self) -> EmitBuffer {
        Rc::clone(&self.emit)
    }

    pub fn live_buffer(&self) -> LiveTransformBuffer {
        Rc::clone(&self.live)
    }

    fn resolve_path(&self, path: &str) -> Result<FieldId, String> {
        if let Some(id) = self.resolve_path_fast(path) {
            return Ok(id);
        }
        if let Some(id) = self.resolve_path_scan(path) {
            return Ok(id);
        }
        Err(format!("field path '{path}' not found"))
    }

    /// Fast path: split on the first two `/` and assume the remainder is the
    /// field name. Correct as long as the topic name contains no `/`, which
    /// covers the overwhelming majority of paths without an O(n) scan.
    fn resolve_path_fast(&self, path: &str) -> Option<FieldId> {
        let mut parts = path.splitn(3, '/');
        let (s, t, f) = (parts.next()?, parts.next()?, parts.next()?);
        for src in self.snapshot.sources.iter() {
            if src.entry.removed || src.entry.label != s {
                continue;
            }
            for &topic_id in src.topics.iter() {
                let Some(topic) = self.snapshot.topic(topic_id) else {
                    continue;
                };
                if topic.entry.removed || topic.entry.name != t {
                    continue;
                }
                for fe in self.snapshot.fields.iter() {
                    if !fe.removed && fe.topic == topic_id && fe.name == f {
                        return Some(fe.id);
                    }
                }
            }
        }
        None
    }

    /// Fallback for topics containing `/` (e.g. dynamic live-transform output
    /// topics such as "NAMED_VALUE_FLOAT/airspd"), which `resolve_path_fast`
    /// mis-splits. Scans every live field and matches the exact path
    /// `sources()` builds for it, guaranteeing a round-trip.
    fn resolve_path_scan(&self, path: &str) -> Option<FieldId> {
        for src in self.snapshot.sources.iter() {
            if src.entry.removed {
                continue;
            }
            for &topic_id in src.topics.iter() {
                let Some(topic) = self.snapshot.topic(topic_id) else {
                    continue;
                };
                if topic.entry.removed {
                    continue;
                }
                for fe in self.snapshot.fields.iter() {
                    if !fe.removed
                        && fe.topic == topic_id
                        && path == format!("{}/{}/{}", src.entry.label, topic.entry.name, fe.name)
                    {
                        return Some(fe.id);
                    }
                }
            }
        }
        None
    }
}

#[pymethods]
impl Delog {
    fn sources(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let mut paths: Vec<String> = Vec::new();
        for src in self.snapshot.sources.iter() {
            if src.entry.removed {
                continue;
            }
            for &topic_id in src.topics.iter() {
                let Some(topic) = self.snapshot.topic(topic_id) else {
                    continue;
                };
                if topic.entry.removed {
                    continue;
                }
                for fe in self.snapshot.fields.iter() {
                    if !fe.removed && fe.topic == topic_id {
                        paths.push(format!(
                            "{}/{}/{}",
                            src.entry.label, topic.entry.name, fe.name
                        ));
                    }
                }
            }
        }
        Ok(PyList::new(py, paths)?.unbind())
    }

    fn field(&self, py: Python<'_>, path: &str) -> PyResult<DelogField> {
        let id = self
            .resolve_path(path)
            .map_err(pyo3::exceptions::PyKeyError::new_err)?;
        let (t, v, s) = materialize_field(&self.snapshot, id)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        let s = s.map(|vals| numpy_str_array(py, vals)).transpose()?;
        Ok(DelogField {
            t: t.into_pyarray(py).unbind(),
            v: v.into_pyarray(py).unbind(),
            s,
        })
    }

    fn resample_prev(
        &self,
        py: Python<'_>,
        field: &DelogField,
        base_times: numpy::PyReadonlyArray1<i64>,
    ) -> PyResult<Py<numpy::PyArray1<f64>>> {
        let t = field.t.bind(py).readonly();
        let v = field.v.bind(py).readonly();
        let out = resample_prev(t.as_slice()?, v.as_slice()?, base_times.as_slice()?);
        Ok(out.into_pyarray(py).unbind())
    }

    fn output(&self, times_us: numpy::PyReadonlyArray1<i64>, name: &str) -> PyResult<DelogOutput> {
        let times = times_us.as_slice()?.to_vec();
        let idx = {
            let mut buf = self.emit.borrow_mut();
            buf.push(PendingTopic::new(name.to_string(), times));
            buf.len() - 1
        };
        Ok(DelogOutput {
            emit: Rc::clone(&self.emit),
            index: idx,
        })
    }

    /// Decorator factory: returns a decorator that registers the function and
    /// returns it unchanged.
    #[pyo3(signature = (*, topic, fields, output_topic=None))]
    fn live_transform(
        &self,
        py: Python<'_>,
        topic: String,
        fields: Vec<String>,
        output_topic: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        let spec = LiveTransformSpec::new(
            self.script_name.clone(),
            self.generation,
            topic,
            fields,
            output_topic,
        )
        .map_err(pyo3::exceptions::PyValueError::new_err)?;

        #[pyclass(unsendable)]
        struct Decorator {
            spec: LiveTransformSpec,
            live: LiveTransformBuffer,
        }

        #[pymethods]
        impl Decorator {
            fn __call__(&self, py: Python<'_>, func: Py<PyAny>) -> PyResult<Py<PyAny>> {
                let mut spec = self.spec.clone();
                spec.func_name = func
                    .bind(py)
                    .getattr("__name__")
                    .and_then(|n| n.extract::<String>())
                    .unwrap_or_else(|_| "<callable>".into());
                self.live.borrow_mut().push(PendingLiveTransform {
                    spec,
                    callable: func.clone_ref(py),
                });
                Ok(func)
            }
        }

        Ok(Bound::new(
            py,
            Decorator {
                spec,
                live: Rc::clone(&self.live),
            },
        )?
        .into_any()
        .unbind())
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (name, default, *, min, max, step=None, label=None))]
    fn slider(
        &self,
        py: Python<'_>,
        name: String,
        default: Bound<'_, PyAny>,
        min: f64,
        max: f64,
        step: Option<f64>,
        label: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        // `!(min < max)` rather than `min >= max` so NaN bounds are rejected too.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(min < max) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "slider '{name}': min ({min}) must be < max ({max})"
            )));
        }
        // Integer slider iff the default is a Python int (and not a bool).
        let integer = default.is_instance_of::<PyInt>() && !default.is_instance_of::<PyBool>();
        let mut d: f64 = default.extract()?;
        d = d.clamp(min, max);
        let spec = ParamSpec {
            name: name.clone(),
            label: label.unwrap_or_else(|| name.clone()),
            kind: ParamKind::Slider { min, max, step, integer },
            default: ParamValue::Float(d),
            order: 0,
            generation: self.generation,
        };
        self.declare_and_return(py, spec)
    }

    #[pyo3(signature = (name, default, *, label=None))]
    fn checkbox(
        &self,
        py: Python<'_>,
        name: String,
        default: bool,
        label: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        let spec = ParamSpec {
            name: name.clone(),
            label: label.unwrap_or_else(|| name.clone()),
            kind: ParamKind::Checkbox,
            default: ParamValue::Bool(default),
            order: 0,
            generation: self.generation,
        };
        self.declare_and_return(py, spec)
    }

    #[pyo3(signature = (name, options, *, default=None, label=None))]
    fn combo(
        &self,
        py: Python<'_>,
        name: String,
        options: Vec<String>,
        default: Option<String>,
        label: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        if options.is_empty() || options.iter().any(|o| o.is_empty()) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "combo '{name}': options must be a non-empty list of non-empty strings"
            )));
        }
        let default = match default {
            Some(d) => {
                if !options.contains(&d) {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "combo '{name}': default '{d}' is not one of the options"
                    )));
                }
                d
            }
            None => options[0].clone(),
        };
        let spec = ParamSpec {
            name: name.clone(),
            label: label.unwrap_or_else(|| name.clone()),
            kind: ParamKind::Combo { options },
            default: ParamValue::Text(default),
            order: 0,
            generation: self.generation,
        };
        self.declare_and_return(py, spec)
    }

    #[pyo3(signature = (name, default, *, label=None))]
    fn text(
        &self,
        py: Python<'_>,
        name: String,
        default: String,
        label: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        let spec = ParamSpec {
            name: name.clone(),
            label: label.unwrap_or_else(|| name.clone()),
            kind: ParamKind::Text,
            default: ParamValue::Text(default),
            order: 0,
            generation: self.generation,
        };
        self.declare_and_return(py, spec)
    }

    fn param(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        let script = crate::params::current_script().unwrap_or_else(|| self.script_name.clone());
        let store = self.params.lock().unwrap();
        // Resolve against the declared spec (not a bare persisted value), so an
        // undeclared name raises and a slider's int typing is preserved.
        let spec = store.spec(&script, name).ok_or_else(|| {
            pyo3::exceptions::PyKeyError::new_err(format!(
                "param '{name}' is not declared for script '{script}'"
            ))
        })?;
        let value = store
            .value(&script, name)
            .unwrap_or_else(|| spec.default.clone());
        value_to_py(py, &value, Some(&spec.kind))
    }
}

impl Delog {
    fn declare_and_return(&self, py: Python<'_>, spec: ParamSpec) -> PyResult<Py<PyAny>> {
        let kind = spec.kind.clone();
        let value = self
            .params
            .lock()
            .unwrap()
            .declare(&self.script_name, self.generation, spec)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        value_to_py(py, &value, Some(&kind))
    }
}

fn value_to_py(py: Python<'_>, value: &ParamValue, kind: Option<&ParamKind>) -> PyResult<Py<PyAny>> {
    use pyo3::IntoPyObject;
    match value {
        ParamValue::Float(v) => {
            if matches!(kind, Some(ParamKind::Slider { integer: true, .. })) {
                Ok((v.round() as i64).into_pyobject(py)?.into_any().unbind())
            } else {
                Ok(v.into_pyobject(py)?.into_any().unbind())
            }
        }
        ParamValue::Bool(b) => Ok(b.into_pyobject(py)?.to_owned().into_any().unbind()),
        ParamValue::Text(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
    }
}

/// `.t` int64 us, `.v` float64 (NaN for string fields), `.s` numpy unicode
/// array for string fields (`None` otherwise).
#[pyclass(unsendable, name = "DelogField")]
pub struct DelogField {
    #[pyo3(get)]
    t: Py<PyArray1<i64>>,
    #[pyo3(get)]
    v: Py<PyArray1<f64>>,
    #[pyo3(get)]
    s: Option<Py<PyAny>>,
}

#[pyclass(unsendable, name = "DelogOutput")]
pub struct DelogOutput {
    emit: EmitBuffer,
    index: usize,
}

#[pymethods]
impl DelogOutput {
    #[pyo3(signature = (name, values, unit=None))]
    fn add_field(
        &self,
        name: &str,
        values: numpy::PyReadonlyArray1<f64>,
        unit: Option<String>,
    ) -> PyResult<()> {
        let vals = values.as_slice()?.to_vec();
        self.emit.borrow_mut()[self.index]
            .add_field(name.to_string(), vals, unit)
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Float64Array, Int64Array};

    #[test]
    fn output_builder_collects_topics_and_fields() {
        let mut topic = super::PendingTopic::new("Mag".into(), vec![0, 100, 200]);
        topic
            .add_field("x".into(), vec![1.0, 2.0, 3.0], Some("m".into()))
            .unwrap();
        topic
            .add_field("y".into(), vec![4.0, 5.0, 6.0], None)
            .unwrap();
        assert!(topic.add_field("bad".into(), vec![1.0], None).is_err());
        assert_eq!(topic.fields.len(), 2);
        assert_eq!(topic.times.len(), 3);
    }

    #[test]
    fn resample_prev_picks_the_last_value_at_or_before_each_base_time() {
        let src_t = vec![0_i64, 100, 200];
        let src_v = vec![10.0_f64, 20.0, 30.0];
        let base = vec![-5_i64, 0, 50, 100, 250];
        let out = super::resample_prev(&src_t, &src_v, &base);
        assert!(out[0].is_nan());
        assert_eq!(out[1], 10.0);
        assert_eq!(out[2], 10.0);
        assert_eq!(out[3], 20.0);
        assert_eq!(out[4], 30.0);
    }

    proptest::proptest! {
        #[test]
        fn resample_prev_matches_naive_scan(
            src in proptest::collection::vec((0i64..1000, -1e6f64..1e6), 1..50),
            base in proptest::collection::vec(0i64..1000, 1..50),
        ) {
            let mut src = src;
            src.sort_by_key(|(t, _)| *t);
            src.dedup_by_key(|(t, _)| *t);
            let st: Vec<i64> = src.iter().map(|(t, _)| *t).collect();
            let sv: Vec<f64> = src.iter().map(|(_, v)| *v).collect();
            let got = super::resample_prev(&st, &sv, &base);
            for (i, &bt) in base.iter().enumerate() {
                let naive = st.iter().rposition(|&t| t <= bt).map(|idx| sv[idx]);
                match naive {
                    Some(v) => proptest::prop_assert_eq!(got[i], v),
                    None => proptest::prop_assert!(got[i].is_nan()),
                }
            }
        }
    }
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    #[test]
    fn materialize_field_concatenates_chunks_in_time_order() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let topic = id.add_topic(src, "BARO").unwrap();
        let alt = id.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let c1: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0, 2.0]))];
        let c2: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![3.0]))];
        let chunk1 = Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), c1, &schema).unwrap());
        let chunk2 = Arc::new(Chunk::try_new(Int64Array::from(vec![30]), c2, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk1, chunk2]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();

        let (t, v, s) = materialize_field(&snap, alt).unwrap();
        assert_eq!(t, vec![10, 20, 30]);
        assert_eq!(v, vec![1.0, 2.0, 3.0]);
        assert_eq!(s, None);
    }

    #[test]
    fn resolve_path_falls_back_to_scanning_when_the_topic_name_contains_a_slash() {
        // A dynamic live-transform output topic (e.g. "NAMED_VALUE_FLOAT/airspd")
        // makes the fast splitn(3, '/') path mis-split into topic
        // "NAMED_VALUE_FLOAT" and field "airspd/value". The scan fallback must
        // still resolve it, matching exactly the path `sources()` would build.
        let mut id = IdentityRegistry::new();
        let src = id.add_source("live");
        let topic = id.add_topic(src, "NAMED_VALUE_FLOAT/airspd").unwrap();
        let value = id.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "NAMED_VALUE_FLOAT/airspd",
                [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.5]))];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![100]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snap = Arc::new(StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap());

        let delog = Delog::new(
            snap,
            EmitBuffer::default(),
            LiveTransformBuffer::default(),
            String::new(),
            0,
            crate::params::shared_empty(),
        );
        assert_eq!(
            delog
                .resolve_path("live/NAMED_VALUE_FLOAT/airspd/value")
                .unwrap(),
            value
        );
        assert!(
            delog
                .resolve_path("live/NAMED_VALUE_FLOAT/airspd/missing")
                .is_err()
        );
    }

    #[test]
    fn materialize_field_extracts_strings_for_utf8_columns() {
        use arrow::array::StringArray;
        let mut id = IdentityRegistry::new();
        let src = id.add_source("live");
        let topic = id.add_topic(src, "NAMED_VALUE_FLOAT").unwrap();
        let name = id.add_field(topic, "name").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "NAMED_VALUE_FLOAT",
                [FieldSchema::new("name", DataType::Utf8, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(StringArray::from(vec![Some("airspd"), None]))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();

        let (t, v, s) = materialize_field(&snap, name).unwrap();
        assert_eq!(t, vec![10, 20]);
        assert!(v.iter().all(|x| x.is_nan()));
        assert_eq!(s, Some(vec!["airspd".to_owned(), String::new()]));
    }
}

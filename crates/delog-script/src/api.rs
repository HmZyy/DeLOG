//! The `delog` Python API object and the Arrow→numpy read path (SCR-03).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use delog_core::field_view::FieldView;
use delog_core::field_view::array_row_as_f64;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use numpy::{IntoPyArray, PyArray1, PyArrayMethods};
use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::live::LiveTransformSpec;

/// A live transform registration accumulated during one script run.
pub struct PendingLiveTransform {
    pub spec: LiveTransformSpec,
    pub callable: Py<PyAny>,
}

/// Shared, single-threaded accumulator of live transform registrations for one run.
pub type LiveTransformBuffer = Rc<RefCell<Vec<PendingLiveTransform>>>;

/// Prev-sample resample: for each `base` time, the source value at the latest
/// source timestamp `<= base` (NaN before the first sample). `src_t` must be
/// sorted ascending. O(m log n) via binary search.
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

/// Materialize a field as `(times_us: Vec<i64>, values: Vec<f64>)` by walking
/// its chunks in time order. This concatenates chunk buffers — the One Copy for
/// script consumption, off the render hot path.
// ZC-EXCEPTION: script materialization (PLAN.md §4.5) — a deliberate copy of the
// canonical field into a contiguous host buffer for numpy; counted, off-path.
pub fn materialize_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
) -> Result<(Vec<i64>, Vec<f64>), String> {
    let view = FieldView::new(snapshot, field).map_err(|e| e.to_string())?;
    let col = view.col_index();
    let range = snapshot
        .global_time_range()
        .ok_or_else(|| "field has no data".to_string())?;
    let mut times = Vec::new();
    let mut values = Vec::new();
    for chunk in view.chunks_overlapping(range) {
        for row in 0..chunk.len() {
            times.push(chunk.t.value(row));
            values.push(array_row_as_f64(chunk.cols[col].as_ref(), row));
        }
    }
    Ok((times, values))
}

/// A field accumulated for emission: name + values aligned to its topic's times.
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

/// Shared, single-threaded accumulator of derived topics for one run.
pub type EmitBuffer = Rc<RefCell<Vec<PendingTopic>>>;

/// The `delog` object injected into the interpreter namespace. Holds the
/// snapshot pinned for the current run/eval. `unsendable`: it lives only on the
/// worker thread under the GIL.
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
    live: LiveTransformBuffer,
    script_name: String,
    generation: u64,
}

impl Delog {
    pub fn new(
        snapshot: Arc<StoreSnapshot>,
        emit: EmitBuffer,
        live: LiveTransformBuffer,
        script_name: String,
        generation: u64,
    ) -> Self {
        Self {
            snapshot,
            emit,
            live,
            script_name,
            generation,
        }
    }

    /// The accumulated derived topics (the run lifecycle flushes these — Task 8).
    pub fn emit_buffer(&self) -> EmitBuffer {
        Rc::clone(&self.emit)
    }

    /// The accumulated live transform registrations for this run.
    pub fn live_buffer(&self) -> LiveTransformBuffer {
        Rc::clone(&self.live)
    }

    fn resolve_path(&self, path: &str) -> Result<FieldId, String> {
        let mut parts = path.splitn(3, '/');
        let (s, t, f) = match (parts.next(), parts.next(), parts.next()) {
            (Some(s), Some(t), Some(f)) => (s, t, f),
            _ => return Err(format!("bad field path '{path}' (want source/topic/field)")),
        };
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
                        return Ok(fe.id);
                    }
                }
            }
        }
        Err(format!("field path '{path}' not found"))
    }
}

#[pymethods]
impl Delog {
    /// List every live field as "source/topic/field".
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

    /// Read a field as a `DelogField` carrying numpy arrays.
    fn field(&self, py: Python<'_>, path: &str) -> PyResult<DelogField> {
        let id = self
            .resolve_path(path)
            .map_err(pyo3::exceptions::PyKeyError::new_err)?;
        let (t, v) = materialize_field(&self.snapshot, id)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        Ok(DelogField {
            t: t.into_pyarray(py).unbind(),
            v: v.into_pyarray(py).unbind(),
        })
    }

    /// Prev-sample resample a field's values onto `base_times`.
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

    /// Begin a derived topic; `times_us` is shared by all its fields.
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

    /// Decorator factory: `@delog.live_transform(topic=..., fields=[...], output_topic=...)`
    /// returns a decorator that registers the function and returns it unchanged.
    #[pyo3(signature = (*, topic, fields, output_topic))]
    fn live_transform(
        &self,
        py: Python<'_>,
        topic: String,
        fields: Vec<String>,
        output_topic: String,
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
                self.live.borrow_mut().push(PendingLiveTransform {
                    spec: self.spec.clone(),
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
}

/// A field handed to Python: `.t` int64 µs numpy, `.v` float64 numpy.
#[pyclass(unsendable, name = "DelogField")]
pub struct DelogField {
    #[pyo3(get)]
    t: Py<PyArray1<i64>>,
    #[pyo3(get)]
    v: Py<PyArray1<f64>>,
}

/// Returned by `delog.output(...)`; `add_field` appends to the pending topic.
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
        // length mismatch is rejected
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

        let (t, v) = materialize_field(&snap, alt).unwrap();
        assert_eq!(t, vec![10, 20, 30]);
        assert_eq!(v, vec![1.0, 2.0, 3.0]);
    }
}

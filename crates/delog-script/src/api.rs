//! The `delog` Python API object and the Arrow→numpy read path (SCR-03).

use std::sync::Arc;

use delog_core::field_view::FieldView;
use delog_core::field_view::array_row_as_f64;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use numpy::{IntoPyArray, PyArray1};
use pyo3::prelude::*;
use pyo3::types::PyList;

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

/// The `delog` object injected into the interpreter namespace. Holds the
/// snapshot pinned for the current run/eval. `unsendable`: it lives only on the
/// worker thread under the GIL.
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
}

impl Delog {
    pub fn new(snapshot: Arc<StoreSnapshot>) -> Self {
        Self { snapshot }
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
}

/// A field handed to Python: `.t` int64 µs numpy, `.v` float64 numpy.
#[pyclass(unsendable, name = "DelogField")]
pub struct DelogField {
    #[pyo3(get)]
    t: Py<PyArray1<i64>>,
    #[pyo3(get)]
    v: Py<PyArray1<f64>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Float64Array, Int64Array};
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

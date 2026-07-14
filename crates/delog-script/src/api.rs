use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use delog_core::field_view::FieldView;
use delog_core::field_view::array_row_as_f64;
use delog_core::field_view::array_row_as_str;
use delog_core::identity::FieldId;
use delog_core::identity::{SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;

use numpy::{IntoPyArray, PyArray1, PyArrayMethods};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::types::PyList;
use pyo3::types::PyTuple;
use pyo3::types::{PyMapping, PyMappingMethods};

use crate::live::LiveTransformSpec;
use crate::operations::{
    GroupBySpec, MergeSpec, OperationBuffer, OperationMode, OperationSpec, TopicSelector,
    TransformSpec, merged_field_names, validate_group_template, validate_transform,
};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopicMatch {
    pub(crate) source_id: SourceId,
    pub(crate) source_label: String,
    pub(crate) topic_id: TopicId,
    pub(crate) topic_name: String,
    pub(crate) base_name: String,
    pub(crate) instance: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldMatch {
    pub(crate) source_id: SourceId,
    pub(crate) source_label: String,
    pub(crate) topic_id: TopicId,
    pub(crate) topic_name: String,
    pub(crate) base_name: String,
    pub(crate) instance: Option<u32>,
    pub(crate) field_id: FieldId,
    pub(crate) field_name: String,
    pub(crate) unit: Option<String>,
}

pub(crate) fn parse_topic_instance(name: &str) -> (String, Option<u32>) {
    let Some(open) = name.rfind('[') else {
        return (name.to_owned(), None);
    };
    if !name.ends_with(']') || open == 0 {
        return (name.to_owned(), None);
    }
    let digits = &name[open + 1..name.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return (name.to_owned(), None);
    }
    match digits.parse::<u32>() {
        Ok(instance) => (name[..open].to_owned(), Some(instance)),
        Err(_) => (name.to_owned(), None),
    }
}

pub(crate) fn topic_matches(
    topic_name: &str,
    base_name: &str,
    parsed_instance: Option<u32>,
    requested_topic: Option<&str>,
    requested_instance: Option<u32>,
) -> bool {
    if let Some(topic) = requested_topic {
        if topic_name != topic && base_name != topic {
            return false;
        }
    }
    if let Some(instance) = requested_instance {
        if parsed_instance != Some(instance) {
            return false;
        }
    }
    true
}

pub(crate) fn find_topics(
    snapshot: &StoreSnapshot,
    topic: Option<&str>,
    source: Option<&str>,
    instance: Option<u32>,
) -> Vec<TopicMatch> {
    let mut out = Vec::new();
    for src in snapshot.sources.iter() {
        if src.entry.removed {
            continue;
        }
        if let Some(source) = source {
            if src.entry.label != source {
                continue;
            }
        }
        for &topic_id in src.topics.iter() {
            let Some(topic_snapshot) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic_snapshot.entry.removed {
                continue;
            }
            let (base_name, parsed_instance) = parse_topic_instance(&topic_snapshot.entry.name);
            if !topic_matches(
                &topic_snapshot.entry.name,
                &base_name,
                parsed_instance,
                topic,
                instance,
            ) {
                continue;
            }
            out.push(TopicMatch {
                source_id: src.entry.id,
                source_label: src.entry.label.clone(),
                topic_id,
                topic_name: topic_snapshot.entry.name.clone(),
                base_name,
                instance: parsed_instance,
            });
        }
    }
    out
}

fn field_unit(snapshot: &StoreSnapshot, topic: TopicId, field_name: &str) -> Option<String> {
    let store = snapshot.topic_store(topic)?;
    store.schema.field_by_name(field_name)?.unit.clone()
}

fn find_fields(
    snapshot: &StoreSnapshot,
    topic: Option<&str>,
    field: Option<&str>,
    source: Option<&str>,
    instance: Option<u32>,
) -> Vec<FieldMatch> {
    let mut out = Vec::new();
    for topic_match in find_topics(snapshot, topic, source, instance) {
        for fe in snapshot.fields.iter() {
            if fe.removed || fe.topic != topic_match.topic_id {
                continue;
            }
            if let Some(field) = field {
                if fe.name != field {
                    continue;
                }
            }
            out.push(FieldMatch {
                source_id: topic_match.source_id,
                source_label: topic_match.source_label.clone(),
                topic_id: topic_match.topic_id,
                topic_name: topic_match.topic_name.clone(),
                base_name: topic_match.base_name.clone(),
                instance: topic_match.instance,
                field_id: fe.id,
                field_name: fe.name.clone(),
                unit: field_unit(snapshot, topic_match.topic_id, &fe.name),
            });
        }
    }
    out
}

pub(crate) fn find_fields_in_topic(
    snapshot: &StoreSnapshot,
    topic_id: TopicId,
    field: Option<&str>,
) -> Vec<FieldMatch> {
    let Some(topic_snapshot) = snapshot.topic(topic_id) else {
        return Vec::new();
    };
    if topic_snapshot.entry.removed {
        return Vec::new();
    }
    let Some(src) = snapshot
        .sources
        .iter()
        .find(|src| !src.entry.removed && src.topics.iter().any(|&id| id == topic_id))
    else {
        return Vec::new();
    };
    let (base_name, instance) = parse_topic_instance(&topic_snapshot.entry.name);
    let mut out = Vec::new();
    for fe in snapshot.fields.iter() {
        if fe.removed || fe.topic != topic_id {
            continue;
        }
        if let Some(field) = field {
            if fe.name != field {
                continue;
            }
        }
        out.push(FieldMatch {
            source_id: src.entry.id,
            source_label: src.entry.label.clone(),
            topic_id,
            topic_name: topic_snapshot.entry.name.clone(),
            base_name: base_name.clone(),
            instance,
            field_id: fe.id,
            field_name: fe.name.clone(),
            unit: field_unit(snapshot, topic_id, &fe.name),
        });
    }
    out
}

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
    let kwargs = PyDict::new(py);
    kwargs.set_item("dtype", "str")?;
    Ok(py
        .import("numpy")?
        .call_method("array", (vals,), Some(&kwargs))?
        .unbind())
}

fn materialized_values_to_py(
    py: Python<'_>,
    values: Vec<f64>,
    strings: Option<Vec<String>>,
) -> PyResult<Py<PyAny>> {
    match strings {
        Some(vals) => numpy_str_array(py, vals),
        None => Ok(values.into_pyarray(py).into_any().unbind()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PendingColumn {
    F64(Vec<f64>),
    Utf8(Vec<String>),
}

impl PendingColumn {
    pub fn len(&self) -> usize {
        match self {
            Self::F64(v) => v.len(),
            Self::Utf8(v) => v.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingField {
    pub name: String,
    pub values: PendingColumn,
    pub unit: Option<String>,
}

impl PendingField {
    pub fn numeric(name: impl Into<String>, values: Vec<f64>, unit: Option<String>) -> Self {
        Self {
            name: name.into(),
            values: PendingColumn::F64(values),
            unit,
        }
    }

    pub fn utf8(name: impl Into<String>, values: Vec<String>) -> Self {
        Self {
            name: name.into(),
            values: PendingColumn::Utf8(values),
            unit: None,
        }
    }
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

    pub fn add_field(&mut self, field: PendingField) -> Result<(), String> {
        if field.values.len() != self.times.len() {
            return Err(format!(
                "field '{}': {} values but topic '{}' has {} timestamps",
                field.name,
                field.values.len(),
                self.name,
                self.times.len()
            ));
        }
        self.fields.push(field);
        Ok(())
    }
}

fn parse_emit_field_entry(
    name: &str,
    value: &Bound<'_, PyAny>,
    expected: usize,
) -> PyResult<(Vec<f64>, Option<String>)> {
    if let Ok(tuple) = value.cast::<PyTuple>() {
        if tuple.len() != 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "emit field '{name}' tuple must be (values, unit)"
            )));
        }
        let values: numpy::PyReadonlyArray1<f64> = tuple.get_item(0)?.extract().map_err(|_| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "emit field '{name}' values must be a 1-D float array"
            ))
        })?;
        let vals = values.as_slice()?.to_vec();
        if vals.len() != expected {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "emit field '{name}' produced {} values but topic has {expected} timestamps",
                vals.len()
            )));
        }
        let unit: Option<String> = tuple.get_item(1)?.extract().map_err(|_| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "emit field '{name}' unit must be a string or None"
            ))
        })?;
        return Ok((vals, unit));
    }

    let values: numpy::PyReadonlyArray1<f64> = value.extract().map_err(|_| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "emit field '{name}' must be values or (values, unit)"
        ))
    })?;
    let vals = values.as_slice()?.to_vec();
    if vals.len() != expected {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "emit field '{name}' produced {} values but topic has {expected} timestamps",
            vals.len()
        )));
    }
    Ok((vals, None))
}

pub type EmitBuffer = Rc<RefCell<Vec<PendingTopic>>>;

/// `unsendable`: lives only on the worker thread under the GIL.
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
    live: LiveTransformBuffer,
    operations: OperationBuffer,
    script_name: String,
    generation: u64,
    params: SharedParams,
}

impl Delog {
    pub fn new(
        snapshot: Arc<StoreSnapshot>,
        emit: EmitBuffer,
        live: LiveTransformBuffer,
        operations: OperationBuffer,
        script_name: String,
        generation: u64,
        params: SharedParams,
    ) -> Self {
        Self {
            snapshot,
            emit,
            live,
            operations,
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

    pub fn operation_buffer(&self) -> OperationBuffer {
        Rc::clone(&self.operations)
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

#[pyclass(unsendable, name = "SourceRef", skip_from_py_object)]
#[derive(Clone)]
struct SourceRefPy {
    #[pyo3(get)]
    label: String,
    #[pyo3(get)]
    path: String,
}

#[pyclass(unsendable, name = "TopicRef", skip_from_py_object)]
#[derive(Clone)]
struct TopicRefPy {
    snapshot: Arc<StoreSnapshot>,
    topic_id: TopicId,
    #[pyo3(get)]
    source: String,
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    instance: Option<u32>,
    #[pyo3(get)]
    path: String,
}

#[allow(dead_code)]
#[pyclass(unsendable, name = "FieldRef", skip_from_py_object)]
#[derive(Clone)]
struct FieldRefPy {
    snapshot: Arc<StoreSnapshot>,
    field_id: FieldId,
    topic_id: TopicId,
    #[pyo3(get)]
    source: String,
    #[pyo3(get)]
    topic: String,
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    unit: Option<String>,
    #[pyo3(get)]
    path: String,
}

#[pyclass(unsendable, name = "Catalog")]
struct CatalogPy {
    snapshot: Arc<StoreSnapshot>,
}

fn topic_ref(snapshot: Arc<StoreSnapshot>, m: TopicMatch) -> TopicRefPy {
    TopicRefPy {
        snapshot,
        topic_id: m.topic_id,
        source: m.source_label.clone(),
        name: m.topic_name.clone(),
        instance: m.instance,
        path: format!("{}/{}", m.source_label, m.topic_name),
    }
}

fn field_ref(snapshot: Arc<StoreSnapshot>, m: FieldMatch) -> FieldRefPy {
    FieldRefPy {
        snapshot,
        field_id: m.field_id,
        topic_id: m.topic_id,
        source: m.source_label.clone(),
        topic: m.topic_name.clone(),
        name: m.field_name.clone(),
        unit: m.unit.clone(),
        path: format!("{}/{}/{}", m.source_label, m.topic_name, m.field_name),
    }
}

fn candidate_topic_paths(matches: &[TopicMatch]) -> String {
    matches
        .iter()
        .map(|m| format!("{}/{}", m.source_label, m.topic_name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn candidate_field_paths(matches: &[FieldMatch]) -> String {
    matches
        .iter()
        .map(|m| format!("{}/{}/{}", m.source_label, m.topic_name, m.field_name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn unique_topic(
    snapshot: Arc<StoreSnapshot>,
    name: &str,
    source: Option<&str>,
    instance: Option<u32>,
) -> PyResult<TopicRefPy> {
    let matches = find_topics(&snapshot, Some(name), source, instance);
    match matches.len() {
        1 => Ok(topic_ref(snapshot, matches.into_iter().next().unwrap())),
        0 => {
            let candidates = find_topics(&snapshot, None, source, instance);
            if candidates.is_empty() {
                Err(pyo3::exceptions::PyKeyError::new_err(format!(
                    "topic '{name}' not found"
                )))
            } else {
                Err(pyo3::exceptions::PyKeyError::new_err(format!(
                    "topic '{name}' not found; candidates: {}",
                    candidate_topic_paths(&candidates)
                )))
            }
        }
        _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "topic '{name}' is ambiguous; candidates: {}; pass source= or instance=",
            candidate_topic_paths(&matches)
        ))),
    }
}

#[pymethods]
impl SourceRefPy {
    fn __repr__(&self) -> String {
        format!("SourceRef({:?})", self.label)
    }
}

#[pymethods]
impl TopicRefPy {
    fn __repr__(&self) -> String {
        format!("TopicRef({:?})", self.path)
    }

    fn fields(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let out = PyList::empty(py);
        for m in find_fields_in_topic(&self.snapshot, self.topic_id, None) {
            out.append(Bound::new(py, field_ref(Arc::clone(&self.snapshot), m))?)?;
        }
        Ok(out.unbind())
    }

    fn field(&self, name: &str) -> PyResult<FieldRefPy> {
        let matches = find_fields_in_topic(&self.snapshot, self.topic_id, Some(name));
        match matches.len() {
            1 => Ok(field_ref(
                Arc::clone(&self.snapshot),
                matches.into_iter().next().unwrap(),
            )),
            0 => {
                let candidates = find_fields_in_topic(&self.snapshot, self.topic_id, None);
                if candidates.is_empty() {
                    Err(pyo3::exceptions::PyKeyError::new_err(format!(
                        "field '{name}' not found in topic '{}'",
                        self.name
                    )))
                } else {
                    Err(pyo3::exceptions::PyKeyError::new_err(format!(
                        "field '{name}' not found in topic '{}'; candidates: {}",
                        self.name,
                        candidate_field_paths(&candidates)
                    )))
                }
            }
            _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "field '{name}' in topic '{}' is ambiguous",
                self.name
            ))),
        }
    }

    #[pyo3(signature = (*fields))]
    fn read(&self, py: Python<'_>, fields: &Bound<'_, PyTuple>) -> PyResult<DelogTable> {
        let fields = fields
            .iter()
            .map(|item| item.extract::<String>())
            .collect::<PyResult<Vec<_>>>()?;
        let requested = if fields.is_empty() {
            find_fields_in_topic(&self.snapshot, self.topic_id, None)
                .into_iter()
                .map(|m| m.field_name)
                .collect::<Vec<_>>()
        } else {
            fields
        };
        let mut table_t: Option<Vec<i64>> = None;
        let mut names = Vec::with_capacity(requested.len());
        let mut columns = std::collections::HashMap::new();
        for name in requested {
            let field_ref = self.field(&name)?;
            let (t, v, s) = materialize_field(&self.snapshot, field_ref.field_id)
                .map_err(pyo3::exceptions::PyValueError::new_err)?;
            match &table_t {
                None => table_t = Some(t),
                Some(existing) if *existing == t => {}
                Some(_) => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "topic '{}' field '{name}' does not share the topic timeline",
                        self.name
                    )));
                }
            }
            columns.insert(name.clone(), materialized_values_to_py(py, v, s)?);
            names.push(name);
        }
        Ok(DelogTable {
            t: table_t.unwrap_or_default().into_pyarray(py).unbind(),
            fields: names,
            columns,
        })
    }
}

#[pymethods]
impl FieldRefPy {
    fn __repr__(&self) -> String {
        format!("FieldRef({:?})", self.path)
    }

    fn read(&self, py: Python<'_>) -> PyResult<DelogField> {
        let (t, v, s) = materialize_field(&self.snapshot, self.field_id)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        let s = s.map(|vals| numpy_str_array(py, vals)).transpose()?;
        Ok(DelogField {
            t: t.into_pyarray(py).unbind(),
            v: v.into_pyarray(py).unbind(),
            s,
        })
    }
}

#[pymethods]
impl CatalogPy {
    fn sources(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let out = PyList::empty(py);
        for src in self.snapshot.sources.iter().filter(|s| !s.entry.removed) {
            out.append(Bound::new(
                py,
                SourceRefPy {
                    label: src.entry.label.clone(),
                    path: src.entry.label.clone(),
                },
            )?)?;
        }
        Ok(out.unbind())
    }

    fn topics(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let out = PyList::empty(py);
        for m in find_topics(&self.snapshot, None, None, None) {
            out.append(Bound::new(py, topic_ref(Arc::clone(&self.snapshot), m))?)?;
        }
        Ok(out.unbind())
    }

    fn fields(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let out = PyList::empty(py);
        for m in find_fields(&self.snapshot, None, None, None, None) {
            out.append(Bound::new(py, field_ref(Arc::clone(&self.snapshot), m))?)?;
        }
        Ok(out.unbind())
    }
}

#[pymethods]
impl Delog {
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (topic, *, multiplier=1.0, offset=0.0, fields=None, unit=None, units=None, output_topic=None, source=None, instance=None, mode="both"))]
    fn transform(
        &self,
        topic: String,
        multiplier: f64,
        offset: f64,
        fields: Option<Vec<String>>,
        unit: Option<String>,
        units: Option<std::collections::HashMap<String, String>>,
        output_topic: Option<String>,
        source: Option<String>,
        instance: Option<u32>,
        mode: &str,
    ) -> PyResult<()> {
        let units = units.unwrap_or_default();
        validate_transform(multiplier, offset, unit.as_deref(), &units)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        if matches!(&fields, Some(fields) if fields.is_empty()) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "transform fields must not be empty",
            ));
        }
        if output_topic.as_deref() == Some("") {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "transform output_topic must not be empty",
            ));
        }
        let output_topic = output_topic.unwrap_or_else(|| topic.clone());
        let mode = Some(mode);
        let mode = OperationMode::parse(mode.as_deref())
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        self.operations
            .borrow_mut()
            .push(OperationSpec::Transform(TransformSpec {
                input: TopicSelector {
                    topic,
                    source,
                    instance,
                },
                multiplier,
                offset,
                fields,
                unit,
                units,
                output_topic,
                mode,
            }));
        Ok(())
    }

    #[pyo3(signature = (topics, *, base_topic, output_topic, source=None, mode="both"))]
    fn merge(
        &self,
        topics: &Bound<'_, PyMapping>,
        base_topic: String,
        output_topic: String,
        source: Option<String>,
        mode: &str,
    ) -> PyResult<()> {
        if topics.is_empty()? {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "merge topics must not be empty",
            ));
        }
        let mut ordered_topics = Vec::with_capacity(topics.len()?);
        for item in topics.items()?.iter() {
            let item = item.cast::<PyTuple>()?;
            let topic = item.get_item(0)?.extract::<String>().map_err(|_| {
                pyo3::exceptions::PyValueError::new_err("merge topic names must be strings")
            })?;
            let fields = item.get_item(1)?.extract::<Vec<String>>().map_err(|_| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "merge topic '{topic}' fields must be a list of strings"
                ))
            })?;
            if fields.is_empty() {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "merge topic '{topic}' fields must not be empty"
                )));
            }
            ordered_topics.push((topic, fields));
        }
        if !ordered_topics.iter().any(|(topic, _)| topic == &base_topic) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "merge base_topic '{base_topic}' must be present in topics"
            )));
        }
        let borrowed = ordered_topics
            .iter()
            .map(|(topic, fields)| {
                (
                    topic.as_str(),
                    fields.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let flat_names =
            merged_field_names(&borrowed).map_err(pyo3::exceptions::PyValueError::new_err)?;
        let mut names = flat_names.into_iter();
        let output_names = ordered_topics
            .iter()
            .map(|(_, fields)| names.by_ref().take(fields.len()).collect::<Vec<_>>())
            .collect();
        let mode = Some(mode);
        let mode = OperationMode::parse(mode.as_deref())
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        self.operations
            .borrow_mut()
            .push(OperationSpec::Merge(MergeSpec {
                topics: ordered_topics,
                base_topic,
                output_topic,
                source,
                output_names,
                mode,
            }));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (topic, field, *, fields=None, output_topic=None, source=None, instance=None, mode="both"))]
    fn group_by(
        &self,
        topic: String,
        field: String,
        fields: Option<Vec<String>>,
        output_topic: Option<String>,
        source: Option<String>,
        instance: Option<u32>,
        mode: &str,
    ) -> PyResult<()> {
        if matches!(&fields, Some(fields) if fields.is_empty()) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "group_by fields must not be empty",
            ));
        }
        let output_template = output_topic.unwrap_or_else(|| "{topic}/{value}".to_owned());
        validate_group_template(&output_template)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        let mode = Some(mode);
        let mode = OperationMode::parse(mode.as_deref())
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        self.operations
            .borrow_mut()
            .push(OperationSpec::GroupBy(GroupBySpec {
                input: TopicSelector {
                    topic,
                    source,
                    instance,
                },
                field,
                fields,
                output_template,
                mode,
            }));
        Ok(())
    }

    fn catalog(&self) -> CatalogPy {
        CatalogPy {
            snapshot: Arc::clone(&self.snapshot),
        }
    }

    #[pyo3(signature = (name, *, source=None, instance=None))]
    fn topic(
        &self,
        name: &str,
        source: Option<&str>,
        instance: Option<u32>,
    ) -> PyResult<TopicRefPy> {
        unique_topic(Arc::clone(&self.snapshot), name, source, instance)
    }

    #[pyo3(signature = (topic, field=None, *, source=None, instance=None))]
    fn find(
        &self,
        topic: &str,
        field: Option<&str>,
        source: Option<&str>,
        instance: Option<u32>,
        py: Python<'_>,
    ) -> PyResult<Py<PyAny>> {
        if let Some(field_name) = field {
            let matches = find_fields(
                &self.snapshot,
                Some(topic),
                Some(field_name),
                source,
                instance,
            );
            return match matches.len() {
                1 => Ok(Bound::new(
                    py,
                    field_ref(
                        Arc::clone(&self.snapshot),
                        matches.into_iter().next().unwrap(),
                    ),
                )?
                .into_any()
                .unbind()),
                0 => {
                    let candidates =
                        find_fields(&self.snapshot, Some(topic), None, source, instance);
                    if candidates.is_empty() {
                        Err(pyo3::exceptions::PyKeyError::new_err(format!(
                            "field '{field_name}' not found in topic '{topic}'"
                        )))
                    } else {
                        Err(pyo3::exceptions::PyKeyError::new_err(format!(
                            "field '{field_name}' not found in topic '{topic}'; candidates: {}",
                            candidate_field_paths(&candidates)
                        )))
                    }
                }
                _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "field '{field_name}' in topic '{topic}' is ambiguous; candidates: {}; pass source= or instance=",
                    candidate_field_paths(&matches)
                ))),
            };
        }
        Ok(Bound::new(
            py,
            unique_topic(Arc::clone(&self.snapshot), topic, source, instance)?,
        )?
        .into_any()
        .unbind())
    }

    #[pyo3(signature = (topic=None, field=None, *, source=None, instance=None))]
    fn find_all(
        &self,
        py: Python<'_>,
        topic: Option<&str>,
        field: Option<&str>,
        source: Option<&str>,
        instance: Option<u32>,
    ) -> PyResult<Py<PyList>> {
        let out = PyList::empty(py);
        if field.is_some() {
            for m in find_fields(&self.snapshot, topic, field, source, instance) {
                out.append(Bound::new(py, field_ref(Arc::clone(&self.snapshot), m))?)?;
            }
            return Ok(out.unbind());
        }
        for m in find_topics(&self.snapshot, topic, source, instance) {
            out.append(Bound::new(py, topic_ref(Arc::clone(&self.snapshot), m))?)?;
        }
        Ok(out.unbind())
    }

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

    fn field(&self, py: Python<'_>, path: &Bound<'_, PyAny>) -> PyResult<DelogField> {
        if let Ok(field_ref) = path.extract::<PyRef<'_, FieldRefPy>>() {
            let (t, v, s) = materialize_field(&field_ref.snapshot, field_ref.field_id)
                .map_err(pyo3::exceptions::PyValueError::new_err)?;
            let s = s.map(|vals| numpy_str_array(py, vals)).transpose()?;
            return Ok(DelogField {
                t: t.into_pyarray(py).unbind(),
                v: v.into_pyarray(py).unbind(),
                s,
            });
        }
        let path: String = path.extract()?;
        let id = self
            .resolve_path(&path)
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

    fn emit(
        &self,
        name: &str,
        times_us: numpy::PyReadonlyArray1<i64>,
        fields: &Bound<'_, PyDict>,
    ) -> PyResult<()> {
        if fields.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "emit topic '{name}' must contain at least one field"
            )));
        }
        let times = times_us.as_slice()?.to_vec();
        let mut topic = PendingTopic::new(name.to_owned(), times);
        for (key, value) in fields.iter() {
            let field_name: String = key.extract().map_err(|_| {
                pyo3::exceptions::PyValueError::new_err("emit field names must be strings")
            })?;
            let (values, unit) = parse_emit_field_entry(&field_name, &value, topic.times.len())?;
            topic
                .add_field(PendingField::numeric(field_name, values, unit))
                .map_err(pyo3::exceptions::PyValueError::new_err)?;
        }
        self.emit.borrow_mut().push(topic);
        Ok(())
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
            kind: ParamKind::Slider {
                min,
                max,
                step,
                integer,
            },
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

fn value_to_py(
    py: Python<'_>,
    value: &ParamValue,
    kind: Option<&ParamKind>,
) -> PyResult<Py<PyAny>> {
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

fn extract_base_times(py: Python<'_>, base: &Bound<'_, PyAny>) -> PyResult<Vec<i64>> {
    if let Ok(field) = base.extract::<PyRef<'_, DelogField>>() {
        let t = field.t.bind(py).readonly();
        return Ok(t.as_slice()?.to_vec());
    }
    if let Ok(table) = base.extract::<PyRef<'_, DelogTable>>() {
        let t = table.t.bind(py).readonly();
        return Ok(t.as_slice()?.to_vec());
    }
    let arr: numpy::PyReadonlyArray1<i64> = base.extract().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(
            "align_prev base must be a DelogField, DelogTable, or int64 numpy array",
        )
    })?;
    Ok(arr.as_slice()?.to_vec())
}

#[pyclass(unsendable, name = "DelogTable")]
pub struct DelogTable {
    #[pyo3(get)]
    t: Py<PyArray1<i64>>,
    fields: Vec<String>,
    columns: std::collections::HashMap<String, Py<PyAny>>,
}

#[pymethods]
impl DelogTable {
    fn fields(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        Ok(PyList::new(py, self.fields.clone())?.unbind())
    }

    fn __getitem__(&self, name: &str) -> PyResult<Py<PyAny>> {
        self.columns
            .get(name)
            .map(|obj| Python::attach(|py| obj.clone_ref(py)))
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(name.to_owned()))
    }

    fn __getattr__(&self, name: &str) -> PyResult<Py<PyAny>> {
        self.__getitem__(name)
            .map_err(|_| pyo3::exceptions::PyAttributeError::new_err(name.to_owned()))
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

#[pymethods]
impl DelogField {
    fn align_prev(&self, py: Python<'_>, base: &Bound<'_, PyAny>) -> PyResult<Py<PyArray1<f64>>> {
        let src_t = self.t.bind(py).readonly();
        let src_v = self.v.bind(py).readonly();
        let base_times = extract_base_times(py, base)?;
        let out = resample_prev(src_t.as_slice()?, src_v.as_slice()?, &base_times);
        Ok(out.into_pyarray(py).unbind())
    }
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
            .add_field(PendingField::numeric(name, vals, unit))
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{OperationBuffer, OperationMode, OperationSpec};
    use arrow::array::{ArrayRef, Float64Array, Int64Array};

    #[test]
    fn declarative_methods_expose_both_as_the_mode_default() {
        Python::attach(|py| {
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    OperationBuffer::default(),
                    String::new(),
                    0,
                    crate::params::shared_empty(),
                ),
            )
            .unwrap();
            let inspect = py.import("inspect").unwrap();
            for method in ["transform", "merge", "group_by"] {
                let signature = inspect
                    .call_method1("signature", (delog.getattr(method).unwrap(),))
                    .unwrap();
                let default: String = signature
                    .getattr("parameters")
                    .unwrap()
                    .get_item("mode")
                    .unwrap()
                    .getattr("default")
                    .unwrap()
                    .extract()
                    .unwrap();
                assert_eq!(default, "both", "wrong mode default for {method}");
            }
        });
    }

    #[test]
    fn declarative_methods_register_validated_specs_in_python_order() {
        Python::attach(|py| {
            let operations = OperationBuffer::default();
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    Rc::clone(&operations),
                    String::new(),
                    0,
                    crate::params::shared_empty(),
                ),
            )
            .unwrap();
            let locals = PyDict::new(py);
            locals.set_item("delog", delog).unwrap();
            let code = std::ffi::CString::new(
                r#"
from collections import UserDict
delog.transform("ATTITUDE", multiplier=57.29577951308232)
delog.merge(UserDict({"ATTITUDE": ["roll"], "GPS": ["alt"]}),
            base_topic="ATTITUDE", output_topic="STATE")
delog.group_by("PARAM_VALUE", "param_id")
"#,
            )
            .unwrap();
            py.run(&code, None, Some(&locals)).unwrap();

            let specs = operations.borrow();
            assert_eq!(specs.len(), 3);
            let OperationSpec::Transform(transform) = &specs[0] else {
                panic!("first operation was not transform")
            };
            assert_eq!(transform.input.topic, "ATTITUDE");
            assert_eq!(transform.output_topic, "ATTITUDE");
            assert_eq!(transform.mode, OperationMode::Both);

            let OperationSpec::Merge(merge) = &specs[1] else {
                panic!("second operation was not merge")
            };
            assert_eq!(
                merge
                    .topics
                    .iter()
                    .map(|(topic, _)| topic.as_str())
                    .collect::<Vec<_>>(),
                vec!["ATTITUDE", "GPS"]
            );
            assert_eq!(merge.output_names, vec![vec!["roll"], vec!["alt"]]);

            let OperationSpec::GroupBy(group) = &specs[2] else {
                panic!("third operation was not group_by")
            };
            assert_eq!(group.output_template, "{topic}/{value}");
            assert_eq!(group.mode, OperationMode::Both);
        });
    }

    #[test]
    fn invalid_declarative_calls_do_not_register_partial_specs() {
        Python::attach(|py| {
            let operations = OperationBuffer::default();
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    Rc::clone(&operations),
                    String::new(),
                    0,
                    crate::params::shared_empty(),
                ),
            )
            .unwrap();
            let locals = PyDict::new(py);
            locals.set_item("delog", delog).unwrap();
            let invalid_calls = [
                r#"delog.transform("A", multiplier=float("nan"))"#,
                r#"delog.transform("A", unit="deg", units={"x": "rad"})"#,
                r#"delog.transform("A", fields=[])"#,
                r#"delog.transform("A", mode="stream")"#,
                r#"delog.transform("A", mode=None)"#,
                r#"delog.merge({}, base_topic="A", output_topic="OUT")"#,
                r#"delog.merge({"A": ["x"]}, base_topic="B", output_topic="OUT")"#,
                r#"delog.group_by("A", "key", fields=[])"#,
                r#"delog.group_by("A", "key", output_topic="{topic}/fixed")"#,
            ];
            for call in invalid_calls {
                let code = std::ffi::CString::new(call).unwrap();
                let err = py.run(&code, None, Some(&locals)).unwrap_err();
                assert!(
                    err.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
                        || err.is_instance_of::<pyo3::exceptions::PyValueError>(py),
                    "unexpected error for {call}: {err}"
                );
                assert!(operations.borrow().is_empty());
            }
        });
    }

    #[test]
    fn transform_rejects_explicit_empty_output_topic_without_registering() {
        Python::attach(|py| {
            let operations = OperationBuffer::default();
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    Rc::clone(&operations),
                    String::new(),
                    0,
                    crate::params::shared_empty(),
                ),
            )
            .unwrap();
            let locals = PyDict::new(py);
            locals.set_item("delog", delog).unwrap();
            let code = std::ffi::CString::new(r#"delog.transform("A", output_topic="")"#).unwrap();

            let error = py.run(&code, None, Some(&locals)).unwrap_err();
            assert!(error.is_instance_of::<pyo3::exceptions::PyValueError>(py));
            assert!(
                error.to_string().contains("output_topic must not be empty"),
                "{error}"
            );
            assert!(operations.borrow().is_empty());
        });
    }

    #[test]
    fn output_builder_collects_topics_and_fields() {
        let mut topic = super::PendingTopic::new("Mag".into(), vec![0, 100, 200]);
        topic
            .add_field(PendingField::numeric(
                "x",
                vec![1.0, 2.0, 3.0],
                Some("m".into()),
            ))
            .unwrap();
        topic
            .add_field(PendingField::numeric("y", vec![4.0, 5.0, 6.0], None))
            .unwrap();
        assert!(
            topic
                .add_field(PendingField::numeric("bad", vec![1.0], None))
                .is_err()
        );
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
    fn parse_topic_instance_suffixes() {
        assert_eq!(super::parse_topic_instance("IMU"), ("IMU".to_owned(), None));
        assert_eq!(
            super::parse_topic_instance("IMU[0]"),
            ("IMU".to_owned(), Some(0))
        );
        assert_eq!(
            super::parse_topic_instance("vehicle_attitude[12]"),
            ("vehicle_attitude".to_owned(), Some(12))
        );
        assert_eq!(
            super::parse_topic_instance("NAMED_VALUE_FLOAT/airspd"),
            ("NAMED_VALUE_FLOAT/airspd".to_owned(), None)
        );
        assert_eq!(
            super::parse_topic_instance("bad[x]"),
            ("bad[x]".to_owned(), None)
        );
        assert_eq!(
            super::parse_topic_instance("bad[]"),
            ("bad[]".to_owned(), None)
        );
    }

    #[test]
    fn snapshot_lookup_finds_topics_and_fields() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let imu = id.add_topic_instance(src, "IMU", 0).unwrap();
        let gps = id.add_topic(src, "GPS").unwrap();
        let accx = id.add_field(imu, "AccX").unwrap();
        let accy = id.add_field(imu, "AccY").unwrap();
        let alt = id.add_field(gps, "Alt").unwrap();

        let imu_schema = Arc::new(
            TopicSchema::new(
                "IMU[0]",
                [
                    FieldSchema::new("AccX", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                    FieldSchema::new("AccY", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let gps_schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let imu_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![
                    Arc::new(Float64Array::from(vec![1.0])) as ArrayRef,
                    Arc::new(Float64Array::from(vec![2.0])) as ArrayRef,
                ],
                &imu_schema,
            )
            .unwrap(),
        );
        let gps_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![Arc::new(Float64Array::from(vec![100.0])) as ArrayRef],
                &gps_schema,
            )
            .unwrap(),
        );
        let imu_store = Arc::new(TopicStore::from_chunks(imu_schema, [imu_chunk]).unwrap());
        let gps_store = Arc::new(TopicStore::from_chunks(gps_schema, [gps_chunk]).unwrap());
        let snap =
            StoreSnapshot::from_registry(&id, [(imu, imu_store), (gps, gps_store)], 0).unwrap();

        let topics = super::find_topics(&snap, Some("IMU"), None, Some(0));
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].topic_id, imu);
        assert_eq!(topics[0].source_label, "flight");
        assert_eq!(topics[0].topic_name, "IMU[0]");
        assert_eq!(topics[0].base_name, "IMU");
        assert_eq!(topics[0].instance, Some(0));

        let fields = super::find_fields(&snap, Some("IMU"), Some("AccX"), None, Some(0));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_id, accx);
        assert_eq!(fields[0].field_name, "AccX");
        assert_eq!(fields[0].unit.as_deref(), Some("m/s^2"));

        let all_fields = super::find_fields(&snap, Some("IMU"), None, None, Some(0));
        let ids: Vec<_> = all_fields.iter().map(|m| m.field_id).collect();
        assert_eq!(ids, vec![accx, accy]);

        let gps_fields = super::find_fields(&snap, Some("GPS"), Some("Alt"), Some("flight"), None);
        assert_eq!(gps_fields[0].field_id, alt);
    }

    #[test]
    fn topic_ref_field_lookup_keeps_exact_topic_identity() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let gps = id.add_topic(src, "GPS").unwrap();
        let gps0 = id.add_topic_instance(src, "GPS", 0).unwrap();
        let lat = id.add_field(gps, "Lat").unwrap();
        let fix = id.add_field(gps0, "Fix").unwrap();

        let gps_schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Lat", DataType::Float64, Some("deg"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let gps0_schema = Arc::new(
            TopicSchema::new(
                "GPS[0]",
                [FieldSchema::new("Fix", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let gps_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![Arc::new(Float64Array::from(vec![1.0])) as ArrayRef],
                &gps_schema,
            )
            .unwrap(),
        );
        let gps0_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![Arc::new(Float64Array::from(vec![2.0])) as ArrayRef],
                &gps0_schema,
            )
            .unwrap(),
        );
        let gps_store = Arc::new(TopicStore::from_chunks(gps_schema, [gps_chunk]).unwrap());
        let gps0_store = Arc::new(TopicStore::from_chunks(gps0_schema, [gps0_chunk]).unwrap());
        let snap = Arc::new(
            StoreSnapshot::from_registry(&id, [(gps, gps_store), (gps0, gps0_store)], 0).unwrap(),
        );

        let gps_ref = TopicRefPy {
            snapshot: Arc::clone(&snap),
            topic_id: gps,
            source: "flight".to_owned(),
            name: "GPS".to_owned(),
            instance: None,
            path: "flight/GPS".to_owned(),
        };

        let lat_ref = gps_ref.field("Lat").unwrap();
        assert_eq!(lat_ref.field_id, lat);
        assert!(gps_ref.field("Fix").is_err());
        assert_ne!(lat_ref.field_id, fix);
    }

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
            OperationBuffer::default(),
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

use std::rc::Rc;
use std::sync::Arc;

use delog_api::catalog::{
    FieldMatch, TopicMatch, find_fields, find_fields_in_topic, find_topics, materialize_field,
    materialize_topic, resolve_field, resolve_field_in_topic, resolve_topic,
};
use delog_api::control::{ControlRequest, MarkerRequest, PlotContext, ScriptOwner};
use delog_api::markers::PendingMarker;
use delog_api::operations::{
    MergeSpec, OperationMode, OperationSpec, SplitBySpec, TopicSelector, TransformSpec,
};
use delog_api::params::{ParamKind, ParamSpec, ParamValue, SharedParams};
use delog_api::timestamps::{AlignmentMode, align_values};
use delog_core::derived::{PendingField, PendingTopic};
use delog_core::identity::{FieldId, TopicId};
use delog_core::snapshot::StoreSnapshot;

use numpy::{IntoPyArray, PyArray1, PyArrayMethods};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::types::PyList;
use pyo3::types::PyTuple;
use pyo3::types::{PyMapping, PyMappingMethods};

use crate::live::LiveTransformSpec;
use crate::staging::{
    EmitBuffer, LiveTransformBuffer, MarkerBuffer, OperationBuffer, PendingLiveTransform,
    active_marker_buffer,
};
use pyo3::types::{PyBool, PyInt};

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

/// `unsendable`: lives only on the worker thread under the GIL.
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
    live: LiveTransformBuffer,
    operations: OperationBuffer,
    markers: MarkerBuffer,
    batches: crate::control::DeferredControlBuffer,
    script_name: String,
    generation: u64,
    params: SharedParams,
}

impl Delog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot: Arc<StoreSnapshot>,
        emit: EmitBuffer,
        live: LiveTransformBuffer,
        operations: OperationBuffer,
        markers: MarkerBuffer,
        batches: crate::control::DeferredControlBuffer,
        script_name: String,
        generation: u64,
        params: SharedParams,
    ) -> Self {
        Self {
            snapshot,
            emit,
            live,
            operations,
            markers,
            batches,
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

    pub fn marker_buffer(&self) -> MarkerBuffer {
        Rc::clone(&self.markers)
    }

    fn owner(&self) -> Option<ScriptOwner> {
        (!self.script_name.is_empty()).then(|| ScriptOwner {
            name: self.script_name.clone(),
            generation: self.generation,
        })
    }

    fn marker_owner(&self) -> String {
        if self.script_name.is_empty() {
            "console".into()
        } else {
            self.script_name.clone()
        }
    }

    fn plot_context(&self) -> PlotContext {
        PlotContext {
            owner: self.owner(),
            snapshot: Arc::clone(&self.snapshot),
        }
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
pub(crate) struct FieldRefPy {
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

fn unique_topic(
    snapshot: Arc<StoreSnapshot>,
    name: &str,
    source: Option<&str>,
    instance: Option<u32>,
) -> PyResult<TopicRefPy> {
    let topic = resolve_topic(&snapshot, name, source, instance).map_err(crate::errors::lookup)?;
    Ok(topic_ref(snapshot, topic))
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
        let field = resolve_field_in_topic(&self.snapshot, self.topic_id, name)
            .map_err(crate::errors::lookup)?;
        Ok(field_ref(Arc::clone(&self.snapshot), field))
    }

    #[pyo3(signature = (*fields))]
    fn read(&self, py: Python<'_>, fields: &Bound<'_, PyTuple>) -> PyResult<DelogTable> {
        let fields = fields
            .iter()
            .map(|item| item.extract::<String>())
            .collect::<PyResult<Vec<_>>>()?;
        let table = materialize_topic(
            &self.snapshot,
            self.topic_id,
            &fields,
            crate::context::current_timestamp_mode(),
        )
        .map_err(crate::errors::lookup)?;
        let mut names = Vec::with_capacity(table.columns.len());
        let mut columns = std::collections::HashMap::new();
        for column in table.columns {
            columns.insert(
                column.name.clone(),
                materialized_values_to_py(py, column.values, column.strings)?,
            );
            names.push(column.name);
        }
        Ok(DelogTable {
            t: table.times_us.into_pyarray(py).unbind(),
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
        let field = materialize_field(
            &self.snapshot,
            self.field_id,
            crate::context::current_timestamp_mode(),
        )
        .map_err(crate::errors::value)?;
        let s = field
            .strings
            .map(|values| numpy_str_array(py, values))
            .transpose()?;
        Ok(DelogField {
            t: field.times_us.into_pyarray(py).unbind(),
            v: field.values.into_pyarray(py).unbind(),
            s,
        })
    }
}

impl FieldRefPy {
    pub(crate) fn field_id(&self) -> FieldId {
        self.field_id
    }

    pub(crate) fn label(&self) -> String {
        format!("{}.{}", self.topic, self.name)
    }

    pub(crate) fn qualified_path(&self) -> &str {
        &self.path
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
    #[pyo3(signature = (time_us, label, *, color=None, note=None))]
    fn add_marker(
        &self,
        time_us: i64,
        label: String,
        color: Option<String>,
        note: Option<String>,
    ) -> PyResult<()> {
        let marker = PendingMarker::new(time_us, label, color.as_deref(), note)
            .map_err(crate::errors::value)?;
        let request = ControlRequest::Markers(MarkerRequest::Append {
            owner: self.marker_owner(),
            generation: self.generation,
            markers: vec![marker.clone()],
        });
        if crate::control::stage_batch_request(&request)
            .map_err(crate::control::control_call_error)?
        {
            return Ok(());
        }
        let markers = active_marker_buffer().unwrap_or_else(|| Rc::clone(&self.markers));
        markers.borrow_mut().push(marker);
        Ok(())
    }

    #[pyo3(signature = (lat, lon, alt, *, dege7=false, alt_mm=false, alt_offset_m=0.0))]
    fn gps(
        &self,
        lat: Bound<'_, PyAny>,
        lon: Bound<'_, PyAny>,
        alt: Bound<'_, PyAny>,
        dege7: bool,
        alt_mm: bool,
        alt_offset_m: f64,
    ) -> PyResult<crate::control::vehicles::VehiclePositionPy> {
        crate::control::vehicles::gps(
            &self.snapshot,
            &lat,
            &lon,
            &alt,
            dege7,
            alt_mm,
            alt_offset_m,
        )
    }

    #[pyo3(signature = (north, east, down, *, reference=None))]
    fn ned(
        &self,
        north: Bound<'_, PyAny>,
        east: Bound<'_, PyAny>,
        down: Bound<'_, PyAny>,
        reference: Option<PyRef<'_, crate::control::vehicles::GeoReferencePy>>,
    ) -> PyResult<crate::control::vehicles::VehiclePositionPy> {
        crate::control::vehicles::ned(&self.snapshot, &north, &east, &down, reference.as_deref())
    }

    fn geo(
        &self,
        lat_deg: f64,
        lon_deg: f64,
        alt_m: f64,
    ) -> PyResult<crate::control::vehicles::GeoReferencePy> {
        crate::control::vehicles::geo(lat_deg, lon_deg, alt_m)
    }

    fn geo_fields(
        &self,
        lat: Bound<'_, PyAny>,
        lon: Bound<'_, PyAny>,
        alt: Bound<'_, PyAny>,
    ) -> PyResult<crate::control::vehicles::GeoReferencePy> {
        crate::control::vehicles::geo_fields(&self.snapshot, &lat, &lon, &alt)
    }

    #[pyo3(signature = (roll, pitch, yaw, *, degrees=false))]
    fn euler(
        &self,
        roll: Bound<'_, PyAny>,
        pitch: Bound<'_, PyAny>,
        yaw: Bound<'_, PyAny>,
        degrees: bool,
    ) -> PyResult<crate::control::vehicles::VehicleOrientationPy> {
        crate::control::vehicles::euler(&self.snapshot, &roll, &pitch, &yaw, degrees)
    }

    fn quat(
        &self,
        w: Bound<'_, PyAny>,
        x: Bound<'_, PyAny>,
        y: Bound<'_, PyAny>,
        z: Bound<'_, PyAny>,
    ) -> PyResult<crate::control::vehicles::VehicleOrientationPy> {
        crate::control::vehicles::quat(&self.snapshot, &w, &x, &y, &z)
    }

    fn static_ori(&self) -> crate::control::vehicles::VehicleOrientationPy {
        crate::control::vehicles::static_orientation()
    }

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
        let mode = OperationMode::parse(Some(mode)).map_err(crate::errors::value)?;
        let spec = TransformSpec::new(
            TopicSelector {
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
        )
        .map_err(crate::errors::value)?;
        self.operations
            .borrow_mut()
            .push(OperationSpec::Transform(spec));
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
            ordered_topics.push((topic, fields));
        }
        let mode = OperationMode::parse(Some(mode)).map_err(crate::errors::value)?;
        let spec = MergeSpec::new(ordered_topics, base_topic, output_topic, source, mode)
            .map_err(crate::errors::value)?;
        self.operations
            .borrow_mut()
            .push(OperationSpec::Merge(spec));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (topic, field, *, fields=None, output_topic=None, source=None, instance=None, mode="both"))]
    fn split_by(
        &self,
        topic: String,
        field: String,
        fields: Option<Vec<String>>,
        output_topic: Option<String>,
        source: Option<String>,
        instance: Option<u32>,
        mode: &str,
    ) -> PyResult<()> {
        let mode = OperationMode::parse(Some(mode)).map_err(crate::errors::value)?;
        let spec = SplitBySpec::new(
            TopicSelector {
                topic,
                source,
                instance,
            },
            field,
            fields,
            output_topic,
            mode,
        )
        .map_err(crate::errors::value)?;
        self.operations
            .borrow_mut()
            .push(OperationSpec::SplitBy(spec));
        Ok(())
    }

    fn catalog(&self) -> CatalogPy {
        CatalogPy {
            snapshot: Arc::clone(&self.snapshot),
        }
    }

    #[pyo3(signature = (*, window=None))]
    fn plots(
        &self,
        py: Python<'_>,
        window: Option<u64>,
    ) -> PyResult<Vec<crate::control::plots::PlotPy>> {
        crate::control::plots::list_plots(py, window, self.plot_context())
    }

    fn focused_plot(&self, py: Python<'_>) -> PyResult<Option<crate::control::plots::PlotPy>> {
        crate::control::plots::focused_plot(py, self.plot_context())
    }

    #[getter]
    fn workspace(&self) -> crate::control::workspace::WorkspacePy {
        crate::control::workspace::WorkspacePy::new(self.plot_context())
    }

    #[getter]
    fn windows(&self) -> crate::control::workspace::WindowsPy {
        crate::control::workspace::WindowsPy
    }

    #[getter]
    fn annotations(&self) -> crate::control::annotations::GlobalAnnotationsPy {
        crate::control::annotations::GlobalAnnotationsPy
    }

    #[getter]
    fn markers(&self) -> crate::control::markers::MarkerCollectionPy {
        crate::control::markers::MarkerCollectionPy::new(
            Rc::clone(&self.markers),
            self.marker_owner(),
            self.generation,
        )
    }

    #[getter]
    fn layouts(&self) -> crate::control::layouts::LayoutsPy {
        crate::control::layouts::LayoutsPy
    }

    fn batch(&self) -> crate::control::BatchPy {
        crate::control::BatchPy::new(Rc::clone(&self.batches))
    }

    #[getter]
    fn vehicles(&self) -> crate::control::vehicles::VehicleCollectionPy {
        crate::control::vehicles::VehicleCollectionPy::new(self.plot_context())
    }

    #[getter]
    fn vehicle_profiles(&self) -> crate::control::vehicles::profiles::VehicleProfilesPy {
        crate::control::vehicles::profiles::VehicleProfilesPy::new(self.plot_context())
    }

    #[getter]
    fn playback(&self) -> crate::control::workspace::PlaybackPy {
        crate::control::workspace::PlaybackPy
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
            let field = resolve_field(&self.snapshot, topic, field_name, source, instance)
                .map_err(crate::errors::lookup)?;
            return Ok(
                Bound::new(py, field_ref(Arc::clone(&self.snapshot), field))?
                    .into_any()
                    .unbind(),
            );
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
            markers: MarkerBuffer,
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
                    markers: Rc::clone(&self.markers),
                });
                Ok(func)
            }
        }

        Ok(Bound::new(
            py,
            Decorator {
                spec,
                live: Rc::clone(&self.live),
                markers: self.marker_buffer(),
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
        let script = crate::context::current_script().unwrap_or_else(|| self.script_name.clone());
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
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
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
            "align base must be a DelogField, DelogTable, or int64 numpy array",
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
    #[pyo3(signature = (base, mode="prev"))]
    fn align(
        &self,
        py: Python<'_>,
        base: &Bound<'_, PyAny>,
        mode: &str,
    ) -> PyResult<Py<PyArray1<f64>>> {
        let src_t = self.t.bind(py).readonly();
        let src_v = self.v.bind(py).readonly();
        let base_times = extract_base_times(py, base)?;
        let out = align_values(
            src_t.as_slice()?,
            src_v.as_slice()?,
            &base_times,
            AlignmentMode::parse(mode).map_err(crate::errors::value)?,
        );
        Ok(out.into_pyarray(py).unbind())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staging::OperationBuffer;
    use delog_api::operations::{OperationMode, OperationSpec};

    fn test_delog_with_markers(markers: MarkerBuffer) -> Delog {
        Delog::new(
            Arc::new(StoreSnapshot::empty()),
            EmitBuffer::default(),
            LiveTransformBuffer::default(),
            OperationBuffer::default(),
            markers,
            crate::control::DeferredControlBuffer::default(),
            String::new(),
            0,
            delog_api::params::shared_empty(),
        )
    }

    #[test]
    fn add_marker_validates_and_stages_values() {
        Python::attach(|py| {
            let markers = MarkerBuffer::default();
            let delog = Bound::new(py, test_delog_with_markers(Rc::clone(&markers))).unwrap();
            let locals = PyDict::new(py);
            locals.set_item("delog", delog).unwrap();
            let code = std::ffi::CString::new(
                r##"
delog.add_marker(42, "launch")
delog.add_marker(43, "rgb", color="#112233", note="opaque")
delog.add_marker(44, "rgba", color="#11223344")
"##,
            )
            .unwrap();
            py.run(&code, None, Some(&locals)).unwrap();

            assert_eq!(
                *markers.borrow(),
                vec![
                    PendingMarker {
                        time_us: 42,
                        label: "launch".into(),
                        color: None,
                        note: String::new(),
                    },
                    PendingMarker {
                        time_us: 43,
                        label: "rgb".into(),
                        color: Some([17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 1.0]),
                        note: "opaque".into(),
                    },
                    PendingMarker {
                        time_us: 44,
                        label: "rgba".into(),
                        color: Some([17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 68.0 / 255.0,]),
                        note: String::new(),
                    },
                ]
            );
        });
    }

    #[test]
    fn add_marker_rejects_invalid_values_without_staging() {
        Python::attach(|py| {
            let markers = MarkerBuffer::default();
            let delog = Bound::new(py, test_delog_with_markers(Rc::clone(&markers))).unwrap();
            let locals = PyDict::new(py);
            locals.set_item("delog", delog).unwrap();
            let invalid_calls = [
                (r#"delog.add_marker(1, "")"#, "ValueError"),
                (
                    r#"delog.add_marker(1, "bad", color="112233")"#,
                    "ValueError",
                ),
                (
                    r##"delog.add_marker(1, "bad", color="#123")"##,
                    "ValueError",
                ),
                (
                    r##"delog.add_marker(1, "bad", color="#GG2233")"##,
                    "ValueError",
                ),
                (
                    r##"delog.add_marker(1, "bad", color="#aéaaa")"##,
                    "ValueError",
                ),
                (r#"delog.add_marker(1.5, "bad")"#, "TypeError"),
                (r#"delog.add_marker(1, 2)"#, "TypeError"),
                (r#"delog.add_marker(1, "bad", color=2)"#, "TypeError"),
                (r#"delog.add_marker(1, "bad", note=2)"#, "TypeError"),
            ];
            for (call, expected_type) in invalid_calls {
                let code = std::ffi::CString::new(call).unwrap();
                let error = py.run(&code, None, Some(&locals)).unwrap_err();
                match expected_type {
                    "ValueError" => {
                        assert!(error.is_instance_of::<pyo3::exceptions::PyValueError>(py))
                    }
                    "TypeError" => {
                        assert!(error.is_instance_of::<pyo3::exceptions::PyTypeError>(py))
                    }
                    _ => unreachable!(),
                }
                assert!(markers.borrow().is_empty(), "staged rejected call: {call}");
            }
        });
    }

    #[test]
    fn marker_collection_is_not_a_legacy_method_alias_and_live_transforms_capture_its_buffer() {
        Python::attach(|py| {
            let markers = MarkerBuffer::default();
            let live = LiveTransformBuffer::default();
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    Rc::clone(&live),
                    OperationBuffer::default(),
                    Rc::clone(&markers),
                    crate::control::DeferredControlBuffer::default(),
                    String::new(),
                    0,
                    delog_api::params::shared_empty(),
                ),
            )
            .unwrap();
            for alias in ["add_markers", "marker"] {
                assert!(!delog.hasattr(alias).unwrap(), "unexpected alias: {alias}");
            }
            assert!(delog.hasattr("markers").unwrap());
            let locals = PyDict::new(py);
            locals.set_item("delog", delog).unwrap();
            let code = std::ffi::CString::new(
                r#"
@delog.live_transform(topic="A", fields=["x"])
def callback(batch):
    return None
"#,
            )
            .unwrap();
            py.run(&code, None, Some(&locals)).unwrap();

            assert_eq!(live.borrow().len(), 1);
            assert!(Rc::ptr_eq(&live.borrow()[0].markers, &markers));
        });
    }

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
                    MarkerBuffer::default(),
                    crate::control::DeferredControlBuffer::default(),
                    String::new(),
                    0,
                    delog_api::params::shared_empty(),
                ),
            )
            .unwrap();
            let inspect = py.import("inspect").unwrap();
            for method in ["transform", "merge", "split_by"] {
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
    fn legacy_methods_are_not_exposed() {
        Python::attach(|py| {
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    OperationBuffer::default(),
                    MarkerBuffer::default(),
                    crate::control::DeferredControlBuffer::default(),
                    String::new(),
                    0,
                    delog_api::params::shared_empty(),
                ),
            )
            .unwrap();

            for removed in ["sources", "field", "resample_prev", "output"] {
                assert!(
                    !delog.hasattr(removed).unwrap(),
                    "{removed} is still exposed"
                );
            }
            for retained in ["catalog", "topic", "find", "find_all", "emit"] {
                assert!(delog.hasattr(retained).unwrap(), "{retained} is missing");
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
                    MarkerBuffer::default(),
                    crate::control::DeferredControlBuffer::default(),
                    String::new(),
                    0,
                    delog_api::params::shared_empty(),
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
delog.split_by("PARAM_VALUE", "param_id")
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

            let OperationSpec::SplitBy(group) = &specs[2] else {
                panic!("third operation was not split_by")
            };
            assert_eq!(group.output_template, "{topic}/{value}");
            assert_eq!(group.mode, OperationMode::Both);
        });
    }

    #[test]
    fn invalid_declarative_methods_do_not_register_partial_specs() {
        Python::attach(|py| {
            let operations = OperationBuffer::default();
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    Rc::clone(&operations),
                    MarkerBuffer::default(),
                    crate::control::DeferredControlBuffer::default(),
                    String::new(),
                    0,
                    delog_api::params::shared_empty(),
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
                r#"delog.split_by("A", "key", fields=[])"#,
                r#"delog.split_by("A", "key", output_topic="{topic}/fixed")"#,
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
    fn declarative_methods_reject_explicit_empty_output_topic_without_registering() {
        Python::attach(|py| {
            let operations = OperationBuffer::default();
            let delog = Bound::new(
                py,
                Delog::new(
                    Arc::new(StoreSnapshot::empty()),
                    EmitBuffer::default(),
                    LiveTransformBuffer::default(),
                    Rc::clone(&operations),
                    MarkerBuffer::default(),
                    crate::control::DeferredControlBuffer::default(),
                    String::new(),
                    0,
                    delog_api::params::shared_empty(),
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
    fn pending_topic_validates_field_lengths() {
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
}

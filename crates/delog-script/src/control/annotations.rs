use delog_api::color::{format_hex_color, parse_hex_color};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyIterator, PyList};

use super::{
    AnnotationFilter, AnnotationGeometry, AnnotationInfo, AnnotationKind, AnnotationRequest,
    AnnotationStylePatch, ControlRequest, PlotContext, call_immediate_detached, control_call_error,
};

#[pyclass(unsendable, name = "AnnotationCollection", skip_from_py_object)]
#[derive(Clone)]
pub struct AnnotationCollectionPy {
    window: u64,
    tile: u64,
    context: PlotContext,
}

impl AnnotationCollectionPy {
    pub(crate) fn new(window: u64, tile: u64, context: PlotContext) -> Self {
        Self {
            window,
            tile,
            context,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn submit(
        &self,
        py: Python<'_>,
        geometry: AnnotationGeometry,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let style = build_style_patch(color, stroke_px, fill_opacity, font_px, arrow)?;
        let request = AnnotationRequest::Add {
            window: self.window,
            tile: self.tile,
            geometry,
            label: label.to_string(),
            style,
            owner: self.context.owner.clone(),
        };
        let response = call_immediate_detached(py, ControlRequest::Annotations(request))
            .map_err(control_call_error)?;
        let mut infos = response
            .into_annotations()
            .map_err(crate::errors::control)?;
        if infos.len() == 1 {
            Ok(annotation_from_info(infos.remove(0)))
        } else {
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "the DeLOG window answered with the wrong kind of result",
            ))
        }
    }

    fn add_mapping(&self, py: Python<'_>, item: &Bound<'_, PyDict>) -> PyResult<AnnotationPy> {
        let kind_name: String = item
            .get_item("kind")?
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("annotation needs kind="))?
            .extract()
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("kind must be a string"))?;
        let kind = parse_kind(&kind_name)?;
        let at = item.get_item("at")?;
        let a = item.get_item("a")?;
        let b = item.get_item("b")?;
        let y = dict_f64(item, "y")?;
        let geometry = geometry_for(kind, at.as_ref(), a.as_ref(), b.as_ref(), y)?;
        let label = dict_string(item, "label")?.unwrap_or_default();
        let color = dict_string(item, "color")?;
        let stroke_px = dict_f32(item, "stroke_px")?;
        let fill_opacity = dict_f32(item, "fill_opacity")?;
        let font_px = dict_f32(item, "font_px")?;
        let arrow = dict_bool(item, "arrow")?;
        self.submit(
            py,
            geometry,
            &label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }
}

#[pymethods]
impl AnnotationCollectionPy {
    #[pyo3(signature = (at, label="", *, color=None, stroke_px=None, fill_opacity=None, font_px=None, arrow=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_text(
        &self,
        py: Python<'_>,
        at: Bound<'_, PyAny>,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let at = AnnotationGeometry::text(parse_point(&at, "at")?).map_err(crate::errors::value)?;
        self.submit(
            py,
            at,
            label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }

    #[pyo3(signature = (from, to, label="", *, color=None, stroke_px=None, fill_opacity=None, font_px=None, arrow=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_segment(
        &self,
        py: Python<'_>,
        from: Bound<'_, PyAny>,
        to: Bound<'_, PyAny>,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let geometry =
            AnnotationGeometry::segment(parse_point(&from, "from")?, parse_point(&to, "to")?)
                .map_err(crate::errors::value)?;
        self.submit(
            py,
            geometry,
            label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }

    #[pyo3(signature = (a, b, label="", *, color=None, stroke_px=None, fill_opacity=None, font_px=None, arrow=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_rect(
        &self,
        py: Python<'_>,
        a: Bound<'_, PyAny>,
        b: Bound<'_, PyAny>,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let geometry = AnnotationGeometry::rect(parse_point(&a, "a")?, parse_point(&b, "b")?)
            .map_err(crate::errors::value)?;
        self.submit(
            py,
            geometry,
            label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }

    #[pyo3(signature = (a, b, label="", *, color=None, stroke_px=None, fill_opacity=None, font_px=None, arrow=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_ellipse(
        &self,
        py: Python<'_>,
        a: Bound<'_, PyAny>,
        b: Bound<'_, PyAny>,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let geometry = AnnotationGeometry::ellipse(parse_point(&a, "a")?, parse_point(&b, "b")?)
            .map_err(crate::errors::value)?;
        self.submit(
            py,
            geometry,
            label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }

    #[pyo3(signature = (y, label="", *, color=None, stroke_px=None, fill_opacity=None, font_px=None, arrow=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_hline(
        &self,
        py: Python<'_>,
        y: f64,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let geometry = AnnotationGeometry::hline(y).map_err(crate::errors::value)?;
        self.submit(
            py,
            geometry,
            label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }

    #[pyo3(signature = (kind, *, at=None, a=None, b=None, y=None, label="", color=None, stroke_px=None, fill_opacity=None, font_px=None, arrow=None))]
    #[allow(clippy::too_many_arguments)]
    fn add(
        &self,
        py: Python<'_>,
        kind: &str,
        at: Option<Bound<'_, PyAny>>,
        a: Option<Bound<'_, PyAny>>,
        b: Option<Bound<'_, PyAny>>,
        y: Option<f64>,
        label: &str,
        color: Option<String>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> PyResult<AnnotationPy> {
        let kind = parse_kind(kind)?;
        let geometry = geometry_for(kind, at.as_ref(), a.as_ref(), b.as_ref(), y)?;
        self.submit(
            py,
            geometry,
            label,
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        )
    }

    fn extend(&self, py: Python<'_>, items: Vec<Bound<'_, PyDict>>) -> PyResult<Vec<AnnotationPy>> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(self.add_mapping(py, &item)?);
        }
        Ok(out)
    }

    #[pyo3(signature = (target=None, *, kind=None, label=None, owner=None))]
    fn remove(
        &self,
        py: Python<'_>,
        target: Option<Bound<'_, PyAny>>,
        kind: Option<String>,
        label: Option<String>,
        owner: Option<String>,
    ) -> PyResult<()> {
        let target_filter = target
            .as_ref()
            .map(|value| self.parse_local_target(value))
            .transpose()?;
        let filter = resolve_removal_filter(target_filter, kind.as_deref(), label, owner)?;
        self.submit_remove(py, filter)
    }

    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        self.submit_remove(py, AnnotationFilter::All)
    }

    fn list(&self, py: Python<'_>) -> PyResult<Vec<AnnotationPy>> {
        Ok(request_annotations(py, Some((self.window, self.tile)))?
            .into_iter()
            .map(annotation_from_info)
            .collect())
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(self.list(py)?.len())
    }

    fn __getitem__(&self, py: Python<'_>, index: usize) -> PyResult<AnnotationPy> {
        self.list(py)?
            .into_iter()
            .nth(index)
            .ok_or_else(|| pyo3::exceptions::PyIndexError::new_err("annotation index out of range"))
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        let out = PyList::empty(py);
        for annotation in self.list(py)? {
            out.append(Bound::new(py, annotation)?)?;
        }
        out.try_iter()
    }
}

impl AnnotationCollectionPy {
    fn parse_local_target(&self, value: &Bound<'_, PyAny>) -> PyResult<AnnotationFilter> {
        if let Ok(index) = value.extract::<usize>() {
            return Ok(AnnotationFilter::Index(index));
        }
        if let Ok(handle) = value.extract::<PyRef<'_, AnnotationPy>>() {
            if handle.window != self.window || handle.tile != self.tile {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "this annotation belongs to a different plot",
                ));
            }
            return Ok(AnnotationFilter::Id(handle.id));
        }
        Err(pyo3::exceptions::PyValueError::new_err(
            "remove() position must be an index or an Annotation handle",
        ))
    }

    fn submit_remove(&self, py: Python<'_>, filter: AnnotationFilter) -> PyResult<()> {
        let request = AnnotationRequest::Remove {
            target: Some((self.window, self.tile)),
            filter,
        };
        call_immediate_detached(py, ControlRequest::Annotations(request))
            .map_err(control_call_error)?
            .into_unit()
            .map_err(crate::errors::control)
    }
}

fn parse_kind(name: &str) -> PyResult<AnnotationKind> {
    AnnotationKind::parse(name).map_err(crate::errors::value)
}

fn geometry_for(
    kind: AnnotationKind,
    at: Option<&Bound<'_, PyAny>>,
    a: Option<&Bound<'_, PyAny>>,
    b: Option<&Bound<'_, PyAny>>,
    y: Option<f64>,
) -> PyResult<AnnotationGeometry> {
    match kind {
        AnnotationKind::Text => AnnotationGeometry::text(parse_point(require(at, "at")?, "at")?)
            .map_err(crate::errors::value),
        AnnotationKind::Segment => AnnotationGeometry::segment(
            parse_point(require(a, "a")?, "a")?,
            parse_point(require(b, "b")?, "b")?,
        )
        .map_err(crate::errors::value),
        AnnotationKind::Rect => AnnotationGeometry::rect(
            parse_point(require(a, "a")?, "a")?,
            parse_point(require(b, "b")?, "b")?,
        )
        .map_err(crate::errors::value),
        AnnotationKind::Ellipse => AnnotationGeometry::ellipse(
            parse_point(require(a, "a")?, "a")?,
            parse_point(require(b, "b")?, "b")?,
        )
        .map_err(crate::errors::value),
        AnnotationKind::HLine => {
            let y = y.ok_or_else(|| pyo3::exceptions::PyValueError::new_err("hline needs y="))?;
            AnnotationGeometry::hline(y).map_err(crate::errors::value)
        }
    }
}

fn require<'a, 'py>(
    value: Option<&'a Bound<'py, PyAny>>,
    name: &str,
) -> PyResult<&'a Bound<'py, PyAny>> {
    value
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err(format!("annotation needs {name}=")))
}

fn parse_point(value: &Bound<'_, PyAny>, name: &str) -> PyResult<(i64, f64)> {
    let (t_us, y): (i64, f64) = value.extract().map_err(|_| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "{name} must be a (t_us, y) pair of int and float"
        ))
    })?;
    Ok((t_us, y))
}

fn dict_string(item: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match item.get_item(key)? {
        Some(v) => v.extract::<String>().map(Some).map_err(|_| wrong_type(key)),
        None => Ok(None),
    }
}

fn dict_f64(item: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<f64>> {
    match item.get_item(key)? {
        Some(v) => v.extract::<f64>().map(Some).map_err(|_| wrong_type(key)),
        None => Ok(None),
    }
}

fn dict_f32(item: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<f32>> {
    match item.get_item(key)? {
        Some(v) => v.extract::<f32>().map(Some).map_err(|_| wrong_type(key)),
        None => Ok(None),
    }
}

fn dict_bool(item: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<bool>> {
    match item.get_item(key)? {
        Some(v) => v.extract::<bool>().map(Some).map_err(|_| wrong_type(key)),
        None => Ok(None),
    }
}

fn wrong_type(key: &str) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(format!("{key} has the wrong type"))
}

fn build_style_patch(
    color: Option<String>,
    stroke_px: Option<f32>,
    fill_opacity: Option<f32>,
    font_px: Option<f32>,
    arrow: Option<bool>,
) -> PyResult<AnnotationStylePatch> {
    let color = color
        .as_deref()
        .map(parse_hex_color)
        .transpose()
        .map_err(crate::errors::value)?;
    AnnotationStylePatch::new(color, stroke_px, fill_opacity, font_px, arrow)
        .map_err(crate::errors::value)
}

fn resolve_removal_filter(
    target: Option<AnnotationFilter>,
    kind: Option<&str>,
    label: Option<String>,
    owner: Option<String>,
) -> PyResult<AnnotationFilter> {
    let kind = kind
        .map(parse_kind)
        .transpose()?
        .map(AnnotationFilter::Kind);
    let label = label.map(AnnotationFilter::Label);
    let owner = owner.map(AnnotationFilter::Owner);
    match (target, kind, label, owner) {
        (Some(filter), None, None, None) => Ok(filter),
        (None, Some(filter), None, None) => Ok(filter),
        (None, None, Some(filter), None) => Ok(filter),
        (None, None, None, Some(filter)) => Ok(filter),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            "remove() needs exactly one of a position, an Annotation handle, kind=, label=, or owner=",
        )),
    }
}

fn parse_global_target(value: &Bound<'_, PyAny>) -> PyResult<(u64, u64, u64)> {
    if let Ok(handle) = value.extract::<PyRef<'_, AnnotationPy>>() {
        return Ok((handle.window, handle.tile, handle.id));
    }
    Err(pyo3::exceptions::PyValueError::new_err(
        "remove() position must be an Annotation handle",
    ))
}

fn request_annotations(
    py: Python<'_>,
    target: Option<(u64, u64)>,
) -> PyResult<Vec<AnnotationInfo>> {
    let response = call_immediate_detached(
        py,
        ControlRequest::Annotations(AnnotationRequest::List { target }),
    )
    .map_err(control_call_error)?;
    response.into_annotations().map_err(crate::errors::control)
}

fn annotation_from_info(info: AnnotationInfo) -> AnnotationPy {
    AnnotationPy {
        window: info.window,
        tile: info.tile,
        id: info.id,
        index: info.index,
        kind: info.kind,
        geometry: info.geometry,
        label: info.label,
        color: info.color,
        owner: info.owner,
    }
}

fn kind_name(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Text => "text",
        AnnotationKind::Segment => "segment",
        AnnotationKind::Rect => "rect",
        AnnotationKind::Ellipse => "ellipse",
        AnnotationKind::HLine => "hline",
    }
}

#[pyclass(unsendable, name = "Annotation", skip_from_py_object)]
#[derive(Clone)]
pub struct AnnotationPy {
    window: u64,
    tile: u64,
    #[pyo3(get)]
    id: u64,
    #[pyo3(get)]
    index: usize,
    kind: AnnotationKind,
    geometry: AnnotationGeometry,
    label: String,
    color: [f32; 4],
    owner: Option<String>,
}

#[pymethods]
impl AnnotationPy {
    fn __repr__(&self) -> String {
        format!(
            "<Annotation {} kind={} id={}>",
            self.label,
            kind_name(self.kind),
            self.id
        )
    }

    #[getter]
    fn kind(&self) -> &'static str {
        kind_name(self.kind)
    }

    #[getter]
    fn label(&self) -> String {
        self.label.clone()
    }

    #[setter]
    fn set_label(&mut self, py: Python<'_>, label: String) -> PyResult<()> {
        self.set(
            py,
            Some(label.clone()),
            None,
            AnnotationStylePatch::default(),
        )?;
        self.label = label;
        Ok(())
    }

    #[getter]
    fn color(&self) -> String {
        format_hex_color(self.color)
    }

    #[setter]
    fn set_color(&mut self, py: Python<'_>, color: String) -> PyResult<()> {
        let parsed = parse_hex_color(&color).map_err(crate::errors::value)?;
        let style = AnnotationStylePatch::new(Some(parsed), None, None, None, None)
            .map_err(crate::errors::value)?;
        self.set(py, None, None, style)?;
        self.color = parsed;
        Ok(())
    }

    #[getter]
    fn owner(&self) -> Option<String> {
        self.owner.clone()
    }

    #[getter]
    fn y(&self) -> PyResult<f64> {
        match self.geometry {
            AnnotationGeometry::HLine { y } => Ok(y),
            _ => Err(pyo3::exceptions::PyValueError::new_err(
                "y is only available on hline annotations",
            )),
        }
    }

    #[setter]
    fn set_y(&mut self, py: Python<'_>, y: f64) -> PyResult<()> {
        if !matches!(self.kind, AnnotationKind::HLine) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "y can only be set on hline annotations",
            ));
        }
        let geometry = AnnotationGeometry::hline(y).map_err(crate::errors::value)?;
        self.set(
            py,
            None,
            Some(geometry.clone()),
            AnnotationStylePatch::default(),
        )?;
        self.geometry = geometry;
        Ok(())
    }

    fn move_to(&mut self, py: Python<'_>, point: Bound<'_, PyAny>) -> PyResult<()> {
        if matches!(self.kind, AnnotationKind::HLine) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "move_to cannot be used on hline annotations; set y instead",
            ));
        }
        let geometry = self
            .geometry
            .clone()
            .moved_to(parse_point(&point, "point")?)
            .map_err(crate::errors::value)?;
        self.set(
            py,
            None,
            Some(geometry.clone()),
            AnnotationStylePatch::default(),
        )?;
        self.geometry = geometry;
        Ok(())
    }
}

impl AnnotationPy {
    fn set(
        &self,
        py: Python<'_>,
        label: Option<String>,
        geometry: Option<AnnotationGeometry>,
        style: AnnotationStylePatch,
    ) -> PyResult<()> {
        let request = AnnotationRequest::Set {
            window: self.window,
            tile: self.tile,
            id: self.id,
            label,
            geometry,
            style,
        };
        call_immediate_detached(py, ControlRequest::Annotations(request))
            .map_err(control_call_error)?
            .into_unit()
            .map_err(crate::errors::control)
    }
}

#[pyclass(unsendable, name = "GlobalAnnotations", skip_from_py_object)]
#[derive(Clone, Default)]
pub struct GlobalAnnotationsPy;

#[pymethods]
impl GlobalAnnotationsPy {
    #[pyo3(signature = (target=None, *, kind=None, label=None, owner=None))]
    fn remove(
        &self,
        py: Python<'_>,
        target: Option<Bound<'_, PyAny>>,
        kind: Option<String>,
        label: Option<String>,
        owner: Option<String>,
    ) -> PyResult<()> {
        let handle = target.as_ref().map(parse_global_target).transpose()?;
        let target_filter = handle.map(|(_, _, id)| AnnotationFilter::Id(id));
        let filter = resolve_removal_filter(target_filter, kind.as_deref(), label, owner)?;
        let scope = handle.map(|(window, tile, _)| (window, tile));
        submit_global_remove(py, scope, filter)
    }

    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        submit_global_remove(py, None, AnnotationFilter::All)
    }

    fn list(&self, py: Python<'_>) -> PyResult<Vec<AnnotationPy>> {
        Ok(request_annotations(py, None)?
            .into_iter()
            .map(annotation_from_info)
            .collect())
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(self.list(py)?.len())
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        let out = PyList::empty(py);
        for annotation in self.list(py)? {
            out.append(Bound::new(py, annotation)?)?;
        }
        out.try_iter()
    }
}

fn submit_global_remove(
    py: Python<'_>,
    target: Option<(u64, u64)>,
    filter: AnnotationFilter,
) -> PyResult<()> {
    let request = AnnotationRequest::Remove { target, filter };
    call_immediate_detached(py, ControlRequest::Annotations(request))
        .map_err(control_call_error)?
        .into_unit()
        .map_err(crate::errors::control)
}

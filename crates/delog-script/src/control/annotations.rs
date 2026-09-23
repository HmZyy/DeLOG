use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyIterator, PyList};

use crate::api::parse_marker_color;

use super::{
    AnnotationFilter, AnnotationGeometry, AnnotationInfo, AnnotationKind, AnnotationRequest,
    AnnotationStylePatch, ControlRequest, ControlResponse, PlotContext, call_immediate_detached,
    control_call_error,
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
        match response {
            ControlResponse::Annotations(mut infos) if infos.len() == 1 => {
                Ok(annotation_from_info(infos.remove(0)))
            }
            _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "the DeLOG window answered with the wrong kind of result",
            )),
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
        let at = parse_point(&at, "at")?;
        self.submit(
            py,
            AnnotationGeometry::Text { at },
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
        let from = parse_point(&from, "from")?;
        let to = parse_point(&to, "to")?;
        self.submit(
            py,
            AnnotationGeometry::Segment { from, to },
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
        let a = parse_point(&a, "a")?;
        let b = parse_point(&b, "b")?;
        self.submit(
            py,
            AnnotationGeometry::Rect { a, b },
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
        let a = parse_point(&a, "a")?;
        let b = parse_point(&b, "b")?;
        self.submit(
            py,
            AnnotationGeometry::Ellipse { a, b },
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
        let y = finite_f64(y, "y")?;
        self.submit(
            py,
            AnnotationGeometry::HLine { y },
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
            .map(|_| ())
            .map_err(control_call_error)
    }
}

fn parse_kind(name: &str) -> PyResult<AnnotationKind> {
    AnnotationKind::parse(name).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "annotation kind must be 'text', 'segment', 'rect', 'ellipse', or 'hline', got {name:?}"
        ))
    })
}

fn geometry_for(
    kind: AnnotationKind,
    at: Option<&Bound<'_, PyAny>>,
    a: Option<&Bound<'_, PyAny>>,
    b: Option<&Bound<'_, PyAny>>,
    y: Option<f64>,
) -> PyResult<AnnotationGeometry> {
    match kind {
        AnnotationKind::Text => Ok(AnnotationGeometry::Text {
            at: parse_point(require(at, "at")?, "at")?,
        }),
        AnnotationKind::Segment => Ok(AnnotationGeometry::Segment {
            from: parse_point(require(a, "a")?, "a")?,
            to: parse_point(require(b, "b")?, "b")?,
        }),
        AnnotationKind::Rect => Ok(AnnotationGeometry::Rect {
            a: parse_point(require(a, "a")?, "a")?,
            b: parse_point(require(b, "b")?, "b")?,
        }),
        AnnotationKind::Ellipse => Ok(AnnotationGeometry::Ellipse {
            a: parse_point(require(a, "a")?, "a")?,
            b: parse_point(require(b, "b")?, "b")?,
        }),
        AnnotationKind::HLine => {
            let y = y.ok_or_else(|| pyo3::exceptions::PyValueError::new_err("hline needs y="))?;
            Ok(AnnotationGeometry::HLine {
                y: finite_f64(y, "y")?,
            })
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
    Ok((t_us, finite_f64(y, &format!("{name}.y"))?))
}

fn finite_f64(value: f64, name: &str) -> PyResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(pyo3::exceptions::PyValueError::new_err(format!(
            "{name} must be finite"
        )))
    }
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
    let color = color.as_deref().map(parse_marker_color).transpose()?;
    reject_non_finite(stroke_px, "stroke_px")?;
    reject_non_finite(fill_opacity, "fill_opacity")?;
    reject_non_finite(font_px, "font_px")?;
    Ok(AnnotationStylePatch {
        color,
        stroke_px,
        fill_opacity,
        font_px,
        arrow,
    })
}

fn reject_non_finite(value: Option<f32>, name: &str) -> PyResult<()> {
    match value {
        Some(value) if !value.is_finite() => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "{name} must be finite"
        ))),
        _ => Ok(()),
    }
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
    match response {
        ControlResponse::Annotations(infos) => Ok(infos),
        _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
            "the DeLOG window answered with the wrong kind of result",
        )),
    }
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

fn moved_to(geometry: AnnotationGeometry, point: (i64, f64)) -> AnnotationGeometry {
    let delta = |anchor: (i64, f64)| (point.0 - anchor.0, point.1 - anchor.1);
    let shift = |p: (i64, f64), d: (i64, f64)| (p.0 + d.0, p.1 + d.1);
    match geometry {
        AnnotationGeometry::Text { .. } => AnnotationGeometry::Text { at: point },
        AnnotationGeometry::Segment { from, to } => {
            let d = delta(from);
            AnnotationGeometry::Segment {
                from: point,
                to: shift(to, d),
            }
        }
        AnnotationGeometry::Rect { a, b } => {
            let d = delta(a);
            AnnotationGeometry::Rect {
                a: point,
                b: shift(b, d),
            }
        }
        AnnotationGeometry::Ellipse { a, b } => {
            let d = delta(a);
            AnnotationGeometry::Ellipse {
                a: point,
                b: shift(b, d),
            }
        }
        AnnotationGeometry::HLine { y } => AnnotationGeometry::HLine { y },
    }
}

fn format_color(color: [f32; 4]) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        byte(color[0]),
        byte(color[1]),
        byte(color[2]),
        byte(color[3])
    )
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
        format_color(self.color)
    }

    #[setter]
    fn set_color(&mut self, py: Python<'_>, color: String) -> PyResult<()> {
        let parsed = parse_marker_color(&color)?;
        let style = AnnotationStylePatch {
            color: Some(parsed),
            ..Default::default()
        };
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
        let y = finite_f64(y, "y")?;
        let geometry = AnnotationGeometry::HLine { y };
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
        let point = parse_point(&point, "point")?;
        let geometry = moved_to(self.geometry.clone(), point);
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
            .map(|_| ())
            .map_err(control_call_error)
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
        .map(|_| ())
        .map_err(control_call_error)
}

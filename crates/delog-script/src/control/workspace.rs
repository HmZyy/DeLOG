use pyo3::prelude::*;

use super::plots::{PlotPy, plot_from_info};
use super::{
    ControlRequest, ControlResponse, PlaybackRequest, PlotContext, SplitDirection,
    WorkspaceRequest, call_immediate_detached, control_call_error,
};

#[pyclass(unsendable, name = "Workspace", skip_from_py_object)]
#[derive(Clone)]
pub struct WorkspacePy {
    context: PlotContext,
}

impl WorkspacePy {
    pub(crate) fn new(context: PlotContext) -> Self {
        Self { context }
    }
}

#[pymethods]
impl WorkspacePy {
    #[pyo3(signature = (*, split="horizontal"))]
    fn add_plot(&self, py: Python<'_>, split: &str) -> PyResult<PlotPy> {
        let direction = parse_split_direction(split)?;
        self.request_plot(py, WorkspaceRequest::AddPlot { direction })
    }

    fn split(&self, py: Python<'_>, plot: PyRef<'_, PlotPy>, direction: &str) -> PyResult<PlotPy> {
        let direction = parse_split_direction(direction)?;
        self.request_plot(
            py,
            WorkspaceRequest::Split {
                window: plot.window,
                tile: plot.tile,
                direction,
            },
        )
    }

    fn close(&self, py: Python<'_>, plot: PyRef<'_, PlotPy>) -> PyResult<()> {
        self.request_unit(
            py,
            WorkspaceRequest::Close {
                window: plot.window,
                tile: plot.tile,
            },
        )
    }

    fn equalize(&self, py: Python<'_>) -> PyResult<()> {
        self.request_unit(py, WorkspaceRequest::Equalize)
    }

    fn show_scene(&self, py: Python<'_>, visible: bool) -> PyResult<()> {
        self.request_unit(py, WorkspaceRequest::ShowScene { visible })
    }
}

impl WorkspacePy {
    fn request_plot(&self, py: Python<'_>, request: WorkspaceRequest) -> PyResult<PlotPy> {
        let response = call_immediate_detached(py, ControlRequest::Workspace(request))
            .map_err(control_call_error)?;
        match response {
            ControlResponse::Plots(infos) => match infos.into_iter().next() {
                Some(info) => Ok(plot_from_info(info, self.context.clone())),
                None => Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "the DeLOG window did not report the new plot",
                )),
            },
            _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "the DeLOG window answered with the wrong kind of result",
            )),
        }
    }

    fn request_unit(&self, py: Python<'_>, request: WorkspaceRequest) -> PyResult<()> {
        call_immediate_detached(py, ControlRequest::Workspace(request))
            .map(|_| ())
            .map_err(control_call_error)
    }
}

#[pyclass(unsendable, name = "Windows", skip_from_py_object)]
#[derive(Clone, Default)]
pub struct WindowsPy;

#[pymethods]
impl WindowsPy {
    #[pyo3(signature = (*, title=None))]
    fn open(&self, py: Python<'_>, title: Option<String>) -> PyResult<WindowPy> {
        let response = call_immediate_detached(
            py,
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow { title }),
        )
        .map_err(control_call_error)?;
        match response {
            ControlResponse::Window(id) => Ok(WindowPy { id }),
            _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "the DeLOG window answered with the wrong kind of result",
            )),
        }
    }
}

#[pyclass(unsendable, name = "Window", skip_from_py_object)]
#[derive(Clone)]
pub struct WindowPy {
    #[pyo3(get)]
    id: u64,
}

#[pymethods]
impl WindowPy {
    fn __repr__(&self) -> String {
        format!("<Window {}>", self.id)
    }
}

#[pyclass(unsendable, name = "Playback", skip_from_py_object)]
#[derive(Clone, Default)]
pub struct PlaybackPy;

#[pymethods]
impl PlaybackPy {
    #[setter]
    fn set_speed(&self, py: Python<'_>, speed: f64) -> PyResult<()> {
        if !speed.is_finite() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "playback speed must be finite, got {speed}"
            )));
        }
        self.request(
            py,
            PlaybackRequest::Set {
                speed: Some(speed),
                follow_live: None,
            },
        )
    }

    #[setter]
    fn set_follow_live(&self, py: Python<'_>, follow_live: bool) -> PyResult<()> {
        self.request(
            py,
            PlaybackRequest::Set {
                speed: None,
                follow_live: Some(follow_live),
            },
        )
    }
}

impl PlaybackPy {
    fn request(&self, py: Python<'_>, request: PlaybackRequest) -> PyResult<()> {
        call_immediate_detached(py, ControlRequest::Playback(request))
            .map(|_| ())
            .map_err(control_call_error)
    }
}

fn parse_split_direction(name: &str) -> PyResult<SplitDirection> {
    SplitDirection::parse(name).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "split direction must be 'horizontal' or 'vertical', got {name:?}"
        ))
    })
}

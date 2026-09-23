use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use pyo3::Python;

use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::api::PendingMarker;

pub mod plots;
pub mod testing;
pub mod traces;
pub mod workspace;

#[derive(Debug, Clone, PartialEq)]
pub struct PlotInfo {
    pub window: u64,
    pub tile: u64,
    pub index: usize,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Line,
    Scatter,
    Step,
}

impl TraceMode {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "line" => Some(Self::Line),
            "scatter" => Some(Self::Scatter),
            "step" => Some(Self::Step),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceInfo {
    pub index: usize,
    pub field_id: FieldId,
    pub field: String,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceRequest {
    List {
        window: u64,
        tile: u64,
    },
    Add {
        window: u64,
        tile: u64,
        field_id: FieldId,
        field: String,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: TraceMode,
        owner: Option<ScriptOwner>,
    },
    Remove {
        window: u64,
        tile: u64,
        index: Option<usize>,
        field_id: Option<FieldId>,
        field: Option<String>,
    },
    Clear {
        window: u64,
        tile: u64,
    },
    Set {
        window: u64,
        tile: u64,
        index: usize,
        field_id: FieldId,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: Option<TraceMode>,
        visible: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerRequest {
    Replace {
        owner: String,
        generation: u64,
        markers: Vec<PendingMarker>,
    },
    Append {
        owner: String,
        generation: u64,
        markers: Vec<PendingMarker>,
    },
    Remove {
        owner: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlotRequest {
    List { window: Option<u64> },
    Focused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl SplitDirection {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceRequest {
    AddPlot {
        direction: SplitDirection,
    },
    Split {
        window: u64,
        tile: u64,
        direction: SplitDirection,
    },
    Close {
        window: u64,
        tile: u64,
    },
    Equalize,
    ShowScene {
        visible: bool,
    },
    OpenWindow {
        title: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackRequest {
    Set {
        speed: Option<f64>,
        follow_live: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptOwner {
    pub name: String,
    pub generation: u64,
}

#[derive(Clone)]
pub struct PlotContext {
    pub owner: Option<ScriptOwner>,
    pub snapshot: Arc<StoreSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationRequest {
    Commit { owner: String, generation: u64 },
    Rollback { owner: String, generation: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    Markers(MarkerRequest),
    Plots(PlotRequest),
    Traces(TraceRequest),
    Generation(GenerationRequest),
    Workspace(WorkspaceRequest),
    Playback(PlaybackRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    Unit,
    Plots(Vec<PlotInfo>),
    Traces(Vec<TraceInfo>),
    Window(u64),
}

pub trait ControlHost: Send + Sync {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String>;
}

thread_local! {
    static HOST: RefCell<Option<Arc<dyn ControlHost>>> = const { RefCell::new(None) };
}

pub struct HostGuard {
    previous: Option<Arc<dyn ControlHost>>,
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        HOST.with(|host| {
            *host.borrow_mut() = self.previous.take();
        });
    }
}

pub fn install_host(host: Option<Arc<dyn ControlHost>>) -> HostGuard {
    let previous = HOST.with(|current| std::mem::replace(&mut *current.borrow_mut(), host));
    HostGuard { previous }
}

pub fn current_host() -> Option<Arc<dyn ControlHost>> {
    HOST.with(|host| host.borrow().clone())
}

fn missing_host() -> String {
    "the DeLOG control API is not available here; it works in the scripting console \
     and in named script runs, not inside live transforms, parsers, or flow scripts"
        .into()
}

/// Round-trips a request to the host with the interpreter detached, so other
/// Python threads keep running and a pending interrupt can unblock the wait.
pub fn call_immediate_detached(
    py: Python<'_>,
    request: ControlRequest,
) -> Result<ControlResponse, String> {
    let host = current_host().ok_or_else(missing_host)?;
    py.detach(move || host.call(request))
}

#[derive(Default)]
pub struct RecordingHost {
    seen: Mutex<Vec<ControlRequest>>,
    error: Option<String>,
}

impl RecordingHost {
    pub fn failing(error: &str) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            error: Some(error.into()),
        }
    }

    pub fn taken(&self) -> Vec<ControlRequest> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

impl ControlHost for RecordingHost {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String> {
        self.seen.lock().unwrap().push(request);
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(ControlResponse::Unit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::time::Duration;

    fn call(request: ControlRequest) -> Result<ControlResponse, String> {
        Python::attach(|py| call_immediate_detached(py, request))
    }

    #[test]
    fn immediate_calls_without_a_host_report_the_missing_context() {
        let error = call(ControlRequest::Markers(MarkerRequest::Remove {
            owner: "flight.py".into(),
        }))
        .unwrap_err();
        assert!(error.contains("not available"), "{error}");
    }

    #[test]
    fn installing_a_host_routes_calls_to_it_and_uninstalls_on_drop() {
        let host = Arc::new(RecordingHost::default());
        {
            let _guard = install_host(Some(host.clone()));
            let response = call(ControlRequest::Markers(MarkerRequest::Remove {
                owner: "flight.py".into(),
            }))
            .unwrap();
            assert_eq!(response, ControlResponse::Unit);
        }
        assert_eq!(
            host.taken(),
            vec![ControlRequest::Markers(MarkerRequest::Remove {
                owner: "flight.py".into()
            })]
        );
        assert!(
            call(ControlRequest::Markers(MarkerRequest::Remove {
                owner: "x".into()
            }))
            .is_err()
        );
    }

    #[test]
    fn a_host_error_surfaces_verbatim() {
        let host = Arc::new(RecordingHost::failing("pane closed"));
        let _guard = install_host(Some(host));
        let error = call(ControlRequest::Markers(MarkerRequest::Remove {
            owner: "x".into(),
        }))
        .unwrap_err();
        assert_eq!(error, "pane closed");
    }

    #[test]
    fn installing_none_uninstalls_the_current_host() {
        let host = Arc::new(RecordingHost::default());
        let _outer = install_host(Some(host));
        {
            let _inner = install_host(None);
            assert!(current_host().is_none());
        }
        assert!(current_host().is_some());
    }

    struct BlockingHost {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl ControlHost for BlockingHost {
        fn call(&self, _request: ControlRequest) -> Result<ControlResponse, String> {
            let _ = self.entered.send(());
            let _ = self.release.lock().unwrap().recv();
            Ok(ControlResponse::Unit)
        }
    }

    #[test]
    fn a_waiting_call_leaves_the_interpreter_free_for_other_threads() {
        let (entered_tx, entered_rx) = sync_channel::<()>(1);
        let (release_tx, release_rx) = sync_channel::<()>(1);
        let host = Arc::new(BlockingHost {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let waiter = std::thread::spawn(move || {
            let _guard = install_host(Some(host as Arc<dyn ControlHost>));
            call(ControlRequest::Plots(PlotRequest::List { window: None }))
        });
        entered_rx.recv().unwrap();

        let (attached_tx, attached_rx) = sync_channel::<()>(1);
        let prober = std::thread::spawn(move || {
            Python::attach(|_py| {});
            let _ = attached_tx.send(());
        });
        let attached = attached_rx.recv_timeout(Duration::from_secs(10)).is_ok();

        let _ = release_tx.send(());
        prober.join().unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(ControlResponse::Unit));
        assert!(
            attached,
            "another thread could not attach to Python while the call waited"
        );
    }
}

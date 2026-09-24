use delog_api::control::*;

pub mod annotations;
pub mod layouts;
pub mod markers;
pub mod plots;
mod runtime;
pub mod testing;
pub mod traces;
pub mod vehicles;
pub mod workspace;

pub use runtime::{
    BatchPy, DeferredControlBuffer, HostGuard, RecordingHost, call_immediate_detached,
    control_call_error, current_host, install_host, stage_batch_request,
};

#[cfg(test)]
mod tests {
    use super::*;
    use delog_api::ErrorKind;
    use pyo3::Python;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn call(request: ControlRequest) -> delog_api::Result<ControlResponse> {
        Python::attach(|py| call_immediate_detached(py, request))
    }

    #[test]
    fn immediate_calls_without_a_host_report_the_missing_context() {
        let error = call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
            owner: "flight.py".into(),
        }))
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(error.to_string().contains("not available"), "{error}");
    }

    #[test]
    fn installing_a_host_routes_calls_to_it_and_uninstalls_on_drop() {
        let host = Arc::new(RecordingHost::default());
        {
            let _guard = install_host(Some(host.clone()));
            let response = call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into(),
            }))
            .unwrap();
            assert_eq!(response, ControlResponse::Unit);
        }
        assert_eq!(
            host.taken(),
            vec![ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into()
            })]
        );
        assert!(
            call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "x".into()
            }))
            .is_err()
        );
    }

    #[test]
    fn recording_host_preserves_vehicle_requests() {
        let host = Arc::new(RecordingHost::default());
        let _guard = install_host(Some(host.clone()));
        let request = ControlRequest::Vehicles(VehicleRequest::List);
        Python::attach(|py| call_immediate_detached(py, request.clone())).unwrap();
        assert_eq!(host.taken(), vec![request]);
    }

    #[test]
    fn recording_host_preserves_control_requests() {
        let host = Arc::new(RecordingHost::default());
        let _guard = install_host(Some(host.clone()));
        let marker = ControlRequest::Markers(MarkerRequest::List);
        let layout = ControlRequest::Layouts(LayoutRequest::Current);
        let batch = ControlRequest::Batch(vec![ControlRequest::Playback(PlaybackRequest::Set {
            speed: Some(2.0),
            follow_live: None,
        })]);
        let _report = ControlResponse::LoadReport(LoadReport::default());
        Python::attach(|py| {
            call_immediate_detached(py, marker.clone()).unwrap();
            call_immediate_detached(py, layout.clone()).unwrap();
            call_immediate_detached(py, batch.clone()).unwrap();
        });
        assert_eq!(host.taken(), vec![marker, layout, batch]);
    }

    #[test]
    fn a_host_error_surfaces_verbatim() {
        let host = Arc::new(RecordingHost::failing("pane closed"));
        let _guard = install_host(Some(host));
        let error = call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
            owner: "x".into(),
        }))
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Execution);
        assert_eq!(error.to_string(), "pane closed");
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
        fn call(&self, _request: ControlRequest) -> delog_api::Result<ControlResponse> {
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

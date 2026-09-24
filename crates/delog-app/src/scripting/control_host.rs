use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::time::Duration;

use delog_api::control::{ControlHost, ControlRequest, ControlResponse};
use delog_api::{Error, Result};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const CLAIMED_GRACE: Duration = Duration::from_secs(5);

const UNCLAIMED: u8 = 0;
const CLAIMED_BY_DRAIN: u8 = 1;
const ABANDONED: u8 = 2;

struct Pending {
    request: ControlRequest,
    reply: SyncSender<Result<ControlResponse>>,
    claim: Arc<AtomicU8>,
}

pub struct ScriptControlHost {
    tx: Sender<Pending>,
    ctx: egui::Context,
}

pub struct ControlQueue {
    rx: Receiver<Pending>,
}

impl ScriptControlHost {
    pub fn new(ctx: egui::Context) -> (Arc<Self>, ControlQueue) {
        let (tx, rx) = channel::<Pending>();
        (Arc::new(Self { tx, ctx }), ControlQueue { rx })
    }

    pub fn call_with_timeout(
        &self,
        request: ControlRequest,
        timeout: Duration,
    ) -> Result<ControlResponse> {
        let (reply_tx, reply_rx) = sync_channel::<Result<ControlResponse>>(1);
        let claim = Arc::new(AtomicU8::new(UNCLAIMED));
        self.tx
            .send(Pending {
                request,
                reply: reply_tx,
                claim: Arc::clone(&claim),
            })
            .map_err(|_| Error::unavailable("the DeLOG window is gone"))?;
        self.ctx.request_repaint();
        match reply_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                if claim
                    .compare_exchange(UNCLAIMED, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return Err(Error::unavailable("the DeLOG window did not respond"));
                }
                reply_rx
                    .recv_timeout(CLAIMED_GRACE)
                    .map_err(|_| Error::unavailable("the DeLOG window did not respond"))?
            }
        }
    }
}

impl ControlHost for ScriptControlHost {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse> {
        self.call_with_timeout(request, DEFAULT_TIMEOUT)
    }
}

impl ControlQueue {
    pub fn drain_with(
        &self,
        mut apply: impl FnMut(ControlRequest) -> std::result::Result<ControlResponse, String>,
    ) {
        while let Ok(pending) = self.rx.try_recv() {
            if pending
                .claim
                .compare_exchange(
                    UNCLAIMED,
                    CLAIMED_BY_DRAIN,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                continue;
            }
            let _ = pending
                .reply
                .send(apply(pending.request).map_err(Error::execution));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_api::ErrorKind;
    use delog_api::control::MarkerRequest;

    #[test]
    fn a_request_is_answered_by_whoever_drains_the_queue() {
        let (host, queue) = ScriptControlHost::new(egui::Context::default());
        let worker = std::thread::spawn(move || {
            host.call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into(),
            }))
        });
        let mut served = 0;
        while served == 0 {
            queue.drain_with(|_request| {
                served += 1;
                Ok(ControlResponse::Unit)
            });
        }
        assert_eq!(worker.join().unwrap(), Ok(ControlResponse::Unit));
    }

    #[test]
    fn a_queue_that_is_never_drained_times_out_rather_than_hanging() {
        let (host, _queue) = ScriptControlHost::new(egui::Context::default());
        let error = host
            .call_with_timeout(
                ControlRequest::Markers(MarkerRequest::RemoveOwned {
                    owner: "flight.py".into(),
                }),
                std::time::Duration::from_millis(50),
            )
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(error.to_string().contains("did not respond"), "{error}");
    }

    #[test]
    fn a_request_that_timed_out_is_never_applied_by_a_later_drain() {
        let (host, queue) = ScriptControlHost::new(egui::Context::default());
        let error = host
            .call_with_timeout(
                ControlRequest::Markers(MarkerRequest::RemoveOwned {
                    owner: "flight.py".into(),
                }),
                std::time::Duration::from_millis(50),
            )
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(error.to_string().contains("did not respond"), "{error}");

        let mut applied = 0;
        queue.drain_with(|_request| {
            applied += 1;
            Ok(ControlResponse::Unit)
        });
        assert_eq!(applied, 0);
    }

    #[test]
    fn a_request_the_drain_has_already_claimed_is_never_abandoned_midway() {
        let (host, queue) = ScriptControlHost::new(egui::Context::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let applied = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let applied_in_drain = std::sync::Arc::clone(&applied);

        let worker = std::thread::spawn(move || {
            host.call_with_timeout(
                ControlRequest::Markers(MarkerRequest::RemoveOwned {
                    owner: "flight.py".into(),
                }),
                std::time::Duration::from_millis(200),
            )
        });

        let drain = std::thread::spawn(move || {
            loop {
                let mut served = false;
                queue.drain_with(|_request| {
                    served = true;
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv();
                    applied_in_drain.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(ControlResponse::Unit)
                });
                if served {
                    return;
                }
            }
        });

        entered_rx.recv().expect("drain claimed the request");
        std::thread::sleep(std::time::Duration::from_millis(250));
        let _ = release_tx.send(());
        let outcome = worker.join().unwrap();
        drain.join().unwrap();

        assert!(
            outcome.is_ok(),
            "a request the drain had already claimed must be waited for, not abandoned: {outcome:?}"
        );
        assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn app_dispatch_errors_cross_the_queue_as_execution_errors() {
        let (host, queue) = ScriptControlHost::new(egui::Context::default());
        let worker = std::thread::spawn(move || {
            host.call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into(),
            }))
        });
        let mut served = false;
        while !served {
            queue.drain_with(|_request| {
                served = true;
                Err("pane closed".to_string())
            });
            std::thread::yield_now();
        }
        let error = worker.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Execution);
        assert_eq!(error.to_string(), "pane closed");
    }

    #[test]
    fn a_disconnected_window_is_an_unavailable_error() {
        let (host, queue) = ScriptControlHost::new(egui::Context::default());
        drop(queue);
        let error = host
            .call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into(),
            }))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.to_string(), "the DeLOG window is gone");
    }
}

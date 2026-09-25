use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use delog_api::control::{
    AuthorizedControlHost, CancelCheck, ControlCall, ControlHost, ControlPrincipal, ControlRequest,
    ControlResponse,
};
use delog_api::{Error, MutationCompletion, Result};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const CLAIMED_GRACE: Duration = Duration::from_secs(5);
const CANCEL_POLL: Duration = Duration::from_millis(10);

const UNCLAIMED: u8 = 0;
const CLAIMED_BY_DRAIN: u8 = 1;
const ABANDONED: u8 = 2;

struct Pending {
    call: ControlCall,
    reply: SyncSender<Result<ControlResponse>>,
    claim: Arc<AtomicU8>,
    cancelled: CancelCheck,
}

fn never_cancelled() -> CancelCheck {
    Arc::new(|| false)
}

pub struct AppControlHost {
    tx: SyncSender<Pending>,
    ctx: egui::Context,
}

pub struct ControlQueue {
    rx: Receiver<Pending>,
    capacity: usize,
    ctx: egui::Context,
}

impl AppControlHost {
    pub fn new(ctx: egui::Context, capacity: usize) -> (Arc<Self>, ControlQueue) {
        let (tx, rx) = sync_channel(capacity);
        let queue = ControlQueue {
            rx,
            capacity: capacity.max(1),
            ctx: ctx.clone(),
        };
        (Arc::new(Self { tx, ctx }), queue)
    }

    pub fn call_with_timeout(
        &self,
        request: ControlRequest,
        timeout: Duration,
    ) -> Result<ControlResponse> {
        self.call_with_timeout_call(ControlCall::Trusted(request), timeout)
    }

    fn call_with_timeout_call(
        &self,
        call: ControlCall,
        timeout: Duration,
    ) -> Result<ControlResponse> {
        self.call_with_timeouts_and_hooks(
            call,
            timeout,
            CLAIMED_GRACE,
            never_cancelled(),
            || {},
            || {},
        )
    }

    fn call_with_timeouts_and_hooks(
        &self,
        call: ControlCall,
        timeout: Duration,
        claimed_grace: Duration,
        cancelled: CancelCheck,
        on_enqueued: impl FnOnce(),
        on_claimed_timeout: impl FnOnce(),
    ) -> Result<ControlResponse> {
        let (reply_tx, reply_rx) = sync_channel(1);
        let claim = Arc::new(AtomicU8::new(UNCLAIMED));
        self.tx
            .try_send(Pending {
                call,
                reply: reply_tx,
                claim: Arc::clone(&claim),
                cancelled: Arc::clone(&cancelled),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::unavailable("the DeLOG control queue is full")
                    .with_completion(MutationCompletion::NotStarted),
                TrySendError::Disconnected(_) => Error::unavailable("the DeLOG window is gone")
                    .with_completion(MutationCompletion::NotStarted),
            })?;
        self.ctx.request_repaint();
        on_enqueued();
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match reply_rx.recv_timeout(remaining.min(CANCEL_POLL)) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) if !remaining.is_zero() && !cancelled() => {}
                Err(_) => break,
            }
        }
        let claimed = matches!(
            claim.compare_exchange(UNCLAIMED, ABANDONED, Ordering::SeqCst, Ordering::SeqCst),
            Err(CLAIMED_BY_DRAIN)
        );
        if !claimed {
            let message = if cancelled() {
                "the control was cancelled before DeLOG applied it"
            } else {
                "the DeLOG window did not respond"
            };
            return Err(Error::unavailable(message).with_completion(MutationCompletion::NotStarted));
        }
        on_claimed_timeout();
        reply_rx.recv_timeout(claimed_grace).map_err(|_| {
            Error::unavailable("the DeLOG window did not respond")
                .with_completion(MutationCompletion::Unknown)
        })?
    }
}

impl ControlHost for AppControlHost {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse> {
        self.call_with_timeout(request, DEFAULT_TIMEOUT)
    }
}

impl AuthorizedControlHost for AppControlHost {
    fn call_as(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
    ) -> Result<ControlResponse> {
        self.call_with_timeout_call(
            ControlCall::External { principal, request },
            DEFAULT_TIMEOUT,
        )
    }

    fn call_as_within(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
        timeout: Duration,
    ) -> Result<ControlResponse> {
        self.call_with_timeout_call(ControlCall::External { principal, request }, timeout)
    }

    fn call_as_cancellable(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
        timeout: Duration,
        cancelled: CancelCheck,
    ) -> Result<ControlResponse> {
        self.call_with_timeouts_and_hooks(
            ControlCall::External { principal, request },
            timeout,
            CLAIMED_GRACE,
            cancelled,
            || {},
            || {},
        )
    }
}

impl ControlQueue {
    pub fn drain_with(&self, mut apply: impl FnMut(ControlCall) -> Result<ControlResponse>) {
        for received in 0.. {
            if received == self.capacity {
                self.ctx.request_repaint();
                return;
            }
            let Ok(pending) = self.rx.try_recv() else {
                return;
            };
            if (pending.cancelled)() {
                let _ = pending.claim.compare_exchange(
                    UNCLAIMED,
                    ABANDONED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                continue;
            }
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
            let _ = pending.reply.send(apply(pending.call));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_api::ErrorKind;
    use delog_api::control::{AccessMode, MarkerRequest, ResourceOwner};

    fn unit_request() -> ControlRequest {
        ControlRequest::Markers(MarkerRequest::RemoveOwned {
            owner: "flight.py".into(),
        })
    }

    fn enqueue(tx: &SyncSender<Pending>) -> Receiver<Result<ControlResponse>> {
        let (reply, answer) = sync_channel(1);
        tx.try_send(Pending {
            call: ControlCall::Trusted(unit_request()),
            reply,
            claim: Arc::new(AtomicU8::new(UNCLAIMED)),
            cancelled: never_cancelled(),
        })
        .unwrap();
        answer
    }

    #[test]
    fn one_drain_serves_at_most_the_queue_capacity_and_schedules_another_frame() {
        let ctx = egui::Context::default();
        let (repaints, repaint_requests) = std::sync::mpsc::channel();
        ctx.set_request_repaint_callback(move |_| {
            repaints.send(()).unwrap();
        });
        let (host, queue) = AppControlHost::new(ctx, 2);
        let tx = host.tx.clone();
        let mut answers = vec![enqueue(&tx), enqueue(&tx)];

        let mut applied = 0;
        queue.drain_with(|_request| {
            applied += 1;
            answers.push(enqueue(&tx));
            Ok(ControlResponse::Unit)
        });

        assert_eq!(applied, 2);
        assert!(repaint_requests.try_recv().is_ok());
        assert_eq!(answers[0].try_recv().unwrap(), Ok(ControlResponse::Unit));
        assert_eq!(answers[1].try_recv().unwrap(), Ok(ControlResponse::Unit));
        assert!(answers[2].try_recv().is_err());

        let mut applied = 0;
        queue.drain_with(|_request| {
            applied += 1;
            Ok(ControlResponse::Unit)
        });
        assert_eq!(applied, 2);
        assert_eq!(answers[2].try_recv().unwrap(), Ok(ControlResponse::Unit));
        assert_eq!(answers[3].try_recv().unwrap(), Ok(ControlResponse::Unit));
    }

    #[test]
    fn a_request_is_answered_by_whoever_drains_the_queue() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let worker = std::thread::spawn(move || host.call(unit_request()));
        let mut served = 0;
        while served == 0 {
            queue.drain_with(|_request| {
                served += 1;
                Ok(ControlResponse::Unit)
            });
            std::thread::yield_now();
        }
        assert_eq!(worker.join().unwrap(), Ok(ControlResponse::Unit));
    }

    #[test]
    fn an_authorized_call_reaches_the_queue_with_its_authenticated_principal() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let principal = ControlPrincipal {
            owner: ResourceOwner {
                name: "flight-diagnosis".into(),
                generation: 7,
            },
            access: AccessMode::Safe,
        };
        let expected = principal.clone();
        let worker = std::thread::spawn(move || host.call_as(principal, unit_request()));
        let mut served = false;
        while !served {
            queue.drain_with(|call| {
                let ControlCall::External { principal, .. } = call else {
                    panic!("external call was downgraded to trusted")
                };
                assert_eq!(principal, expected);
                served = true;
                Ok(ControlResponse::Unit)
            });
            std::thread::yield_now();
        }
        assert_eq!(worker.join().unwrap(), Ok(ControlResponse::Unit));
    }

    #[test]
    fn a_queue_that_is_never_drained_times_out_rather_than_hanging() {
        let (host, _queue) = AppControlHost::new(egui::Context::default(), 8);
        let error = host
            .call_with_timeout(unit_request(), Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.completion(), Some(MutationCompletion::NotStarted));
        assert!(error.to_string().contains("did not respond"), "{error}");
    }

    fn external_principal() -> ControlPrincipal {
        ControlPrincipal {
            owner: ResourceOwner {
                name: "flight-diagnosis".into(),
                generation: 1,
            },
            access: AccessMode::Safe,
        }
    }

    #[test]
    fn a_cancelled_unclaimed_request_is_abandoned_before_its_timeout_and_never_applied() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let check = Arc::clone(&cancelled);
        let started = std::time::Instant::now();
        let caller = std::thread::spawn(move || {
            host.call_as_cancellable(
                external_principal(),
                unit_request(),
                Duration::from_secs(5),
                Arc::new(move || check.load(Ordering::SeqCst)),
            )
        });
        std::thread::sleep(Duration::from_millis(30));
        cancelled.store(true, Ordering::SeqCst);
        let error = caller.join().unwrap().unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.completion(), Some(MutationCompletion::NotStarted));
        assert!(error.to_string().contains("cancelled"), "{error}");

        let mut applied = 0;
        queue.drain_with(|_request| {
            applied += 1;
            Ok(ControlResponse::Unit)
        });
        assert_eq!(applied, 0);
    }

    #[test]
    fn a_drain_refuses_a_request_cancelled_before_its_caller_noticed() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let (queued_tx, queued_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let check = Arc::clone(&cancelled);
        let caller = std::thread::spawn(move || {
            host.call_with_timeouts_and_hooks(
                ControlCall::External {
                    principal: external_principal(),
                    request: unit_request(),
                },
                Duration::from_secs(5),
                CLAIMED_GRACE,
                Arc::new(move || check.load(Ordering::SeqCst)),
                || {
                    queued_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                },
                || {},
            )
        });
        queued_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancelled.store(true, Ordering::SeqCst);
        let mut applied = 0;
        queue.drain_with(|_request| {
            applied += 1;
            Ok(ControlResponse::Unit)
        });
        release_tx.send(()).unwrap();
        let error = caller.join().unwrap().unwrap_err();
        assert_eq!(applied, 0);
        assert_eq!(error.completion(), Some(MutationCompletion::NotStarted));
    }

    #[test]
    fn a_request_that_timed_out_is_never_applied_by_a_later_drain() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let error = host
            .call_with_timeout(unit_request(), Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.completion(), Some(MutationCompletion::NotStarted));
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
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let (queued_tx, queued_rx) = std::sync::mpsc::channel();
        let (start_wait_tx, start_wait_rx) = std::sync::mpsc::channel();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let (primary_timeout_tx, primary_timeout_rx) = std::sync::mpsc::channel();
        let (start_grace_tx, start_grace_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (drain_done_tx, drain_done_rx) = std::sync::mpsc::channel();
        let (answer_tx, answer_rx) = std::sync::mpsc::channel();
        let applied = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let applied_in_drain = std::sync::Arc::clone(&applied);

        let worker = std::thread::spawn(move || {
            let result = host.call_with_timeouts_and_hooks(
                ControlCall::Trusted(unit_request()),
                Duration::from_millis(20),
                Duration::from_millis(100),
                never_cancelled(),
                || {
                    queued_tx.send(()).unwrap();
                    start_wait_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                },
                || {
                    primary_timeout_tx.send(()).unwrap();
                    start_grace_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                },
            );
            answer_tx.send(result).unwrap();
        });
        queued_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("request enqueued");
        let drain = std::thread::spawn(move || {
            queue.drain_with(|_request| {
                claimed_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                applied_in_drain.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ControlResponse::Unit)
            });
            drain_done_tx.send(()).unwrap();
        });
        claimed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drain claimed before first timeout");
        start_wait_tx.send(()).unwrap();
        primary_timeout_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("caller reached the claimed grace wait");
        release_tx.send(()).unwrap();
        drain_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drain finished after release");
        drain.join().unwrap();
        start_grace_tx.send(()).unwrap();
        let outcome = answer_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("claimed request answered during grace");
        worker.join().unwrap();

        assert!(
            outcome.is_ok(),
            "a request the drain had already claimed must be waited for, not abandoned: {outcome:?}"
        );
        assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_claimed_request_that_outlives_the_grace_has_unknown_completion() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let (queued_tx, queued_rx) = std::sync::mpsc::channel();
        let (start_wait_tx, start_wait_rx) = std::sync::mpsc::channel();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let (primary_timeout_tx, primary_timeout_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (drain_done_tx, drain_done_rx) = std::sync::mpsc::channel();
        let (answer_tx, answer_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = host.call_with_timeouts_and_hooks(
                ControlCall::Trusted(unit_request()),
                Duration::from_millis(20),
                Duration::from_millis(40),
                never_cancelled(),
                || {
                    queued_tx.send(()).unwrap();
                    start_wait_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                },
                || primary_timeout_tx.send(()).unwrap(),
            );
            answer_tx.send(result).unwrap();
        });
        queued_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("request enqueued");
        let drain = std::thread::spawn(move || {
            queue.drain_with(|_request| {
                claimed_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(ControlResponse::Unit)
            });
            drain_done_tx.send(()).unwrap();
        });
        claimed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drain claimed before first timeout");
        start_wait_tx.send(()).unwrap();
        primary_timeout_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("caller reached the claimed grace wait");
        let result = answer_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("claimed request returned after grace expired");
        release_tx.send(()).unwrap();
        drain_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drain finished after release");
        drain.join().unwrap();
        worker.join().unwrap();
        let error = result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.completion(), Some(MutationCompletion::Unknown));
    }

    #[test]
    fn app_dispatch_errors_cross_the_queue_as_execution_errors() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        let worker = std::thread::spawn(move || host.call(unit_request()));
        let mut served = false;
        while !served {
            queue.drain_with(|_request| {
                served = true;
                Err(Error::execution("pane closed"))
            });
            std::thread::yield_now();
        }
        let error = worker.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Execution);
        assert_eq!(error.to_string(), "pane closed");
    }

    #[test]
    fn a_disconnected_window_is_an_unavailable_error() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 8);
        drop(queue);
        let error = host.call(unit_request()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.completion(), Some(MutationCompletion::NotStarted));
        assert_eq!(error.to_string(), "the DeLOG window is gone");
    }

    #[test]
    fn a_full_control_queue_fails_without_blocking() {
        let (host, queue) = AppControlHost::new(egui::Context::default(), 1);
        let first = host
            .call_with_timeout(unit_request(), Duration::ZERO)
            .unwrap_err();
        assert_eq!(first.completion(), Some(MutationCompletion::NotStarted));

        let (answer_tx, answer_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            answer_tx.send(host.call_with_timeout(unit_request(), Duration::from_secs(1)))
        });
        let answer = answer_rx.recv_timeout(Duration::from_millis(250));
        drop(queue);
        worker.join().unwrap().unwrap();
        let error = answer
            .expect("full queue must reject promptly")
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.completion(), Some(MutationCompletion::NotStarted));
    }
}

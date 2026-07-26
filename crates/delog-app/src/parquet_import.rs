use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use delog_core::parse_ctl::ParseCtl;
use delog_parsers::{
    TimestampSelection, TimestampSelectionError, TimestampSelectionProvider,
    TimestampSelectionRequest, TimestampUnit,
};

struct PendingRequest {
    request: TimestampSelectionRequest,
    response: mpsc::SyncSender<Option<TimestampSelection>>,
    cancelled: Arc<AtomicBool>,
}

struct ChannelTimestampSelectionProvider {
    requests: mpsc::Sender<PendingRequest>,
}

impl TimestampSelectionProvider for ChannelTimestampSelectionProvider {
    fn select(
        &self,
        request: TimestampSelectionRequest,
        ctl: &ParseCtl,
    ) -> Result<TimestampSelection, TimestampSelectionError> {
        let (response, replies) = mpsc::sync_channel(0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let pending = PendingRequest {
            request,
            response,
            cancelled: Arc::clone(&cancelled),
        };

        if self.requests.send(pending).is_err() {
            return Err(TimestampSelectionError::Cancelled);
        }

        loop {
            if ctl.is_cancelled() {
                cancelled.store(true, Ordering::Relaxed);
                return Err(TimestampSelectionError::Cancelled);
            }

            match replies.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(selection)) => return Ok(selection),
                Ok(None) => return Err(TimestampSelectionError::Cancelled),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    cancelled.store(true, Ordering::Relaxed);
                    return Err(TimestampSelectionError::Cancelled);
                }
            }
        }
    }
}

struct DialogRequest {
    state: DialogState,
    response: mpsc::SyncSender<Option<TimestampSelection>>,
    cancelled: Arc<AtomicBool>,
}

struct DialogState {
    request: TimestampSelectionRequest,
    selected_candidate: Option<usize>,
    numeric_unit: Option<TimestampUnit>,
}

impl DialogState {
    fn new(request: TimestampSelectionRequest) -> Self {
        Self {
            request,
            selected_candidate: None,
            numeric_unit: None,
        }
    }

    fn select_column(&mut self, candidate_index: usize) {
        self.selected_candidate = Some(candidate_index);
    }

    fn select_unit(&mut self, unit: TimestampUnit) {
        if !self.unit_is_locked() {
            self.numeric_unit = Some(unit);
        }
    }

    fn selected_candidate(&self) -> Option<&delog_parsers::TimestampCandidate> {
        self.selected_candidate
            .and_then(|index| self.request.candidates.get(index))
    }

    fn resolved_unit(&self) -> Option<TimestampUnit> {
        self.selected_candidate()
            .and_then(|candidate| candidate.logical_unit)
            .or(self.numeric_unit)
    }

    fn unit_is_locked(&self) -> bool {
        self.selected_candidate()
            .is_some_and(|candidate| candidate.logical_unit.is_some())
    }

    fn can_import(&self) -> bool {
        self.selected_candidate().is_some() && self.resolved_unit().is_some()
    }

    fn import_response(&self) -> Option<TimestampSelection> {
        Some(TimestampSelection {
            column_index: self.selected_candidate()?.column_index,
            unit: self.resolved_unit()?,
        })
    }

    fn cancel_response(&self) -> Option<TimestampSelection> {
        None
    }

    fn close_response(&self) -> Option<TimestampSelection> {
        None
    }
}

pub struct ParquetImportUi {
    incoming: mpsc::Receiver<PendingRequest>,
    queued: VecDeque<PendingRequest>,
    active: Option<DialogRequest>,
}

impl ParquetImportUi {
    pub fn new() -> (Self, Arc<dyn TimestampSelectionProvider>) {
        let (requests, incoming) = mpsc::channel();
        (
            Self {
                incoming,
                queued: VecDeque::new(),
                active: None,
            },
            Arc::new(ChannelTimestampSelectionProvider { requests }),
        )
    }

    fn poll_requests(&mut self) {
        self.queued.extend(self.incoming.try_iter());

        if self
            .active
            .as_ref()
            .is_some_and(|active| active.cancelled.load(Ordering::Relaxed))
        {
            self.active = None;
        }
        self.queued
            .retain(|pending| !pending.cancelled.load(Ordering::Relaxed));
        self.promote_next();
    }

    fn promote_next(&mut self) {
        while self.active.is_none() {
            let Some(pending) = self.queued.pop_front() else {
                return;
            };
            if !pending.cancelled.load(Ordering::Relaxed) {
                self.active = Some(DialogRequest {
                    state: DialogState::new(pending.request),
                    response: pending.response,
                    cancelled: pending.cancelled,
                });
            }
        }
    }

    #[cfg(test)]
    fn active_request_id(&self) -> Option<u64> {
        self.active
            .as_ref()
            .map(|active| active.state.request.request_id)
    }

    fn answer(&mut self, selection: TimestampSelection) {
        self.respond_active(Some(selection));
    }

    fn cancel_active(&mut self) {
        self.respond_active(None);
    }

    fn respond_active(&mut self, response: Option<TimestampSelection>) {
        if let Some(active) = self.active.take() {
            let _ = active.response.send(response);
        }
        self.promote_next();
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll_requests();

        let Some(active) = self.active.as_mut() else {
            return;
        };
        let request_id = active.state.request.request_id;
        let mut open = true;
        let mut response = None;

        egui::Window::new("Import Parquet")
            .id(egui::Id::new(("parquet-import", request_id)))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let state = &mut active.state;
                ui.label(format!(
                    "Select a timestamp column for {}.",
                    state.request.file_label
                ));

                let selected_text = state
                    .selected_candidate()
                    .map(|candidate| format!("{} ({:?})", candidate.name, candidate.data_type))
                    .unwrap_or_else(|| "Select timestamp column".to_owned());
                egui::ComboBox::from_id_salt(("parquet-timestamp-column", request_id))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        let mut selected_candidate = None;
                        for (index, candidate) in state.request.candidates.iter().enumerate() {
                            let label = format!("{} ({:?})", candidate.name, candidate.data_type);
                            if ui
                                .selectable_label(state.selected_candidate == Some(index), label)
                                .clicked()
                            {
                                selected_candidate = Some(index);
                            }
                        }
                        if let Some(candidate_index) = selected_candidate {
                            state.select_column(candidate_index);
                        }
                    });

                ui.separator();
                ui.label("Timestamp unit");
                let locked = state.unit_is_locked();
                let mut selected_unit = state.resolved_unit();
                ui.add_enabled_ui(!locked, |ui| {
                    for (label, unit) in [
                        ("Seconds", TimestampUnit::Seconds),
                        ("Milliseconds", TimestampUnit::Milliseconds),
                        ("Microseconds", TimestampUnit::Microseconds),
                        ("Nanoseconds", TimestampUnit::Nanoseconds),
                    ] {
                        ui.selectable_value(&mut selected_unit, Some(unit), label);
                    }
                });
                if !locked && let Some(unit) = selected_unit {
                    state.select_unit(unit);
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(state.can_import(), egui::Button::new("Import"))
                        .clicked()
                    {
                        response = Some(state.import_response());
                    }
                    if ui.button("Cancel").clicked() {
                        response = Some(state.cancel_response());
                    }
                });
            });

        if !open && response.is_none() {
            response = Some(
                self.active
                    .as_ref()
                    .expect("active request exists while its window is shown")
                    .state
                    .close_response(),
            );
        }
        if let Some(response) = response {
            match response {
                Some(selection) => self.answer(selection),
                None => self.cancel_active(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use super::*;
    use arrow::datatypes::{DataType, TimeUnit};
    use delog_core::identity::SourceId;
    use delog_core::parse_ctl::{CancelToken, ParseCtl};
    use delog_parsers::{
        TimestampCandidate, TimestampSelection, TimestampSelectionError,
        TimestampSelectionProvider, TimestampSelectionRequest, TimestampUnit,
    };

    fn numeric_candidate(name: &str) -> TimestampCandidate {
        TimestampCandidate {
            column_index: 0,
            name: name.to_owned(),
            data_type: DataType::Int64,
            logical_unit: None,
        }
    }

    fn logical_candidate(name: &str, unit: TimestampUnit) -> TimestampCandidate {
        let time_unit = match unit {
            TimestampUnit::Seconds => TimeUnit::Second,
            TimestampUnit::Milliseconds => TimeUnit::Millisecond,
            TimestampUnit::Microseconds => TimeUnit::Microsecond,
            TimestampUnit::Nanoseconds => TimeUnit::Nanosecond,
        };
        TimestampCandidate {
            column_index: 0,
            name: name.to_owned(),
            data_type: DataType::Timestamp(time_unit, None),
            logical_unit: Some(unit),
        }
    }

    fn request(request_id: u64, candidate: TimestampCandidate) -> TimestampSelectionRequest {
        TimestampSelectionRequest {
            request_id,
            file_label: "flight.parquet".to_owned(),
            candidates: vec![candidate],
        }
    }

    fn spawn_request(
        provider: Arc<dyn TimestampSelectionProvider>,
        request: TimestampSelectionRequest,
    ) -> JoinHandle<Result<TimestampSelection, TimestampSelectionError>> {
        std::thread::spawn(move || {
            let ctl = ParseCtl::new(CancelToken::new(), SourceId(1), 0);
            provider.select(request, &ctl)
        })
    }

    fn wait_for_active(ui: &mut ParquetImportUi, request_id: u64) {
        for _ in 0..1_000 {
            ui.poll_requests();
            if ui.active_request_id() == Some(request_id) {
                return;
            }
            std::thread::yield_now();
        }
        panic!("request {request_id} did not become active");
    }

    #[test]
    fn provider_delivers_request_and_correlates_response() {
        let (mut ui, provider) = ParquetImportUi::new();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(1), 0).with_label("generic");
        let handle = std::thread::spawn(move || {
            provider.select(request(41, numeric_candidate("clock")), &ctl)
        });
        wait_for_active(&mut ui, 41);
        assert_eq!(ui.active_request_id(), Some(41));
        ui.answer(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Milliseconds,
        });
        assert_eq!(
            handle.join().unwrap().unwrap().unit,
            TimestampUnit::Milliseconds
        );
    }

    #[test]
    fn cancelled_waiter_is_removed_without_blocking_the_next_request() {
        let (mut ui, provider) = ParquetImportUi::new();
        let token = CancelToken::new();
        let ctl = ParseCtl::new(token.clone(), SourceId(1), 0);
        let handle =
            std::thread::spawn(move || provider.select(request(1, numeric_candidate("t")), &ctl));
        wait_for_active(&mut ui, 1);
        token.cancel();
        assert_eq!(
            handle.join().unwrap(),
            Err(TimestampSelectionError::Cancelled)
        );
        ui.poll_requests();
        assert_eq!(ui.active_request_id(), None);
    }

    #[test]
    fn requests_are_presented_one_at_a_time_in_arrival_order() {
        let (mut ui, provider) = ParquetImportUi::new();
        let first = spawn_request(Arc::clone(&provider), request(1, numeric_candidate("a")));
        wait_for_active(&mut ui, 1);
        let second = spawn_request(provider, request(2, numeric_candidate("b")));
        ui.poll_requests();
        assert_eq!(ui.active_request_id(), Some(1));
        ui.cancel_active();
        wait_for_active(&mut ui, 2);
        assert_eq!(ui.active_request_id(), Some(2));
        ui.cancel_active();
        assert_eq!(
            first.join().unwrap(),
            Err(TimestampSelectionError::Cancelled)
        );
        assert_eq!(
            second.join().unwrap(),
            Err(TimestampSelectionError::Cancelled)
        );
    }

    #[test]
    fn numeric_selection_requires_an_explicit_unit() {
        let mut state = DialogState::new(request(7, numeric_candidate("clock")));
        state.select_column(0);
        assert!(!state.can_import());
        state.select_unit(TimestampUnit::Microseconds);
        assert!(state.can_import());
    }

    #[test]
    fn logical_timestamp_locks_its_unit() {
        let mut state = DialogState::new(request(
            8,
            logical_candidate("stamp", TimestampUnit::Nanoseconds),
        ));
        state.select_column(0);
        assert_eq!(state.resolved_unit(), Some(TimestampUnit::Nanoseconds));
        assert!(state.unit_is_locked());
        assert!(state.can_import());
    }

    #[test]
    fn cancelling_or_closing_sends_no_selection() {
        let mut state = DialogState::new(request(9, numeric_candidate("clock")));
        state.select_column(0);
        state.select_unit(TimestampUnit::Seconds);
        assert_eq!(state.cancel_response(), None);
        assert_eq!(state.close_response(), None);
    }
}

use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use delog_core::analysis::{FieldStats, visible_field_stats};
use delog_core::field_view::FieldViewError;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const LRU_CAPACITY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatsTab {
    #[default]
    Visible,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatsRequestKey {
    pub field: FieldId,
    pub epoch: u64,
    pub t0_us: i64,
    pub t1_us: i64,
}

impl StatsRequestKey {
    pub fn new(field: FieldId, epoch: u64, t0_us: i64, t1_us: i64) -> Self {
        Self {
            field,
            epoch,
            t0_us,
            t1_us,
        }
    }
}

type WorkerResult = (StatsRequestKey, Result<Option<FieldStats>, FieldViewError>);

pub struct FieldStatsController {
    selected: Option<FieldId>,
    tab: StatsTab,
    current: Option<StatsRequestKey>,
    running: Option<StatsRequestKey>,
    pending: Option<(StatsRequestKey, Arc<StoreSnapshot>)>,
    displayed: Option<(StatsRequestKey, FieldStats)>,
    error: Option<String>,
    recent: VecDeque<(StatsRequestKey, FieldStats)>,
    tx: mpsc::Sender<WorkerResult>,
    rx: mpsc::Receiver<WorkerResult>,
    last_launch: Option<Instant>,
}

impl Default for FieldStatsController {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            selected: None,
            tab: StatsTab::Visible,
            current: None,
            running: None,
            pending: None,
            displayed: None,
            error: None,
            recent: VecDeque::new(),
            tx,
            rx,
            last_launch: None,
        }
    }
}

impl FieldStatsController {
    pub fn open(&mut self, field: FieldId) {
        self.selected = Some(field);
        self.tab = StatsTab::Visible;
        self.current = None;
        self.displayed = None;
        self.error = None;
    }

    pub fn close(&mut self) {
        self.selected = None;
        self.current = None;
        self.pending = None;
        self.displayed = None;
        self.error = None;
    }

    pub fn selected(&self) -> Option<FieldId> {
        self.selected
    }

    pub fn tab(&self) -> StatsTab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: StatsTab) {
        self.tab = tab;
    }

    pub fn request(&mut self, key: StatsRequestKey, snapshot: Arc<StoreSnapshot>, now: Instant) {
        if self.current == Some(key) {
            self.poll(now);
            return;
        }
        self.current = Some(key);
        self.error = None;
        if let Some(index) = self.recent.iter().position(|(cached, _)| *cached == key) {
            let (_, stats) = self
                .recent
                .remove(index)
                .expect("index came from the deque");
            self.recent.push_back((key, stats));
            self.displayed = Some((key, stats));
            self.pending = None;
            return;
        }
        self.queue(key, snapshot);
        self.maybe_launch(now);
    }

    pub fn poll(&mut self, now: Instant) {
        while let Ok((key, result)) = self.rx.try_recv() {
            if self.running == Some(key) {
                self.running = None;
            }
            match result {
                Ok(Some(stats)) => self.accept(key, stats),
                Ok(None) => {
                    if self.current == Some(key) {
                        self.error = Some("This field is not numeric.".into());
                    }
                }
                Err(err) => {
                    if self.current == Some(key) {
                        self.error = Some(err.to_string());
                    }
                }
            }
        }
        self.maybe_launch(now);
    }

    pub fn result(&self) -> Option<&FieldStats> {
        let (key, stats) = self.displayed.as_ref()?;
        (Some(*key) == self.current).then_some(stats)
    }

    pub fn stale_result(&self) -> Option<&FieldStats> {
        self.displayed.as_ref().map(|(_, stats)| stats)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_updating(&self) -> bool {
        self.current.is_some()
            && self.result().is_none()
            && (self.running.is_some() || self.pending.is_some())
    }

    fn queue(&mut self, key: StatsRequestKey, snapshot: Arc<StoreSnapshot>) {
        self.pending = Some((key, snapshot));
    }

    fn maybe_launch(&mut self, now: Instant) {
        if self.running.is_some() || self.selected.is_none() {
            return;
        }
        if self
            .last_launch
            .is_some_and(|last| now.saturating_duration_since(last) < REFRESH_INTERVAL)
        {
            return;
        }
        let Some((key, snapshot)) = self.pending.take() else {
            return;
        };
        self.running = Some(key);
        self.last_launch = Some(now);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = visible_field_stats(&snapshot, key.field, key.t0_us, key.t1_us);
            let _ = tx.send((key, result));
        });
    }

    fn accept(&mut self, key: StatsRequestKey, stats: FieldStats) {
        if let Some(index) = self.recent.iter().position(|(cached, _)| *cached == key) {
            self.recent.remove(index);
        }
        self.recent.push_back((key, stats));
        while self.recent.len() > LRU_CAPACITY {
            self.recent.pop_front();
        }
        if self.current == Some(key) {
            self.displayed = Some((key, stats));
            self.error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_on_visible_tab_and_coalesces_pending_requests() {
        let mut controller = FieldStatsController::default();
        let field = delog_core::identity::FieldId(4);
        controller.open(field);
        assert_eq!(controller.tab(), StatsTab::Visible);

        let a = StatsRequestKey::new(field, 1, 0, 10);
        let b = StatsRequestKey::new(field, 1, 10, 20);
        controller.running = Some(a);
        controller.queue(a, Arc::new(StoreSnapshot::empty()));
        controller.queue(b, Arc::new(StoreSnapshot::empty()));
        assert_eq!(controller.pending.as_ref().map(|(key, _)| *key), Some(b));
    }

    #[test]
    fn stale_results_never_replace_the_current_window() {
        let mut controller = FieldStatsController::default();
        let field = delog_core::identity::FieldId(2);
        let old = StatsRequestKey::new(field, 3, 0, 10);
        let current = StatsRequestKey::new(field, 4, 10, 20);
        controller.current = Some(current);
        controller.accept(old, test_stats(1.0));
        assert!(controller.result().is_none());
        controller.accept(current, test_stats(2.0));
        assert_eq!(controller.result().unwrap().min, 2.0);
    }

    #[test]
    fn recent_results_are_lru_bounded_and_close_discards_display_state() {
        let mut controller = FieldStatsController::default();
        let field = FieldId(1);
        for epoch in 0..=LRU_CAPACITY as u64 {
            controller.accept(
                StatsRequestKey::new(field, epoch, 0, 10),
                test_stats(epoch as f64),
            );
        }
        assert_eq!(controller.recent.len(), LRU_CAPACITY);
        assert!(controller.recent.iter().all(|(key, _)| key.epoch != 0));

        controller.current = Some(StatsRequestKey::new(field, 8, 0, 10));
        controller.displayed = Some((StatsRequestKey::new(field, 8, 0, 10), test_stats(8.0)));
        controller.pending = Some((
            StatsRequestKey::new(field, 9, 0, 10),
            Arc::new(StoreSnapshot::empty()),
        ));
        controller.close();
        assert!(controller.result().is_none());
        assert!(controller.pending.is_none());
    }

    #[test]
    fn launch_rate_is_capped_at_ten_hz() {
        let mut controller = FieldStatsController::default();
        let field = FieldId(1);
        controller.open(field);
        let key = StatsRequestKey::new(field, 1, 0, 10);
        controller.queue(key, Arc::new(StoreSnapshot::empty()));
        let now = Instant::now();
        controller.last_launch = Some(now);
        controller.maybe_launch(now + Duration::from_millis(99));
        assert!(controller.running.is_none());
        assert!(controller.pending.is_some());
        controller.maybe_launch(now + Duration::from_millis(100));
        assert_eq!(controller.running, Some(key));
        assert!(controller.pending.is_none());
    }

    fn test_stats(min: f64) -> delog_core::analysis::FieldStats {
        delog_core::analysis::FieldStats {
            min,
            max: min,
            mean: min,
            stddev: 0.0,
            count: 1,
            missing_count: 0,
            rate_hz: None,
        }
    }
}

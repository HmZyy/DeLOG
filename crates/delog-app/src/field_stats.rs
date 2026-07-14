use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use delog_core::analysis::{FieldStats, visible_field_stats};
use delog_core::field_view::FieldViewError;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const LRU_CAPACITY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StatsTab {
    #[default]
    Visible,
    Global,
}

impl StatsTab {
    pub const ALL: [Self; 2] = [Self::Visible, Self::Global];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Visible => "Visible window",
            Self::Global => "Global",
        }
    }
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
type WorkerBatch = (Vec<StatsRequestKey>, Vec<WorkerResult>);

pub struct FieldStatsController {
    fields: Vec<FieldId>,
    tab: StatsTab,
    current: Vec<StatsRequestKey>,
    running: Option<Vec<StatsRequestKey>>,
    pending: Option<(Vec<StatsRequestKey>, Arc<StoreSnapshot>)>,
    displayed: HashMap<FieldId, (StatsRequestKey, FieldStats)>,
    errors: HashMap<FieldId, String>,
    recent: VecDeque<(StatsRequestKey, FieldStats)>,
    tx: mpsc::Sender<WorkerBatch>,
    rx: mpsc::Receiver<WorkerBatch>,
    last_launch: Option<Instant>,
}

impl Default for FieldStatsController {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            fields: Vec::new(),
            tab: StatsTab::Visible,
            current: Vec::new(),
            running: None,
            pending: None,
            displayed: HashMap::new(),
            errors: HashMap::new(),
            recent: VecDeque::new(),
            tx,
            rx,
            last_launch: None,
        }
    }
}

impl FieldStatsController {
    pub fn open(&mut self, field: FieldId) {
        self.open_fields(vec![field]);
    }

    pub fn open_fields(&mut self, fields: Vec<FieldId>) {
        self.fields = fields;
        self.tab = StatsTab::Visible;
        self.current.clear();
        self.pending = None;
        self.displayed.clear();
        self.errors.clear();
    }

    pub fn close(&mut self) {
        self.fields.clear();
        self.current.clear();
        self.pending = None;
        self.displayed.clear();
        self.errors.clear();
    }

    pub fn selected(&self) -> Option<FieldId> {
        self.fields.first().copied()
    }

    pub fn fields(&self) -> &[FieldId] {
        &self.fields
    }

    fn current_key(&self, field: FieldId) -> Option<StatsRequestKey> {
        self.current.iter().copied().find(|key| key.field == field)
    }

    pub fn tab(&self) -> StatsTab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: StatsTab) {
        self.tab = tab;
    }

    pub fn request(&mut self, key: StatsRequestKey, snapshot: Arc<StoreSnapshot>, now: Instant) {
        self.request_keys(vec![key], snapshot, now);
    }

    pub fn request_all(
        &mut self,
        epoch: u64,
        t0_us: i64,
        t1_us: i64,
        snapshot: Arc<StoreSnapshot>,
        now: Instant,
    ) {
        let keys = self
            .fields
            .iter()
            .copied()
            .map(|field| StatsRequestKey::new(field, epoch, t0_us, t1_us))
            .collect();
        self.request_keys(keys, snapshot, now);
    }

    fn request_keys(
        &mut self,
        keys: Vec<StatsRequestKey>,
        snapshot: Arc<StoreSnapshot>,
        now: Instant,
    ) {
        if self.current == keys {
            self.poll(now);
            return;
        }
        self.current = keys.clone();
        self.errors.clear();
        let mut uncached = Vec::new();
        for key in keys {
            if let Some(index) = self.recent.iter().position(|(cached, _)| *cached == key) {
                let (_, stats) = self
                    .recent
                    .remove(index)
                    .expect("index came from the deque");
                self.recent.push_back((key, stats));
                self.displayed.insert(key.field, (key, stats));
            } else {
                uncached.push(key);
            }
        }
        self.pending = (!uncached.is_empty()).then_some((uncached, snapshot));
        self.maybe_launch(now);
    }

    pub fn poll(&mut self, now: Instant) {
        while let Ok((keys, results)) = self.rx.try_recv() {
            if self.running.as_ref() == Some(&keys) {
                self.running = None;
            }
            for (key, result) in results {
                match result {
                    Ok(Some(stats)) => self.accept(key, stats),
                    Ok(None) => {
                        if self.current_key(key.field) == Some(key) {
                            self.errors
                                .insert(key.field, "This field is not numeric.".into());
                        }
                    }
                    Err(err) => {
                        if self.current_key(key.field) == Some(key) {
                            self.errors.insert(key.field, err.to_string());
                        }
                    }
                }
            }
        }
        self.maybe_launch(now);
    }

    pub fn result(&self) -> Option<&FieldStats> {
        self.selected().and_then(|field| self.result_for(field))
    }

    pub fn result_for(&self, field: FieldId) -> Option<&FieldStats> {
        let (key, stats) = self.displayed.get(&field)?;
        (Some(*key) == self.current_key(field)).then_some(stats)
    }

    pub fn stale_result(&self) -> Option<&FieldStats> {
        self.selected()
            .and_then(|field| self.stale_result_for(field))
    }

    pub fn stale_result_for(&self, field: FieldId) -> Option<&FieldStats> {
        self.displayed.get(&field).map(|(_, stats)| stats)
    }

    pub fn error(&self) -> Option<&str> {
        self.selected().and_then(|field| self.error_for(field))
    }

    pub fn error_for(&self, field: FieldId) -> Option<&str> {
        self.errors.get(&field).map(String::as_str)
    }

    pub fn is_updating(&self) -> bool {
        self.selected()
            .is_some_and(|field| self.is_updating_for(field))
    }

    pub fn is_updating_for(&self, field: FieldId) -> bool {
        self.current_key(field).is_some()
            && self.result_for(field).is_none()
            && (self
                .running
                .as_ref()
                .is_some_and(|keys| keys.iter().any(|key| key.field == field))
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|(keys, _)| keys.iter().any(|key| key.field == field)))
    }

    pub fn is_any_updating(&self) -> bool {
        self.fields
            .iter()
            .copied()
            .any(|field| self.is_updating_for(field))
    }

    fn maybe_launch(&mut self, now: Instant) {
        if self.running.is_some() || self.fields.is_empty() {
            return;
        }
        if self
            .last_launch
            .is_some_and(|last| now.saturating_duration_since(last) < REFRESH_INTERVAL)
        {
            return;
        }
        let Some((keys, snapshot)) = self.pending.take() else {
            return;
        };
        self.running = Some(keys.clone());
        self.last_launch = Some(now);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let results = keys
                .iter()
                .copied()
                .map(|key| {
                    let result = visible_field_stats(&snapshot, key.field, key.t0_us, key.t1_us);
                    (key, result)
                })
                .collect();
            let _ = tx.send((keys, results));
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
        if self.current_key(key.field) == Some(key) {
            self.displayed.insert(key.field, (key, stats));
            self.errors.remove(&key.field);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_captures_fields_in_order_and_resets_to_visible_tab() {
        let mut controller = FieldStatsController::default();
        let fields = vec![FieldId(4), FieldId(2)];

        controller.set_tab(StatsTab::Global);
        controller.open_fields(fields.clone());

        assert_eq!(controller.fields(), fields.as_slice());
        assert_eq!(controller.tab(), StatsTab::Visible);
    }

    #[test]
    fn a_new_range_coalesces_all_pending_fields_into_one_batch() {
        let mut controller = FieldStatsController::default();
        let fields = vec![FieldId(1), FieldId(2)];
        controller.open_fields(fields.clone());
        controller.running = Some(vec![
            StatsRequestKey::new(fields[0], 1, 0, 10),
            StatsRequestKey::new(fields[1], 1, 0, 10),
        ]);

        controller.request_all(1, 10, 20, Arc::new(StoreSnapshot::empty()), Instant::now());

        let pending = &controller.pending.as_ref().unwrap().0;
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|key| key.t0_us == 10 && key.t1_us == 20));
    }

    #[test]
    fn stale_results_never_replace_current_results_for_either_field() {
        let mut controller = FieldStatsController::default();
        let first = FieldId(2);
        let second = FieldId(3);
        controller.open_fields(vec![first, second]);
        controller.current = vec![StatsRequestKey::new(first, 3, 0, 10)];
        controller.accept(StatsRequestKey::new(first, 3, 0, 10), test_stats(1.0));
        controller.current = vec![
            StatsRequestKey::new(first, 4, 10, 20),
            StatsRequestKey::new(second, 4, 10, 20),
        ];

        controller.accept(StatsRequestKey::new(second, 4, 10, 20), test_stats(2.0));

        assert!(controller.result_for(first).is_none());
        assert_eq!(controller.result_for(second).unwrap().min, 2.0);
        assert_eq!(controller.stale_result_for(first).unwrap().min, 1.0);
    }

    #[test]
    fn recent_results_are_lru_bounded() {
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
    }

    #[test]
    fn close_discards_all_captured_and_displayed_state() {
        let mut controller = FieldStatsController::default();
        let field = FieldId(1);
        controller.open_fields(vec![field]);
        controller.current = vec![StatsRequestKey::new(field, 8, 0, 10)];
        controller.displayed.insert(
            field,
            (StatsRequestKey::new(field, 8, 0, 10), test_stats(8.0)),
        );
        controller.pending = Some((
            vec![StatsRequestKey::new(field, 9, 0, 10)],
            Arc::new(StoreSnapshot::empty()),
        ));

        controller.close();

        assert!(controller.fields().is_empty());
        assert!(controller.result_for(field).is_none());
        assert!(controller.pending.is_none());
    }

    #[test]
    fn launch_rate_is_capped_at_ten_hz() {
        let mut controller = FieldStatsController::default();
        let field = FieldId(1);
        controller.open_fields(vec![field]);
        let key = StatsRequestKey::new(field, 1, 0, 10);
        controller.pending = Some((vec![key], Arc::new(StoreSnapshot::empty())));
        let now = Instant::now();
        controller.last_launch = Some(now);
        controller.maybe_launch(now + Duration::from_millis(99));
        assert!(controller.running.is_none());
        assert!(controller.pending.is_some());
        controller.maybe_launch(now + Duration::from_millis(100));
        assert_eq!(controller.running, Some(vec![key]));
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

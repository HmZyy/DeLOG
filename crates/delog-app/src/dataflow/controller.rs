use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use delog_core::derived::{
    PendingTopic, emit_prepared_topics, open_derived_source, prepare_topics, submit_prepared_topics,
};
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSender, IngestSink};
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::{GraphCommand, apply};
use delog_flow::eval::{Diagnostic, EvalCache, evaluate_windowed};
use delog_flow::graph::{Graph, NodeId, NodeKind};
use delog_flow::publish::{build_outputs, slice_topic_after, source_key};
use delog_flow::types::Value;

use crate::logging::LogLevel;

const UNDO_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewStats {
    pub count: u64,
    pub nan_count: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub stddev: f64,
    pub t0_us: i64,
    pub t1_us: i64,
}

/// Whole-session preview aggregate, merged from per-tick window stats via the
/// parallel variance-combine formula so live previews span the full session at
/// O(window) cost per tick.
#[derive(Debug, Clone, Copy)]
pub struct RunningStats {
    count: u64,
    nan_count: u64,
    min: f64,
    max: f64,
    mean: f64,
    m2: f64,
    t0_us: i64,
    t1_us: i64,
}

impl RunningStats {
    pub fn from_stats(s: PreviewStats) -> Self {
        Self {
            count: s.count,
            nan_count: s.nan_count,
            min: s.min,
            max: s.max,
            mean: if s.count == 0 { 0.0 } else { s.mean },
            m2: if s.count == 0 { 0.0 } else { s.stddev * s.stddev * s.count as f64 },
            t0_us: s.t0_us,
            t1_us: s.t1_us,
        }
    }

    pub fn merge(&mut self, other: PreviewStats) {
        self.nan_count += other.nan_count;
        if other.t0_us != 0 || other.t1_us != 0 {
            if self.count == 0 && self.t0_us == 0 && self.t1_us == 0 {
                self.t0_us = other.t0_us;
            }
            self.t1_us = other.t1_us;
        }
        if other.count == 0 {
            return;
        }
        let other_m2 = other.stddev * other.stddev * other.count as f64;
        if self.count == 0 {
            self.count = other.count;
            self.min = other.min;
            self.max = other.max;
            self.mean = other.mean;
            self.m2 = other_m2;
            return;
        }
        let n_a = self.count as f64;
        let n_b = other.count as f64;
        let n = n_a + n_b;
        let delta = other.mean - self.mean;
        self.mean += delta * n_b / n;
        self.m2 += other_m2 + delta * delta * n_a * n_b / n;
        self.count += other.count;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    pub fn as_preview(&self) -> PreviewStats {
        PreviewStats {
            count: self.count,
            nan_count: self.nan_count,
            min: self.min,
            max: self.max,
            mean: if self.count == 0 { f64::NAN } else { self.mean },
            stddev: if self.count == 0 {
                f64::NAN
            } else {
                (self.m2 / self.count as f64).sqrt()
            },
            t0_us: self.t0_us,
            t1_us: self.t1_us,
        }
    }
}

pub struct EvalOutcome {
    pub generation: u64,
    pub graph_name: String,
    pub previews: HashMap<NodeId, Vec<Option<PreviewStats>>>,
    pub preview_end_t: HashMap<(NodeId, usize), i64>,
    pub diagnostics: Vec<Diagnostic>,
    pub publish: Option<Result<Vec<PendingTopic>, Vec<Diagnostic>>>,
    pub live: bool,
    pub snapshot_max_t: Option<i64>,
}

struct PublicationOutcome {
    generation: u64,
    name: String,
    result: Result<Option<SourceId>, String>,
}

struct PendingRequest {
    generation: u64,
    graph: Graph,
    snapshot: Arc<StoreSnapshot>,
    selection: Option<NodeId>,
    publish: bool,
    window: Option<delog_core::time::TimeRange>,
    live: bool,
    preview_from_t: Option<i64>,
    snapshot_max_t: Option<i64>,
}

pub struct DataFlowController {
    pub graph: Graph,
    pub selection: Option<NodeId>,
    pub dirty: bool,
    undo: Vec<GraphCommand>,
    redo: Vec<GraphCommand>,
    generation: u64,
    latest_generation: Arc<AtomicU64>,
    cancel: Option<Arc<AtomicBool>>,
    tx: mpsc::Sender<EvalOutcome>,
    rx: mpsc::Receiver<EvalOutcome>,
    in_flight: bool,
    pending: Option<PendingRequest>,
    needs_eval: bool,
    last_outcome: Option<EvalOutcome>,
    published: Arc<Mutex<HashMap<String, SourceId>>>,
    publication_tx: mpsc::Sender<PublicationOutcome>,
    publication_rx: mpsc::Receiver<PublicationOutcome>,
    publications_in_flight: usize,
    cache: Arc<Mutex<EvalCache>>,
    live_watermark_t: Option<i64>,
    running_preview: HashMap<(NodeId, usize), RunningStats>,
    last_previewed_t: HashMap<(NodeId, usize), i64>,
    pending_preview_from: Option<i64>,
    pending_snapshot_max: Option<i64>,
    live_source: Option<SourceId>,
    last_published_t: HashMap<String, i64>,
    live_needs_reset: bool,
    #[cfg(feature = "scripting")]
    script_host: Option<Arc<dyn delog_flow::script::ScriptNodeHost + Sync>>,
}

impl DataFlowController {
    pub fn new(graph: Graph) -> Self {
        Self::with_publication_state(
            graph,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicU64::new(0)),
        )
    }

    fn with_publication_state(
        graph: Graph,
        published: Arc<Mutex<HashMap<String, SourceId>>>,
        latest_generation: Arc<AtomicU64>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (publication_tx, publication_rx) = mpsc::channel();
        Self {
            graph,
            selection: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            generation: 0,
            latest_generation,
            cancel: None,
            tx,
            rx,
            in_flight: false,
            pending: None,
            needs_eval: true,
            last_outcome: None,
            published,
            publication_tx,
            publication_rx,
            publications_in_flight: 0,
            cache: Arc::new(Mutex::new(EvalCache::default())),
            live_watermark_t: None,
            running_preview: HashMap::new(),
            last_previewed_t: HashMap::new(),
            pending_preview_from: None,
            pending_snapshot_max: None,
            live_source: None,
            last_published_t: HashMap::new(),
            live_needs_reset: false,
            #[cfg(feature = "scripting")]
            script_host: None,
        }
    }

    pub fn replace_graph(&mut self, graph: Graph) {
        let published = Arc::clone(&self.published);
        let latest_generation = Arc::clone(&self.latest_generation);
        #[cfg(feature = "scripting")]
        let script_host = self.script_host.clone();
        *self = Self::with_publication_state(graph, published, latest_generation);
        #[cfg(feature = "scripting")]
        {
            self.script_host = script_host;
        }
    }

    /// Sets (or clears) the host used to run `NodeKind::Script` nodes. The app
    /// layer refreshes this before each eval/publish request based on whether
    /// the graph currently contains a script node.
    #[cfg(feature = "scripting")]
    pub fn set_script_host(&mut self, host: Option<delog_script::flow::EngineFlowHost>) {
        self.script_host = host.map(|host| Arc::new(host) as Arc<dyn delog_flow::script::ScriptNodeHost + Sync>);
    }

    pub fn apply(&mut self, command: GraphCommand) -> Result<(), String> {
        let inverse = apply(&mut self.graph, command).map_err(|error| format!("{error:?}"))?;
        push_bounded(&mut self.undo, inverse);
        self.redo.clear();
        self.dirty = true;
        self.needs_eval = true;
        self.invalidate_live();
        if self
            .selection
            .is_some_and(|selection| self.graph.node(selection).is_none())
        {
            self.selection = None;
        }
        Ok(())
    }

    pub fn needs_eval(&self) -> bool {
        self.needs_eval
    }

    pub fn undo(&mut self) {
        let Some(command) = self.undo.pop() else {
            return;
        };
        if let Ok(inverse) = apply(&mut self.graph, command) {
            push_bounded(&mut self.redo, inverse);
            self.dirty = true;
            self.needs_eval = true;
            self.invalidate_live();
            if self
                .selection
                .is_some_and(|selection| self.graph.node(selection).is_none())
            {
                self.selection = None;
            }
        }
    }

    pub fn redo(&mut self) {
        let Some(command) = self.redo.pop() else {
            return;
        };
        if let Ok(inverse) = apply(&mut self.graph, command) {
            push_bounded(&mut self.undo, inverse);
            self.dirty = true;
            self.needs_eval = true;
            self.invalidate_live();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn request_eval(&mut self, snapshot: Arc<StoreSnapshot>) {
        self.queue(snapshot, false, None, false);
    }

    pub fn request_publish(&mut self, snapshot: Arc<StoreSnapshot>) {
        self.queue(snapshot, true, None, false);
    }

    pub fn request_live(&mut self, snapshot: Arc<StoreSnapshot>, overlap_secs: f32, append: bool) {
        let previous_watermark = self.live_watermark_t;
        let snapshot_max = snapshot.global_time_range().map(|range| range.max_us);
        // Do NOT advance the watermark here. If this generation is coalesced away
        // before it is merged, the watermark must stay put so the surviving
        // generation re-reads the missed tail. `poll` advances it only when a
        // live outcome actually survives.
        let window = previous_watermark.and_then(|watermark| {
            let max = snapshot_max?;
            let overlap_us = (overlap_secs.max(0.0) as f64 * 1_000_000.0) as i64;
            delog_core::time::TimeRange::new(watermark.saturating_sub(overlap_us), max)
        });
        self.pending_preview_from = previous_watermark;
        self.pending_snapshot_max = snapshot_max;
        self.queue(snapshot, append, window, true);
    }

    pub fn poll(&mut self, sender: &IngestSender) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        self.collect_publications(&mut logs);
        while let Ok(mut outcome) = self.rx.try_recv() {
            self.in_flight = false;
            self.cancel = None;
            if outcome.generation == self.generation {
                if outcome.live {
                    self.merge_live_previews(&outcome);
                    self.append_publish(&mut outcome, sender, &mut logs);
                    if let Some(max) = outcome.snapshot_max_t {
                        self.live_watermark_t = Some(max);
                    }
                } else {
                    self.handle_publish(&mut outcome, sender, &mut logs);
                }
                self.last_outcome = Some(outcome);
            }
            if let Some(request) = self.pending.take() {
                self.launch(request);
            }
        }
        self.collect_publications(&mut logs);
        logs
    }

    pub fn diagnostics_for(&self, node: NodeId) -> Vec<&str> {
        self.last_outcome
            .iter()
            .flat_map(|outcome| outcome.diagnostics.iter())
            .filter(|diagnostic| diagnostic.node == node)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect()
    }

    pub fn preview_for(&self, node: NodeId, port: usize) -> Option<PreviewStats> {
        if let Some(running) = self.running_preview.get(&(node, port)) {
            return Some(running.as_preview());
        }
        self.last_outcome
            .as_ref()?
            .previews
            .get(&node)?
            .get(port)?
            .as_ref()
            .copied()
    }

    pub fn is_evaluating(&self) -> bool {
        self.in_flight || self.pending.is_some() || self.publications_in_flight > 0
    }

    fn merge_live_previews(&mut self, outcome: &EvalOutcome) {
        for (&node, values) in &outcome.previews {
            for (port, stat) in values.iter().enumerate() {
                let Some(stat) = stat else { continue };
                let key = (node, port);
                let end = outcome.preview_end_t.get(&key).copied();
                let previously = self.last_previewed_t.get(&key).copied();
                // Only merge samples newer than the watermark for this port so the
                // recompute overlap is not double-counted. On the seed tick
                // (`previously` is None) the whole window is new.
                let is_new_tail = match (previously, end) {
                    (Some(prev), Some(end)) => end > prev,
                    _ => true,
                };
                if !is_new_tail {
                    continue;
                }
                self.running_preview
                    .entry(key)
                    .and_modify(|running| running.merge(*stat))
                    .or_insert_with(|| RunningStats::from_stats(*stat));
                if let Some(end) = end {
                    self.last_previewed_t.insert(key, end);
                }
            }
        }
    }

    fn queue(
        &mut self,
        snapshot: Arc<StoreSnapshot>,
        publish: bool,
        window: Option<delog_core::time::TimeRange>,
        live: bool,
    ) {
        self.generation = self
            .latest_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        self.latest_generation
            .store(self.generation, Ordering::Relaxed);
        self.needs_eval = false;
        let preview_from_t = if live { self.pending_preview_from } else { None };
        self.pending_preview_from = None;
        let snapshot_max_t = if live { self.pending_snapshot_max } else { None };
        self.pending_snapshot_max = None;
        let request = PendingRequest {
            generation: self.generation,
            graph: self.graph.clone(),
            snapshot,
            selection: self.selection,
            publish,
            window,
            live,
            preview_from_t,
            snapshot_max_t,
        };
        if self.in_flight {
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            self.pending = Some(request);
        } else {
            self.launch(request);
        }
    }

    fn launch(&mut self, request: PendingRequest) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.in_flight = true;
        let tx = self.tx.clone();
        let cache = Arc::clone(&self.cache);
        #[cfg(feature = "scripting")]
        let script_host = self.script_host.clone();
        std::thread::spawn(move || {
            let mut targets = request
                .graph
                .nodes
                .iter()
                .filter_map(|node| matches!(node.kind, NodeKind::Output(_)).then_some(node.id))
                .collect::<Vec<_>>();
            if let Some(selection) = request.selection
                && !targets.contains(&selection)
            {
                targets.push(selection);
            }
            let report = evaluate_windowed(
                &request.graph,
                &request.snapshot,
                &targets,
                &cancel,
                &mut cache.lock().unwrap(),
                request.window,
                #[cfg(feature = "scripting")]
                script_host
                    .as_deref()
                    .map(|host| host as &dyn delog_flow::script::ScriptNodeHost),
            );
            let previews = report
                .values
                .iter()
                .map(|(&node, values)| {
                    (
                        node,
                        values
                            .iter()
                            .map(|value| match value {
                                Value::Signal(signal) => {
                                    Some(preview_stats_from(signal, request.preview_from_t))
                                }
                                Value::Scalar(_) => None,
                            })
                            .collect::<Vec<Option<PreviewStats>>>(),
                    )
                })
                .collect();
            let mut preview_end_t = HashMap::new();
            for (&node, values) in &report.values {
                for (port, value) in values.iter().enumerate() {
                    if let Value::Signal(signal) = value {
                        if let Some(&last) = signal.t.last() {
                            preview_end_t.insert((node, port), last);
                        }
                    }
                }
            }
            let publish = request
                .publish
                .then(|| build_outputs(&request.graph, &report));
            let _ = tx.send(EvalOutcome {
                generation: request.generation,
                graph_name: request.graph.name,
                previews,
                preview_end_t,
                diagnostics: report.diagnostics,
                publish,
                live: request.live,
                snapshot_max_t: request.snapshot_max_t,
            });
        });
    }

    fn handle_publish(
        &mut self,
        outcome: &mut EvalOutcome,
        sender: &IngestSender,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        let Some(result) = outcome.publish.take() else {
            return;
        };
        let topics = match result {
            Ok(topics) => topics,
            Err(diagnostics) => {
                logs.push((
                    LogLevel::Error,
                    diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.message.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                ));
                return;
            }
        };
        let generation = outcome.generation;
        let name = outcome.graph_name.clone();
        let sender = sender.clone();
        let published = Arc::clone(&self.published);
        let latest_generation = Arc::clone(&self.latest_generation);
        let publication_tx = self.publication_tx.clone();
        self.publications_in_flight += 1;
        std::thread::spawn(move || {
            let result = prepare_topics(&topics).map(|prepared| {
                let mut published = published.lock().unwrap();
                if latest_generation.load(Ordering::Relaxed) != generation {
                    return None;
                }
                let mut sink = sender.file_sink();
                let source = emit_prepared_topics(&mut sink, &source_key(&name), prepared);
                if latest_generation.load(Ordering::Relaxed) != generation {
                    sender.remove_source(source);
                    return None;
                }
                if let Some(previous) = published.insert(name.clone(), source) {
                    sender.remove_source(previous);
                    sender.relabel_source(source, source_key(&name));
                }
                Some(source)
            });
            let _ = publication_tx.send(PublicationOutcome {
                generation,
                name,
                result,
            });
        });
    }

    pub fn is_live_published(&self) -> bool {
        self.live_source.is_some()
    }

    pub fn live_source(&self) -> Option<SourceId> {
        self.live_source
    }

    pub fn invalidate_live(&mut self) {
        if self.live_source.is_some() {
            self.live_needs_reset = true;
        }
    }

    pub fn take_needs_live_reset(&mut self) -> bool {
        std::mem::take(&mut self.live_needs_reset)
    }

    pub fn reset_live(&mut self, sender: &IngestSender) {
        if let Some(source) = self.live_source.take() {
            let mut sink = sender.file_sink();
            sink.close_source(source, delog_core::ingest::ParseSummary::default());
        }
        self.last_published_t.clear();
        self.live_watermark_t = None;
        self.running_preview.clear();
        self.last_previewed_t.clear();
    }

    fn append_publish(
        &mut self,
        outcome: &mut EvalOutcome,
        sender: &IngestSender,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        let Some(result) = outcome.publish.take() else {
            return;
        };
        let topics = match result {
            Ok(topics) => topics,
            Err(diagnostics) => {
                logs.push((
                    LogLevel::Error,
                    diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.message.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                ));
                return;
            }
        };
        let mut sink = sender.file_sink();
        let source = *self
            .live_source
            .get_or_insert_with(|| open_derived_source(&mut sink, &source_key(&outcome.graph_name)));

        let mut tails = Vec::new();
        for topic in &topics {
            let after = self
                .last_published_t
                .get(&topic.name)
                .copied()
                .unwrap_or(i64::MIN);
            let tail = slice_topic_after(topic, after);
            if let Some(&last) = tail.times.last() {
                self.last_published_t.insert(topic.name.clone(), last);
            }
            if !tail.times.is_empty() {
                tails.push(tail);
            }
        }
        if tails.is_empty() {
            return;
        }
        match prepare_topics(&tails) {
            Ok(prepared) => submit_prepared_topics(&mut sink, source, prepared),
            Err(message) => logs.push((LogLevel::Error, message)),
        }
    }

    fn collect_publications(&mut self, logs: &mut Vec<(LogLevel, String)>) {
        while let Ok(outcome) = self.publication_rx.try_recv() {
            self.publications_in_flight = self.publications_in_flight.saturating_sub(1);
            if outcome.generation != self.generation {
                continue;
            }
            match outcome.result {
                Ok(Some(_)) => logs.push((
                    LogLevel::Info,
                    format!("Published data flow '{}'.", outcome.name),
                )),
                Ok(None) => {}
                Err(message) => logs.push((LogLevel::Error, message)),
            }
        }
    }
}

/// Preview stats over the signal samples strictly newer than `from` (all samples
/// when `from` is `None`). The `None` case is the non-live path and is identical
/// to computing over the whole signal.
fn preview_stats_from(signal: &delog_flow::types::Signal, from: Option<i64>) -> PreviewStats {
    let mut count = 0_u64;
    let mut nan_count = 0_u64;
    let mut min = f64::NAN;
    let mut max = f64::NAN;
    let mut mean = 0.0;
    let mut m2 = 0.0;
    let mut t0 = None;
    let mut t1 = 0;
    for (i, &value) in signal.v.iter().enumerate() {
        let t = signal.t.get(i).copied().unwrap_or_default();
        if from.is_some_and(|from| t <= from) {
            continue;
        }
        t0.get_or_insert(t);
        t1 = t;
        if value.is_nan() {
            nan_count += 1;
            continue;
        }
        count += 1;
        min = if min.is_nan() { value } else { min.min(value) };
        max = if max.is_nan() { value } else { max.max(value) };
        let delta = value - mean;
        mean += delta / count as f64;
        m2 += delta * (value - mean);
    }
    PreviewStats {
        count,
        nan_count,
        min,
        max,
        mean: if count == 0 { f64::NAN } else { mean },
        stddev: if count == 0 {
            f64::NAN
        } else {
            (m2 / count as f64).sqrt()
        },
        t0_us: t0.unwrap_or_default(),
        t1_us: t1,
    }
}

fn push_bounded(stack: &mut Vec<GraphCommand>, command: GraphCommand) {
    if stack.len() == UNDO_CAPACITY {
        stack.remove(0);
    }
    stack.push(command);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    #[cfg(feature = "scripting")]
    use std::collections::VecDeque;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::derived::{PendingField, PendingTopic};
    use delog_core::identity::{IdentityRegistry, SourceId};
    use delog_core::ingest::{IngestMsg, RecvOutcome, SourceKind, ingest_channel};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;
    use delog_flow::command::GraphCommand;
    use delog_flow::graph::{
        FieldSelector, Graph, Node, NodeId, NodeKind, OutputFieldSpec, OutputSpec,
    };

    use super::*;

    fn snapshot() -> Arc<StoreSnapshot> {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "GPS").unwrap();
        identity.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![100, 200, 300]),
                vec![Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 1).unwrap())
    }

    fn snapshot_alt(times: Vec<i64>, values: Vec<f64>, epoch: u64) -> Arc<StoreSnapshot> {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "GPS").unwrap();
        identity.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(times),
                vec![Arc::new(Float64Array::from(values)) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], epoch).unwrap())
    }

    fn data() -> NodeKind {
        NodeKind::DataField(FieldSelector {
            source: Some("flight".into()),
            topic: "GPS".into(),
            instance: None,
            field: "Alt".into(),
        })
    }

    fn add_node(graph: &mut Graph, kind: NodeKind) -> NodeId {
        let id = graph.alloc_id();
        graph.insert_node(Node {
            id,
            pos: [0.0; 2],
            kind,
        });
        id
    }

    fn scale_graph(multiplier: f64) -> (Graph, NodeId) {
        let mut graph = Graph::new("g");
        let input = add_node(&mut graph, data());
        let scale = add_node(
            &mut graph,
            NodeKind::ScaleOffset {
                multiplier,
                offset: 0.0,
            },
        );
        graph.connect(input, 0, scale, 0).unwrap();
        (graph, scale)
    }

    fn wait_for(
        controller: &mut DataFlowController,
        sender: &delog_core::ingest::IngestSender,
    ) -> Vec<(LogLevel, String)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut logs = Vec::new();
        while controller.is_evaluating() && Instant::now() < deadline {
            logs.extend(controller.poll(sender));
            std::thread::sleep(Duration::from_millis(10));
        }
        logs.extend(controller.poll(sender));
        assert!(!controller.is_evaluating(), "worker timed out");
        logs
    }

    #[test]
    fn apply_undo_redo_track_dirty_and_stacks() {
        let mut controller = DataFlowController::new(Graph::new("g"));
        let id = controller.graph.alloc_id();
        controller
            .apply(GraphCommand::AddNode {
                node: Node {
                    id,
                    pos: [0.0; 2],
                    kind: NodeKind::Constant { value: 2.0 },
                },
            })
            .unwrap();
        assert!(controller.can_undo() && controller.dirty);
        controller.undo();
        assert!(controller.graph.node(id).is_none() && controller.can_redo());
        controller.redo();
        assert!(controller.graph.node(id).is_some() && controller.can_undo());
    }

    #[test]
    fn eval_outcome_carries_previews_and_diagnostics() {
        let (graph, scale) = scale_graph(2.0);
        let mut controller = DataFlowController::new(graph);
        controller.selection = Some(scale);
        let (sender, _receiver) = ingest_channel();
        controller.request_eval(snapshot());
        wait_for(&mut controller, &sender);
        let preview = controller.preview_for(scale, 0).unwrap();
        assert_eq!(preview.count, 3);
        assert_eq!(preview.mean, 4.0);
        assert!(controller.diagnostics_for(scale).is_empty());
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Observed {
        Open(String, SourceKind),
        Batch,
        Close(SourceId),
        Remove(SourceId),
    }

    #[test]
    fn poll_does_not_wait_for_publication_ingest_acknowledgement() {
        let mut controller = DataFlowController::new(Graph::new("g"));
        controller.generation = 1;
        controller.latest_generation.store(1, Ordering::Relaxed);
        let mut topic = PendingTopic::new("derived".into(), vec![100]);
        topic
            .add_field(PendingField::numeric("alt", vec![1.0], None))
            .unwrap();
        controller
            .tx
            .send(EvalOutcome {
                generation: 1,
                graph_name: "g".into(),
                previews: HashMap::new(),
                preview_end_t: HashMap::new(),
                diagnostics: Vec::new(),
                publish: Some(Ok(vec![topic])),
                live: false,
                snapshot_max_t: None,
            })
            .unwrap();
        let (sender, receiver) = ingest_channel();
        let (returned_tx, returned_rx) = mpsc::channel();

        let poll_thread = std::thread::spawn(move || {
            let logs = controller.poll(&sender);
            returned_tx.send(logs).unwrap();
            controller
        });
        let open = receiver.recv_timeout(Duration::from_secs(1));
        let RecvOutcome::Message(IngestMsg::OpenSource { reply, .. }) = open else {
            panic!("publication did not request a source: {open:?}");
        };
        let returned_before_ack = returned_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        reply.send(SourceId(40)).unwrap();
        let mut controller = poll_thread.join().unwrap();
        while let Some(message) = receiver.try_recv() {
            if matches!(message, IngestMsg::CloseSource { .. }) {
                break;
            }
        }
        let _ = controller.poll(&ingest_channel().0);

        assert!(
            returned_before_ack,
            "UI poll blocked on source acknowledgement"
        );
    }

    #[test]
    fn publication_becoming_stale_before_emission_has_no_ingest_side_effects() {
        let mut controller = DataFlowController::new(Graph::new("g"));
        controller.generation = 1;
        controller.latest_generation.store(1, Ordering::Relaxed);
        let mut topic = PendingTopic::new("derived".into(), vec![100]);
        topic
            .add_field(PendingField::numeric("alt", vec![1.0], None))
            .unwrap();
        controller
            .tx
            .send(EvalOutcome {
                generation: 1,
                graph_name: "g".into(),
                previews: HashMap::new(),
                preview_end_t: HashMap::new(),
                diagnostics: Vec::new(),
                publish: Some(Ok(vec![topic])),
                live: false,
                snapshot_max_t: None,
            })
            .unwrap();
        let (sender, receiver) = ingest_channel();
        let published = Arc::clone(&controller.published);
        let guard = published.lock().unwrap();

        controller.poll(&sender);
        controller.generation = 2;
        controller.latest_generation.store(2, Ordering::Relaxed);
        drop(guard);
        wait_for(&mut controller, &sender);

        assert!(receiver.try_recv().is_none());
        assert!(controller.published.lock().unwrap().is_empty());
    }

    #[test]
    fn publication_becoming_stale_while_open_waits_keeps_previous_source() {
        let mut controller = DataFlowController::new(Graph::new("g"));
        controller.generation = 1;
        controller.latest_generation.store(1, Ordering::Relaxed);
        controller
            .published
            .lock()
            .unwrap()
            .insert("g".into(), SourceId(39));
        let mut topic = PendingTopic::new("derived".into(), vec![100]);
        topic
            .add_field(PendingField::numeric("alt", vec![1.0], None))
            .unwrap();
        controller
            .tx
            .send(EvalOutcome {
                generation: 1,
                graph_name: "g".into(),
                previews: HashMap::new(),
                preview_end_t: HashMap::new(),
                diagnostics: Vec::new(),
                publish: Some(Ok(vec![topic])),
                live: false,
                snapshot_max_t: None,
            })
            .unwrap();
        let (sender, receiver) = ingest_channel();
        let (open_tx, open_rx) = mpsc::channel();
        let (ack_tx, ack_rx) = mpsc::channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let ingest_thread = std::thread::spawn(move || {
            while let Some(message) = receiver.recv() {
                let observed = match message {
                    IngestMsg::OpenSource { key, kind, reply } => {
                        open_tx.send(()).unwrap();
                        ack_rx.recv().unwrap();
                        reply.send(SourceId(40)).unwrap();
                        Observed::Open(key, kind)
                    }
                    IngestMsg::Batch(_) => Observed::Batch,
                    IngestMsg::CloseSource { source, .. } => Observed::Close(source),
                    IngestMsg::RemoveSource { source } => Observed::Remove(source),
                    _ => continue,
                };
                observed_tx.send(observed).unwrap();
            }
        });

        controller.poll(&sender);
        open_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        controller.generation = 2;
        controller.latest_generation.store(2, Ordering::Relaxed);
        ack_tx.send(()).unwrap();
        wait_for(&mut controller, &sender);
        let observed = (0..4)
            .map(|_| observed_rx.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect::<Vec<_>>();

        assert!(!observed.contains(&Observed::Remove(SourceId(39))));
        assert!(observed.contains(&Observed::Remove(SourceId(40))));
        assert_eq!(controller.published.lock().unwrap()["g"], SourceId(39));

        drop(sender);
        ingest_thread.join().unwrap();
    }

    #[test]
    fn publish_is_all_or_nothing_and_replaces_previous() {
        let mut graph = Graph::new("g");
        let input = add_node(&mut graph, data());
        let output = add_node(
            &mut graph,
            NodeKind::Output(OutputSpec {
                topic: "derived".into(),
                fields: vec![OutputFieldSpec {
                    name: "alt".into(),
                    unit: None,
                }],
            }),
        );
        graph.connect(input, 0, output, 0).unwrap();
        let mut controller = DataFlowController::new(graph);
        let (sender, receiver) = ingest_channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let ingest_thread = std::thread::spawn(move || {
            let mut next_id = 40;
            while let Some(message) = receiver.recv() {
                let observed = match message {
                    IngestMsg::OpenSource { key, kind, reply } => {
                        let id = SourceId(next_id);
                        next_id += 1;
                        reply.send(id).unwrap();
                        Observed::Open(key, kind)
                    }
                    IngestMsg::Batch(_) => Observed::Batch,
                    IngestMsg::CloseSource { source, .. } => Observed::Close(source),
                    IngestMsg::RemoveSource { source } => Observed::Remove(source),
                    _ => continue,
                };
                observed_tx.send(observed).unwrap();
            }
        });

        controller.request_publish(snapshot());
        let logs = wait_for(&mut controller, &sender);
        assert!(logs.iter().any(|(level, _)| *level == LogLevel::Info));
        assert_eq!(
            (0..3)
                .map(|_| observed_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                .collect::<Vec<_>>(),
            vec![
                Observed::Open("dataflow:g".into(), SourceKind::Derived),
                Observed::Batch,
                Observed::Close(SourceId(40)),
            ]
        );
        let first_id = controller.published.lock().unwrap()["g"];

        controller.request_publish(snapshot());
        wait_for(&mut controller, &sender);
        assert_eq!(
            (0..4)
                .map(|_| observed_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                .collect::<Vec<_>>(),
            vec![
                Observed::Open("dataflow:g".into(), SourceKind::Derived),
                Observed::Batch,
                Observed::Close(SourceId(41)),
                Observed::Remove(first_id),
            ]
        );
        let second_id = controller.published.lock().unwrap()["g"];

        controller
            .apply(GraphCommand::SetKind {
                id: output,
                kind: NodeKind::Output(OutputSpec {
                    topic: "derived".into(),
                    fields: vec![
                        OutputFieldSpec {
                            name: "same".into(),
                            unit: None,
                        },
                        OutputFieldSpec {
                            name: "same".into(),
                            unit: None,
                        },
                    ],
                }),
            })
            .unwrap();
        controller.request_publish(snapshot());
        let logs = wait_for(&mut controller, &sender);
        assert!(logs.iter().any(|(level, _)| *level == LogLevel::Error));
        assert!(observed_rx.try_recv().is_err());
        assert_eq!(controller.published.lock().unwrap()["g"], second_id);

        drop(sender);
        ingest_thread.join().unwrap();
    }

    #[test]
    fn graph_replacement_preserves_published_source_ownership() {
        let mut graph = Graph::new("g");
        let input = add_node(&mut graph, data());
        let output = add_node(
            &mut graph,
            NodeKind::Output(OutputSpec {
                topic: "derived".into(),
                fields: vec![OutputFieldSpec {
                    name: "alt".into(),
                    unit: None,
                }],
            }),
        );
        graph.connect(input, 0, output, 0).unwrap();
        let mut controller = DataFlowController::new(graph.clone());
        let (sender, receiver) = ingest_channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let ingest_thread = std::thread::spawn(move || {
            let mut next_id = 40;
            while let Some(message) = receiver.recv() {
                let observed = match message {
                    IngestMsg::OpenSource { key, kind, reply } => {
                        let id = SourceId(next_id);
                        next_id += 1;
                        reply.send(id).unwrap();
                        Observed::Open(key, kind)
                    }
                    IngestMsg::Batch(_) => Observed::Batch,
                    IngestMsg::CloseSource { source, .. } => Observed::Close(source),
                    IngestMsg::RemoveSource { source } => Observed::Remove(source),
                    _ => continue,
                };
                observed_tx.send(observed).unwrap();
            }
        });

        controller.request_publish(snapshot());
        wait_for(&mut controller, &sender);
        let first_id = controller.published.lock().unwrap()["g"];
        for _ in 0..3 {
            observed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }

        controller.replace_graph(graph);
        controller.request_publish(snapshot());
        wait_for(&mut controller, &sender);

        assert_eq!(
            (0..4)
                .map(|_| observed_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                .collect::<Vec<_>>(),
            vec![
                Observed::Open("dataflow:g".into(), SourceKind::Derived),
                Observed::Batch,
                Observed::Close(SourceId(41)),
                Observed::Remove(first_id),
            ]
        );
        assert_eq!(controller.published.lock().unwrap().len(), 1);
        assert_eq!(controller.published.lock().unwrap()["g"], SourceId(41));

        drop(sender);
        ingest_thread.join().unwrap();
    }

    #[cfg(feature = "scripting")]
    struct FakeScriptHost {
        responses: Mutex<VecDeque<Result<Vec<delog_flow::script::ScriptOutput>, String>>>,
    }

    #[cfg(feature = "scripting")]
    impl FakeScriptHost {
        fn new(responses: Vec<Result<Vec<delog_flow::script::ScriptOutput>, String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[cfg(feature = "scripting")]
    impl delog_flow::script::ScriptNodeHost for FakeScriptHost {
        fn eval(
            &self,
            _request: delog_flow::script::ScriptRequest,
            _cancel: &AtomicBool,
        ) -> Result<Vec<delog_flow::script::ScriptOutput>, String> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("fake host ran out of scripted responses")
        }
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_node_preview_arrives_per_output_port() {
        use delog_flow::script::{ScriptOutput, ScriptOutputSpec, ScriptSpec};

        let mut graph = Graph::new("g");
        let node = add_node(
            &mut graph,
            NodeKind::Script(ScriptSpec {
                name: "Solo".to_owned(),
                inputs: vec![],
                outputs: vec![
                    ScriptOutputSpec {
                        name: "a".to_owned(),
                        unit: None,
                    },
                    ScriptOutputSpec {
                        name: "b".to_owned(),
                        unit: None,
                    },
                ],
                code: "def flow(inputs):\n    return {}\n".to_owned(),
            }),
        );
        let mut controller = DataFlowController::new(graph);
        controller.selection = Some(node);
        controller.script_host = Some(Arc::new(FakeScriptHost::new(vec![Ok(vec![
            ScriptOutput {
                times: Some(vec![1, 2, 3]),
                values: vec![1.0, 2.0, 3.0],
                unit: None,
            },
            ScriptOutput {
                times: Some(vec![1, 2, 3]),
                values: vec![4.0, 5.0, 6.0],
                unit: None,
            },
        ])])));
        let (sender, _receiver) = ingest_channel();
        controller.request_eval(snapshot());
        wait_for(&mut controller, &sender);

        assert_eq!(controller.preview_for(node, 0).unwrap().mean, 2.0);
        assert_eq!(controller.preview_for(node, 1).unwrap().mean, 5.0);
    }

    #[test]
    fn stale_generations_are_dropped() {
        let (graph, scale) = scale_graph(2.0);
        let mut controller = DataFlowController::new(graph);
        controller.selection = Some(scale);
        let (sender, _receiver) = ingest_channel();
        controller.request_eval(snapshot());
        controller
            .apply(GraphCommand::SetKind {
                id: scale,
                kind: NodeKind::ScaleOffset {
                    multiplier: 3.0,
                    offset: 0.0,
                },
            })
            .unwrap();
        controller.request_eval(snapshot());
        wait_for(&mut controller, &sender);
        assert_eq!(controller.preview_for(scale, 0).unwrap().mean, 6.0);
    }

    #[test]
    fn running_stats_merge_matches_single_pass() {
        // Combining stats of [1,2,3] then [4,5,6] equals stats of [1..=6].
        fn stats_of(values: &[f64], t0: i64, t1: i64) -> PreviewStats {
            let count = values.len() as u64;
            let mean = values.iter().sum::<f64>() / count as f64;
            let m2: f64 = values.iter().map(|v| (v - mean).powi(2)).sum();
            PreviewStats {
                count,
                nan_count: 0,
                min: values.iter().cloned().fold(f64::INFINITY, f64::min),
                max: values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                mean,
                stddev: (m2 / count as f64).sqrt(),
                t0_us: t0,
                t1_us: t1,
            }
        }

        let mut running = RunningStats::from_stats(stats_of(&[1.0, 2.0, 3.0], 10, 30));
        running.merge(stats_of(&[4.0, 5.0, 6.0], 40, 60));
        let merged = running.as_preview();
        let whole = stats_of(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 10, 60);

        assert_eq!(merged.count, 6);
        assert!((merged.mean - whole.mean).abs() < 1e-9);
        assert!((merged.stddev - whole.stddev).abs() < 1e-9);
        assert_eq!(merged.min, 1.0);
        assert_eq!(merged.max, 6.0);
        assert_eq!(merged.t0_us, 10);
        assert_eq!(merged.t1_us, 60);
    }

    #[test]
    fn live_preview_accumulates_across_ticks() {
        let mut graph = Graph::new("g");
        let field = add_node(&mut graph, data());
        let mut controller = DataFlowController::new(graph);
        controller.selection = Some(field);
        let (sender, _receiver) = ingest_channel();

        // Seed tick: full history [100,200,300] -> [1,2,3].
        controller.request_live(snapshot_alt(vec![100, 200, 300], vec![1.0, 2.0, 3.0], 1), 3.0, false);
        wait_for(&mut controller, &sender);
        assert_eq!(controller.preview_for(field, 0).unwrap().count, 3);

        // Append tick: new samples 400,500 -> [4,5]; overlap re-reads 300 but the
        // tail merge only adds t > watermark, so count becomes 5, not 6.
        controller.request_live(
            snapshot_alt(vec![100, 200, 300, 400, 500], vec![1.0, 2.0, 3.0, 4.0, 5.0], 2),
            3.0,
            false,
        );
        wait_for(&mut controller, &sender);
        let preview = controller.preview_for(field, 0).unwrap();
        assert_eq!(preview.count, 5);
        assert_eq!(preview.mean, 3.0);
    }

    #[test]
    fn live_preview_survives_coalesced_generation() {
        let mut graph = Graph::new("g");
        let field = add_node(&mut graph, data());
        let mut controller = DataFlowController::new(graph);
        controller.selection = Some(field);
        let (sender, _receiver) = ingest_channel();

        // First live tick (seed) launches and stays in_flight (we do NOT poll).
        controller.request_live(snapshot_alt(vec![100, 200, 300], vec![1.0, 2.0, 3.0], 1), 3.0, false);
        // Second tick arrives before the first is polled -> coalesces, cancelling gen 1.
        controller.request_live(
            snapshot_alt(vec![100, 200, 300, 400, 500], vec![1.0, 2.0, 3.0, 4.0, 5.0], 2),
            3.0,
            false,
        );
        wait_for(&mut controller, &sender);

        // With the fix, the dropped gen-1 never advanced the watermark, so the surviving
        // gen-2 re-read the full history: count 5, mean 3.0. (Pre-fix this was 2.)
        let preview = controller.preview_for(field, 0).unwrap();
        assert_eq!(preview.count, 5);
        assert_eq!(preview.mean, 3.0);
    }

    #[test]
    fn live_append_seeds_then_appends_only_new_tail() {
        let mut graph = Graph::new("g");
        let input = add_node(&mut graph, data());
        let out = add_node(
            &mut graph,
            NodeKind::Output(OutputSpec {
                topic: "derived".into(),
                fields: vec![OutputFieldSpec {
                    name: "alt".into(),
                    unit: None,
                }],
            }),
        );
        graph.connect(input, 0, out, 0).unwrap();
        let mut controller = DataFlowController::new(graph);
        let (sender, receiver) = ingest_channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let ingest_thread = std::thread::spawn(move || {
            let mut next = 40;
            while let Some(message) = receiver.recv() {
                let observed = match message {
                    IngestMsg::OpenSource { key, kind, reply } => {
                        let id = SourceId(next);
                        next += 1;
                        reply.send(id).unwrap();
                        Observed::Open(key, kind)
                    }
                    IngestMsg::Batch(batch) => {
                        // record row count via a Batch marker; count rows through summary elsewhere
                        let _ = batch;
                        Observed::Batch
                    }
                    IngestMsg::CloseSource { source, .. } => Observed::Close(source),
                    IngestMsg::RemoveSource { source } => Observed::Remove(source),
                    _ => continue,
                };
                observed_tx.send(observed).unwrap();
            }
        });

        // Seed.
        controller.request_live(snapshot_alt(vec![100, 200, 300], vec![1.0, 2.0, 3.0], 1), 3.0, true);
        wait_for(&mut controller, &sender);
        assert!(controller.is_live_published());
        assert_eq!(
            observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Observed::Open("dataflow:g".into(), SourceKind::Derived)
        );
        assert_eq!(
            observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Observed::Batch
        );

        // Append: new samples 400,500; must NOT re-open, must NOT close, one more batch.
        controller.request_live(
            snapshot_alt(vec![100, 200, 300, 400, 500], vec![1.0, 2.0, 3.0, 4.0, 5.0], 2),
            3.0,
            true,
        );
        wait_for(&mut controller, &sender);
        assert_eq!(
            observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Observed::Batch
        );
        assert!(observed_rx.try_recv().is_err(), "no second open, no close on append");

        drop(sender);
        ingest_thread.join().unwrap();
    }

    #[test]
    fn live_seed_after_preview_covers_full_history() {
        // Timestamps are spaced 10s apart so the 3s overlap window genuinely clips
        // earlier history: without the reset before seeding, the seed would omit the
        // pre-watermark samples. (Sub-microsecond spacing would let the 3s overlap
        // span everything and hide the bug.)
        #[derive(Debug, PartialEq, Eq)]
        enum Rec {
            Open,
            Batch(usize),
            Close,
            Remove,
        }
        let mut graph = Graph::new("g");
        let input = add_node(&mut graph, data());
        let out = add_node(
            &mut graph,
            NodeKind::Output(OutputSpec {
                topic: "derived".into(),
                fields: vec![OutputFieldSpec {
                    name: "alt".into(),
                    unit: None,
                }],
            }),
        );
        graph.connect(input, 0, out, 0).unwrap();
        let mut controller = DataFlowController::new(graph);
        let (sender, receiver) = ingest_channel();
        let (rec_tx, rec_rx) = mpsc::channel();
        let ingest_thread = std::thread::spawn(move || {
            let mut next = 40;
            while let Some(message) = receiver.recv() {
                let rec = match message {
                    IngestMsg::OpenSource { reply, .. } => {
                        reply.send(SourceId(next)).unwrap();
                        next += 1;
                        Rec::Open
                    }
                    IngestMsg::Batch(batch) => Rec::Batch(batch.rows()),
                    IngestMsg::CloseSource { .. } => Rec::Close,
                    IngestMsg::RemoveSource { .. } => Rec::Remove,
                    _ => continue,
                };
                rec_tx.send(rec).unwrap();
            }
        });

        // 1. Preview tick (append=false): advances the watermark to 30_000_000, no publish.
        controller.request_live(
            snapshot_alt(vec![10_000_000, 20_000_000, 30_000_000], vec![1.0, 2.0, 3.0], 1),
            3.0,
            false,
        );
        wait_for(&mut controller, &sender);

        // 2. Clear the preview-advanced watermark before seeding (the drive fix's mechanism).
        controller.reset_live(&sender);

        // 3. Seed publish (append=true) over the full history.
        controller.request_live(
            snapshot_alt(
                vec![10_000_000, 20_000_000, 30_000_000, 40_000_000, 50_000_000],
                vec![1.0, 2.0, 3.0, 4.0, 5.0],
                2,
            ),
            3.0,
            true,
        );
        wait_for(&mut controller, &sender);

        // The seed spans all 5 rows (full history), not a windowed tail.
        assert_eq!(rec_rx.recv_timeout(Duration::from_secs(1)).unwrap(), Rec::Open);
        assert_eq!(
            rec_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Rec::Batch(5)
        );
        assert!(
            rec_rx.try_recv().is_err(),
            "exactly one open and one full-history seed batch"
        );

        drop(sender);
        ingest_thread.join().unwrap();
    }

    #[test]
    fn live_source_getter_exposes_open_source() {
        let mut graph = Graph::new("g");
        let input = add_node(&mut graph, data());
        let out = add_node(
            &mut graph,
            NodeKind::Output(OutputSpec {
                topic: "derived".into(),
                fields: vec![OutputFieldSpec {
                    name: "alt".into(),
                    unit: None,
                }],
            }),
        );
        graph.connect(input, 0, out, 0).unwrap();
        let mut controller = DataFlowController::new(graph);
        let (sender, receiver) = ingest_channel();
        let ingest_thread = std::thread::spawn(move || {
            let mut next = 40;
            while let Some(message) = receiver.recv() {
                if let IngestMsg::OpenSource { reply, .. } = message {
                    reply.send(SourceId(next)).unwrap();
                    next += 1;
                }
            }
        });

        assert!(controller.live_source().is_none());
        controller.request_live(
            snapshot_alt(vec![100, 200, 300], vec![1.0, 2.0, 3.0], 1),
            3.0,
            true,
        );
        wait_for(&mut controller, &sender);
        assert!(controller.live_source().is_some());

        controller.reset_live(&sender);
        assert!(controller.live_source().is_none());

        drop(sender);
        ingest_thread.join().unwrap();
    }
}

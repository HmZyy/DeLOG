use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use delog_core::derived::{
    PendingTopic, emit_prepared_topics, open_derived_source, prepare_topics, submit_prepared_topics,
};
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSender, IngestSink, SourceKind};
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::{GraphCommand, apply};
use delog_flow::eval::{Diagnostic, EvalCache, evaluate_windowed};
use delog_flow::graph::{Edge, Graph, Node, NodeId, NodeKind};
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

/// Copied nodes and the edges between them, for in-editor duplication.
#[derive(Debug, Clone, Default)]
pub struct Clipboard {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Clipboard {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

struct PendingRequest {
    generation: u64,
    graph: Graph,
    snapshot: Arc<StoreSnapshot>,
    selection: HashSet<NodeId>,
    publish: bool,
    window: Option<delog_core::time::TimeRange>,
    live: bool,
    preview_from_t: Option<i64>,
    snapshot_max_t: Option<i64>,
}

pub struct DataFlowController {
    pub graph: Graph,
    pub selection: HashSet<NodeId>,
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
            selection: HashSet::new(),
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
        self.selection.retain(|id| self.graph.node(*id).is_some());
        Ok(())
    }

    pub fn needs_eval(&self) -> bool {
        self.needs_eval
    }

    /// Bind (or clear) a DataField's source for this session only. The choice
    /// is not persisted and is not an undoable graph edit, so it does not touch
    /// `dirty`/undo; it just re-evaluates and re-seeds any live publication off
    /// the newly chosen source.
    pub fn set_field_source(&mut self, node: NodeId, source: Option<String>) {
        if let Some(node) = self.graph.node_mut(node)
            && let NodeKind::DataField(selector) = &mut node.kind
            && selector.source != source
        {
            selector.source = source;
            self.needs_eval = true;
            self.invalidate_live();
        }
    }

    /// The single selected node, or `None` when zero or multiple are selected.
    pub fn sole_selection(&self) -> Option<NodeId> {
        if self.selection.len() == 1 {
            self.selection.iter().copied().next()
        } else {
            None
        }
    }

    pub fn copy_selection(&self) -> Clipboard {
        let nodes: Vec<Node> = self
            .graph
            .nodes
            .iter()
            .filter(|node| self.selection.contains(&node.id))
            .cloned()
            .collect();
        let ids: HashSet<NodeId> = nodes.iter().map(|node| node.id).collect();
        let edges: Vec<Edge> = self
            .graph
            .edges
            .iter()
            .filter(|edge| ids.contains(&edge.from) && ids.contains(&edge.to))
            .cloned()
            .collect();
        Clipboard { nodes, edges }
    }

    pub fn delete_selection(&mut self) -> Result<(), String> {
        if self.selection.is_empty() {
            return Ok(());
        }
        let commands = self
            .selection
            .iter()
            .map(|&id| GraphCommand::RemoveNode { id })
            .collect();
        self.selection.clear();
        self.apply(GraphCommand::Batch(commands))
    }

    /// Duplicates `clipboard` into the graph shifted by `offset`, remapping edges
    /// between copied nodes, as one undo step, and selects the new nodes.
    pub fn paste(&mut self, clipboard: &Clipboard, offset: [f32; 2]) -> Result<(), String> {
        if clipboard.is_empty() {
            return Ok(());
        }
        let mut id_map = HashMap::new();
        let mut commands = Vec::new();
        let mut pasted = HashSet::new();
        for node in &clipboard.nodes {
            let new_id = self.graph.alloc_id();
            id_map.insert(node.id, new_id);
            pasted.insert(new_id);
            commands.push(GraphCommand::AddNode {
                node: Node {
                    id: new_id,
                    pos: [node.pos[0] + offset[0], node.pos[1] + offset[1]],
                    kind: node.kind.clone(),
                },
            });
        }
        for edge in &clipboard.edges {
            if let (Some(&from), Some(&to)) = (id_map.get(&edge.from), id_map.get(&edge.to)) {
                commands.push(GraphCommand::Connect {
                    from,
                    from_port: edge.from_port,
                    to,
                    to_port: edge.to_port,
                });
            }
        }
        self.apply(GraphCommand::Batch(commands))?;
        self.selection = pasted;
        Ok(())
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
            self.selection.retain(|id| self.graph.node(*id).is_some());
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
            selection: self.selection.clone(),
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
            for &selection in &request.selection {
                if !targets.contains(&selection) {
                    targets.push(selection);
                }
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
        let source = *self.live_source.get_or_insert_with(|| {
            open_derived_source(
                &mut sink,
                &source_key(&outcome.graph_name),
                SourceKind::LiveDerived,
            )
        });

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
mod tests;

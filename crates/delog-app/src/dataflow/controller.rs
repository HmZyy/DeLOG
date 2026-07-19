use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use delog_core::derived::{PendingTopic, emit_prepared_topics, prepare_topics};
use delog_core::identity::SourceId;
use delog_core::ingest::IngestSender;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::{GraphCommand, apply};
use delog_flow::eval::{Diagnostic, EvalCache, evaluate};
use delog_flow::graph::{Graph, NodeId, NodeKind};
use delog_flow::publish::{build_outputs, source_key};
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

pub struct EvalOutcome {
    pub generation: u64,
    pub graph_name: String,
    pub previews: HashMap<NodeId, Vec<Option<PreviewStats>>>,
    pub diagnostics: Vec<Diagnostic>,
    pub publish: Option<Result<Vec<PendingTopic>, Vec<Diagnostic>>>,
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
        }
    }

    pub fn replace_graph(&mut self, graph: Graph) {
        let published = Arc::clone(&self.published);
        let latest_generation = Arc::clone(&self.latest_generation);
        *self = Self::with_publication_state(graph, published, latest_generation);
    }

    pub fn apply(&mut self, command: GraphCommand) -> Result<(), String> {
        let inverse = apply(&mut self.graph, command).map_err(|error| format!("{error:?}"))?;
        push_bounded(&mut self.undo, inverse);
        self.redo.clear();
        self.dirty = true;
        self.needs_eval = true;
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
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn request_eval(&mut self, snapshot: Arc<StoreSnapshot>) {
        self.queue(snapshot, false);
    }

    pub fn request_publish(&mut self, snapshot: Arc<StoreSnapshot>) {
        self.queue(snapshot, true);
    }

    pub fn poll(&mut self, sender: &IngestSender) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        self.collect_publications(&mut logs);
        while let Ok(mut outcome) = self.rx.try_recv() {
            self.in_flight = false;
            self.cancel = None;
            if outcome.generation == self.generation {
                self.handle_publish(&mut outcome, sender, &mut logs);
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

    pub fn preview_for(&self, node: NodeId, port: usize) -> Option<&PreviewStats> {
        self.last_outcome
            .as_ref()?
            .previews
            .get(&node)?
            .get(port)?
            .as_ref()
    }

    pub fn is_evaluating(&self) -> bool {
        self.in_flight || self.pending.is_some() || self.publications_in_flight > 0
    }

    fn queue(&mut self, snapshot: Arc<StoreSnapshot>, publish: bool) {
        self.generation = self
            .latest_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        self.latest_generation
            .store(self.generation, Ordering::Relaxed);
        self.needs_eval = false;
        let request = PendingRequest {
            generation: self.generation,
            graph: self.graph.clone(),
            snapshot,
            selection: self.selection,
            publish,
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
            let report = evaluate(
                &request.graph,
                &request.snapshot,
                &targets,
                &cancel,
                &mut cache.lock().unwrap(),
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
                                Value::Signal(signal) => Some(preview_stats(signal)),
                                Value::Scalar(_) => None,
                            })
                            .collect::<Vec<Option<PreviewStats>>>(),
                    )
                })
                .collect();
            let publish = request
                .publish
                .then(|| build_outputs(&request.graph, &report));
            let _ = tx.send(EvalOutcome {
                generation: request.generation,
                graph_name: request.graph.name,
                previews,
                diagnostics: report.diagnostics,
                publish,
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

fn preview_stats(signal: &delog_flow::types::Signal) -> PreviewStats {
    let mut count = 0_u64;
    let mut nan_count = 0_u64;
    let mut min = f64::NAN;
    let mut max = f64::NAN;
    let mut mean = 0.0;
    let mut m2 = 0.0;
    for &value in signal.v.iter() {
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
        t0_us: signal.t.first().copied().unwrap_or_default(),
        t1_us: signal.t.last().copied().unwrap_or_default(),
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
                diagnostics: Vec::new(),
                publish: Some(Ok(vec![topic])),
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
                diagnostics: Vec::new(),
                publish: Some(Ok(vec![topic])),
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
                diagnostics: Vec::new(),
                publish: Some(Ok(vec![topic])),
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
}

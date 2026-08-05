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

fn gps_source(
    identity: &mut IdentityRegistry,
    name: &str,
    values: Vec<f64>,
) -> (delog_core::identity::TopicId, Arc<TopicStore>) {
    let source = identity.add_source(name);
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
            vec![Arc::new(Float64Array::from(values)) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    (topic, Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()))
}

fn snapshot_two_sources() -> Arc<StoreSnapshot> {
    let mut identity = IdentityRegistry::new();
    let a = gps_source(&mut identity, "flight_a", vec![1.0, 2.0, 3.0]);
    let b = gps_source(&mut identity, "flight_b", vec![10.0, 20.0, 30.0]);
    Arc::new(StoreSnapshot::from_registry(&identity, [a, b], 1).unwrap())
}

fn agnostic_field() -> NodeKind {
    NodeKind::DataField(FieldSelector {
        source: None,
        topic: "GPS".into(),
        instance: None,
        field: "Alt".into(),
    })
}

#[test]
fn set_field_source_binds_a_source_without_dirtying_and_re_resolves() {
    let mut graph = Graph::new("g");
    let field = graph.alloc_id();
    graph.insert_node(Node {
        id: field,
        pos: [0.0; 2],
        kind: agnostic_field(),
    });
    let mut controller = DataFlowController::new(graph);
    controller.selection = HashSet::from([field]);
    let (sender, _receiver) = ingest_channel();

    // Agnostic + two matching sources -> ambiguous, no preview.
    controller.request_eval(snapshot_two_sources());
    wait_for(&mut controller, &sender);
    assert!(controller.preview_for(field, 0).is_none());
    assert!(
        controller
            .diagnostics_for(field)
            .iter()
            .any(|message| message.contains("ambiguous"))
    );

    // Binding a source is session-only: re-evaluates, does not dirty/undo.
    controller.set_field_source(field, Some("flight_b".into()));
    assert!(controller.needs_eval());
    assert!(!controller.dirty);
    assert!(!controller.can_undo());

    controller.request_eval(snapshot_two_sources());
    wait_for(&mut controller, &sender);
    // flight_b's Alt is [10,20,30] -> mean 20.
    assert_eq!(controller.preview_for(field, 0).unwrap().mean, 20.0);
}

#[test]
fn sole_selection_only_when_exactly_one() {
    let mut controller = DataFlowController::new(Graph::new("g"));
    assert_eq!(controller.sole_selection(), None);
    controller.selection = HashSet::from([NodeId(1)]);
    assert_eq!(controller.sole_selection(), Some(NodeId(1)));
    controller.selection = HashSet::from([NodeId(1), NodeId(2)]);
    assert_eq!(controller.sole_selection(), None);
}

#[test]
fn delete_selection_removes_all_selected_in_one_undo_step() {
    let mut graph = Graph::new("g");
    let a = add_node(&mut graph, data());
    let b = add_node(
        &mut graph,
        NodeKind::ScaleOffset {
            multiplier: 1.0,
            offset: 0.0,
        },
    );
    graph.connect(a, 0, b, 0).unwrap();
    let mut controller = DataFlowController::new(graph);
    controller.selection = HashSet::from([a, b]);

    controller.delete_selection().unwrap();
    assert!(controller.graph.nodes.is_empty());
    assert!(controller.graph.edges.is_empty());
    assert!(controller.selection.is_empty());

    controller.undo();
    assert_eq!(controller.graph.nodes.len(), 2);
    assert_eq!(controller.graph.edges.len(), 1);
}

#[test]
fn copy_paste_duplicates_nodes_and_internal_edges_in_one_undo_step() {
    let mut graph = Graph::new("g");
    let a = add_node(&mut graph, data());
    let b = add_node(
        &mut graph,
        NodeKind::ScaleOffset {
            multiplier: 2.0,
            offset: 0.0,
        },
    );
    graph.connect(a, 0, b, 0).unwrap();
    let a_pos = graph.node(a).unwrap().pos;
    let mut controller = DataFlowController::new(graph);
    controller.selection = HashSet::from([a, b]);

    let clipboard = controller.copy_selection();
    controller.paste(&clipboard, [30.0, 30.0]).unwrap();

    assert_eq!(controller.graph.nodes.len(), 4);
    assert_eq!(controller.graph.edges.len(), 2);
    assert_eq!(controller.selection.len(), 2);
    assert!(!controller.selection.contains(&a));
    assert!(!controller.selection.contains(&b));
    // A pasted DataField sits at the original offset by +30,+30.
    assert!(controller.graph.nodes.iter().any(|node| {
        controller.selection.contains(&node.id)
            && node.pos == [a_pos[0] + 30.0, a_pos[1] + 30.0]
    }));

    controller.undo();
    assert_eq!(controller.graph.nodes.len(), 2);
    assert_eq!(controller.graph.edges.len(), 1);
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
    controller.selection = HashSet::from([scale]);
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
    controller.selection = HashSet::from([node]);
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
    controller.selection = HashSet::from([scale]);
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
    controller.selection = HashSet::from([field]);
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
    controller.selection = HashSet::from([field]);
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
        // Live streaming derived sources must be LiveDerived so the ingestor
        // seals pending rows by age and they become visible without a close.
        Observed::Open("dataflow:g".into(), SourceKind::LiveDerived)
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

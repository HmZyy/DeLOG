#![cfg(feature = "python")]

use std::sync::{Arc, Mutex, mpsc};

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSink, ParsedBatch, SourceKind, ingest_channel};
use delog_core::ingestor::{IngestObserver, Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::DataStore;
use delog_script::{LiveBatchInput, ScriptCommand, ScriptEngine, ScriptEvent};

struct BlockingForwarder {
    scripts: Arc<Mutex<Option<mpsc::Sender<LiveBatchInput>>>>,
    forwarded: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl IngestObserver for BlockingForwarder {
    fn on_batch(&mut self, kind: SourceKind, source_label: &str, batch: &ParsedBatch) {
        if kind != SourceKind::Live {
            return;
        }
        let scripts = self.scripts.lock().unwrap().clone().unwrap();
        scripts
            .send(LiveBatchInput::new(source_label, batch.clone()))
            .unwrap();
        self.forwarded.send(()).unwrap();
        self.release.recv().unwrap();
    }
}

#[test]
fn first_unpublished_live_batch_keeps_its_source_label() {
    let scripts = Arc::new(Mutex::new(None));
    let (forwarded_tx, forwarded_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let ingestor = Ingestor::new(BlockingForwarder {
        scripts: Arc::clone(&scripts),
        forwarded: forwarded_tx,
        release: release_rx,
    });
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let engine = ScriptEngine::spawn(
        Arc::clone(&store),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    *scripts.lock().unwrap() = Some(engine.live_batch_sender());
    engine
        .send(ScriptCommand::RunScript {
            name: "first".into(),
            source: r#"delog.transform("ATTITUDE", source="fresh", multiplier=2.0, output_topic="ATTITUDE_X", mode="live")"#.into(),
        })
        .unwrap();
    wait_for(&engine, ScriptEvent::Done);

    let raw_source = sender.file_sink().open_source("fresh", SourceKind::Live);
    assert!(
        store.load().source(raw_source).is_none(),
        "the source is not published at open"
    );
    sender
        .file_sink()
        .submit(attitude_batch(raw_source, &[1], &[4.0]));
    forwarded_rx.recv().unwrap();
    assert!(
        store.load().source(raw_source).is_none(),
        "observer is blocked before first-batch publication"
    );
    wait_for(&engine, ScriptEvent::LiveBatchProcessed);
    release_tx.send(()).unwrap();

    let derived = wait_for_source(&store, "script:first");
    wait_until(|| topic_times(&store, derived, "ATTITUDE_X") == [1]);
    assert_eq!(topic_values(&store, derived, "ATTITUDE_X", "roll"), [8.0]);

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn both_mode_backfills_once_and_ignores_derived_feedback() {
    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let raw_source = {
        let mut sink = sender.file_sink();
        sink.open_source("live", SourceKind::Live)
    };
    let already_visible = attitude_batch(raw_source, &[100, 200], &[1.0, 2.0]);
    sender.file_sink().submit(already_visible.clone());
    wait_until(|| topic_times(&store, raw_source, "ATTITUDE") == vec![100, 200]);

    let engine = ScriptEngine::spawn(
        Arc::clone(&store),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    engine
        .send(ScriptCommand::RunScript {
            name: "both".into(),
            source: r#"delog.transform("ATTITUDE", multiplier=2.0)"#.into(),
        })
        .unwrap();
    wait_for(&engine, ScriptEvent::Done);
    assert!(
        engine.has_live_transform("both"),
        "Done is the registration barrier"
    );
    engine.try_send_live_batch("live", already_visible).unwrap();
    wait_for(&engine, ScriptEvent::LiveBatchProcessed);

    let newer = attitude_batch(raw_source, &[300], &[3.0]);
    sender.file_sink().submit(newer.clone());
    engine.try_send_live_batch("live", newer).unwrap();
    wait_for(&engine, ScriptEvent::LiveBatchProcessed);

    let derived_source = wait_for_derived_times(&store, &[100, 200, 300]);
    let derived_feedback = attitude_batch(derived_source, &[300], &[6.0]);
    engine
        .try_send_live_batch("script:both", derived_feedback)
        .unwrap();
    wait_for(&engine, ScriptEvent::LiveBatchProcessed);
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(
        topic_times(&store, derived_source, "ATTITUDE"),
        vec![100, 200, 300]
    );

    assert!(engine.has_live_transform("both"));
    engine
        .send(ScriptCommand::UnregisterLive {
            name: "both".into(),
        })
        .unwrap();
    wait_for_output(&engine, "unregistered live transform 'both'");
    wait_until(|| !store.load().is_source_live(derived_source));
    assert!(!engine.has_live_transform("both"));

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn registration_staging_is_lossless_beyond_the_former_queue_capacity() {
    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let raw_source = sender.file_sink().open_source("live", SourceKind::Live);
    sender
        .file_sink()
        .submit(attitude_batch(raw_source, &[0], &[0.0]));
    wait_until(|| topic_times(&store, raw_source, "ATTITUDE") == [0]);

    let engine = ScriptEngine::spawn(
        Arc::clone(&store),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    engine
        .send(ScriptCommand::RunScript {
            name: "staged".into(),
            source: r#"
import time
print("snapshot-captured")
time.sleep(0.3)
delog.transform("ATTITUDE", multiplier=2.0)
"#
            .into(),
        })
        .unwrap();
    wait_for_output(&engine, "snapshot-captured");

    for time in 0..=256 {
        engine
            .try_send_live_batch("live", attitude_batch(raw_source, &[time], &[time as f64]))
            .unwrap_or_else(|_| panic!("staging rejected timestamp {time}"));
    }
    wait_for(&engine, ScriptEvent::Done);

    let derived = wait_for_source(&store, "script:staged");
    let expected = (0..=256).collect::<Vec<_>>();
    wait_until(|| topic_times(&store, derived, "ATTITUDE") == expected);
    assert_eq!(topic_values(&store, derived, "ATTITUDE", "roll")[0], 0.0);
    assert_eq!(topic_times(&store, derived, "ATTITUDE"), expected);

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn dynamic_cross_operation_collision_disables_only_the_losing_producer() {
    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let raw_source = sender.file_sink().open_source("live", SourceKind::Live);
    let engine = ScriptEngine::spawn(
        Arc::clone(&store),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    engine
        .send(ScriptCommand::RunScript {
            name: "owners".into(),
            source: r#"
delog.group_by("PARAM_VALUE", "param_id", mode="live")
delog.group_by("PARAM_VALUE", "param_id", mode="live")
"#
            .into(),
        })
        .unwrap();
    wait_for(&engine, ScriptEvent::Done);

    for time in [10, 20] {
        engine
            .try_send_live_batch("live", param_batch(raw_source, time, time as f64))
            .unwrap();
        wait_for(&engine, ScriptEvent::LiveBatchProcessed);
    }
    engine
        .try_send_live_batch("live", param_batch(raw_source, 30, 30.0))
        .unwrap();
    wait_for_error(&engine, "disabled after 3 consecutive errors");

    let derived = wait_for_source(&store, "script:owners");
    wait_until(|| topic_times(&store, derived, "PARAM_VALUE/A") == [10, 20, 30]);
    assert_eq!(
        topic_values(&store, derived, "PARAM_VALUE/A", "value"),
        [10.0, 20.0, 30.0],
        "the losing producer must never submit duplicate/corrupt batches"
    );

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

fn attitude_batch(
    source: delog_core::identity::SourceId,
    times: &[i64],
    values: &[f64],
) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "ATTITUDE",
            [FieldSchema::new("roll", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    );
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(times.to_vec()),
        vec![Arc::new(Float64Array::from(values.to_vec())) as ArrayRef],
    )
}

fn param_batch(source: delog_core::identity::SourceId, time: i64, value: f64) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "PARAM_VALUE",
            [
                FieldSchema::new("param_id", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("value", DataType::Float64, Some("m"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(vec![time]),
        vec![
            Arc::new(StringArray::from(vec!["A"])) as ArrayRef,
            Arc::new(Float64Array::from(vec![value])) as ArrayRef,
        ],
    )
}

fn wait_for(engine: &ScriptEngine, expected: ScriptEvent) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            if event == expected {
                return;
            }
            if let ScriptEvent::Error(error) = event {
                panic!("script error: {error}");
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {expected:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_output(engine: &ScriptEngine, expected: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            if matches!(event, ScriptEvent::Output(ref output) if output.contains(expected)) {
                return;
            }
            if let ScriptEvent::Error(error) = event {
                panic!("script error: {error}");
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for output {expected}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_error(engine: &ScriptEngine, expected: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            if matches!(event, ScriptEvent::Error(ref error) if error.contains(expected)) {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for error {expected}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_derived_times(store: &DataStore, expected: &[i64]) -> delog_core::identity::SourceId {
    let mut source = None;
    wait_until(|| {
        let snapshot = store.load();
        source = snapshot
            .sources
            .iter()
            .find(|candidate| candidate.entry.label == "script:both" && !candidate.entry.removed)
            .map(|candidate| candidate.entry.id);
        source.is_some_and(|source| topic_times(store, source, "ATTITUDE") == expected)
    });
    source.unwrap()
}

fn wait_for_source(store: &DataStore, label: &str) -> delog_core::identity::SourceId {
    let mut source = None;
    wait_until(|| {
        source = store
            .load()
            .sources
            .iter()
            .find(|candidate| candidate.entry.label == label && !candidate.entry.removed)
            .map(|candidate| candidate.entry.id);
        source.is_some()
    });
    source.unwrap()
}

fn topic_values(
    store: &DataStore,
    source: delog_core::identity::SourceId,
    topic: &str,
    field: &str,
) -> Vec<f64> {
    let snapshot = store.load();
    let topic = snapshot
        .topics
        .iter()
        .find(|candidate| candidate.entry.source == source && candidate.entry.name == topic)
        .unwrap();
    let store = topic.store.as_ref().unwrap();
    let field = store.schema.field_index(field).unwrap();
    store
        .chunks
        .iter()
        .flat_map(|chunk| {
            chunk.cols[field]
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .values()
                .iter()
                .copied()
        })
        .collect()
}

fn topic_times(store: &DataStore, source: delog_core::identity::SourceId, topic: &str) -> Vec<i64> {
    let snapshot = store.load();
    snapshot
        .topics
        .iter()
        .find(|candidate| {
            candidate.entry.source == source
                && candidate.entry.name == topic
                && !candidate.entry.removed
        })
        .and_then(|candidate| candidate.store.as_ref())
        .map(|store| {
            store
                .chunks
                .iter()
                .flat_map(|chunk| chunk.t.values().iter().copied())
                .collect()
        })
        .unwrap_or_default()
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

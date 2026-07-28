#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

const SNAPSHOT_RADIANS_SCRIPT: &str =
    include_str!("../../../scripts/snapshot/nav_controller_output_radians.py");
const SNAPSHOT_EULER_SCRIPT: &str =
    include_str!("../../../scripts/snapshot/vehicle_attitude_euler.py");

#[test]
fn console_eval_executes_declarative_transform() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(ctun_batch(source, &[100], &[90.0], &[180.0]));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "CTUN")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::Eval(
            "DEG_TO_RAD = 0.017453292519943295".into(),
        ))
        .unwrap();
    assert!(wait_done(&engine).is_empty());
    engine
        .send(ScriptCommand::Eval(
            r#"delog.transform("CTUN", multiplier=DEG_TO_RAD, fields=["Pitch", "Roll", "Pitch"], unit="rad", output_topic="CTUN_DEG")"#.into(),
        ))
        .unwrap();
    let errors = wait_done(&engine);
    assert!(errors.is_empty(), "console transform failed: {errors:?}");

    let snapshot = wait_for_source_topics(&store, "script:console", &["CTUN_DEG"]);
    let source = snapshot
        .sources
        .iter()
        .find(|source| source.entry.label == "script:console" && !source.entry.removed)
        .unwrap()
        .entry
        .id;
    assert_f64(
        &snapshot,
        source,
        "CTUN_DEG",
        "Pitch",
        &[std::f64::consts::FRAC_PI_2],
    );
    assert_f64(
        &snapshot,
        source,
        "CTUN_DEG",
        "Roll",
        &[std::f64::consts::PI],
    );
    assert_unit(&snapshot, source, "CTUN_DEG", "Pitch", Some("rad"));
    assert_unit(&snapshot, source, "CTUN_DEG", "Roll", Some("rad"));

    assert!(engine.has_live_transform("console"));
    engine
        .try_send_live_batch("flight", ctun_batch(raw_source, &[200], &[45.0], &[90.0]))
        .unwrap();
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(source, "CTUN_DEG")
            .is_some_and(|topic| topic.rows == 2)
    });
    let snapshot = store.load();
    assert_f64(
        &snapshot,
        source,
        "CTUN_DEG",
        "Pitch",
        &[std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_4],
    );
    assert_f64(
        &snapshot,
        source,
        "CTUN_DEG",
        "Roll",
        &[std::f64::consts::PI, std::f64::consts::FRAC_PI_2],
    );

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn concise_documentation_examples_execute_verbatim() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(nav_controller_batch(source));
        sink.submit(numeric_batch(
            source,
            "GPS",
            &[100, 200],
            "alt",
            &[10.0, 20.0],
        ));
        sink.submit(param_batch(source));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "PARAM_VALUE")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::RunScript {
            name: "documentation_examples".into(),
            source: r#"
delog.transform("NAV_CONTROLLER_OUTPUT", multiplier=0.017453292519943295,
                fields=["nav_roll", "nav_pitch", "nav_bearing"],
                unit="rad", output_topic="NAV_CONTROLLER_OUTPUT_RAD")
delog.split_by("PARAM_VALUE", "param_id")
delog.merge({"NAV_CONTROLLER_OUTPUT": ["nav_roll"], "GPS": ["alt"]},
            base_topic="NAV_CONTROLLER_OUTPUT", output_topic="NAV_WITH_ALT")
"#
            .into(),
        })
        .unwrap();
    let errors = wait_done(&engine);
    assert!(
        errors.is_empty(),
        "documentation examples failed: {errors:?}"
    );

    let snapshot = wait_for_source_topics(
        &store,
        "script:documentation_examples",
        &[
            "NAV_CONTROLLER_OUTPUT_RAD",
            "PARAM_VALUE/A",
            "PARAM_VALUE/B",
            "NAV_WITH_ALT",
        ],
    );
    let source = snapshot
        .sources
        .iter()
        .find(|source| {
            source.entry.label == "script:documentation_examples" && !source.entry.removed
        })
        .unwrap()
        .entry
        .id;
    assert_f64(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_roll",
        &[std::f64::consts::FRAC_PI_2, std::f64::consts::PI],
    );
    assert_f64(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_pitch",
        &[-std::f64::consts::FRAC_PI_2, 0.0],
    );
    assert_f64(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_bearing",
        &[std::f64::consts::PI, 0.0],
    );
    assert_f64(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "aspd_error",
        &[5.0, 10.0],
    );
    assert_unit(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_roll",
        Some("rad"),
    );
    assert_unit(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_pitch",
        Some("rad"),
    );
    assert_unit(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_bearing",
        Some("rad"),
    );
    assert_unit(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "aspd_error",
        Some("m/s"),
    );
    assert_f64(&snapshot, source, "PARAM_VALUE/A", "value", &[3.0, 5.0]);
    assert_f64(&snapshot, source, "PARAM_VALUE/B", "value", &[4.0]);
    assert_f64(
        &snapshot,
        source,
        "NAV_WITH_ALT",
        "nav_roll",
        &[90.0, 180.0],
    );
    assert_f64(&snapshot, source, "NAV_WITH_ALT", "alt", &[10.0, 20.0]);

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn bundled_snapshot_radians_script_emits_without_registering_live_operation() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(nav_controller_batch(source));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "NAV_CONTROLLER_OUTPUT")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::RunScript {
            name: "snapshot_radians".into(),
            source: SNAPSHOT_RADIANS_SCRIPT.into(),
        })
        .unwrap();
    let errors = wait_done(&engine);
    assert!(errors.is_empty(), "snapshot radians failed: {errors:?}");

    let snapshot = wait_for_source_topics(
        &store,
        "script:snapshot_radians",
        &["NAV_CONTROLLER_OUTPUT_RAD"],
    );
    let source = snapshot
        .sources
        .iter()
        .find(|source| source.entry.label == "script:snapshot_radians" && !source.entry.removed)
        .unwrap()
        .entry
        .id;
    assert_f64(
        &snapshot,
        source,
        "NAV_CONTROLLER_OUTPUT_RAD",
        "nav_roll",
        &[std::f64::consts::FRAC_PI_2, std::f64::consts::PI],
    );
    assert!(
        !engine.has_live_transform("snapshot_radians"),
        "snapshot script must not register a live operation"
    );

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn bundled_snapshot_attitude_script_emits_euler_angles() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(vehicle_attitude_batch(source));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "vehicle_attitude[0]")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::RunScript {
            name: "snapshot_attitude".into(),
            source: SNAPSHOT_EULER_SCRIPT.into(),
        })
        .unwrap();
    let errors = wait_done(&engine);
    assert!(errors.is_empty(), "snapshot attitude failed: {errors:?}");

    let snapshot = wait_for_source_topics(
        &store,
        "script:snapshot_attitude",
        &["vehicle_attitude_euler"],
    );
    let source = snapshot
        .sources
        .iter()
        .find(|source| source.entry.label == "script:snapshot_attitude" && !source.entry.removed)
        .unwrap()
        .entry
        .id;
    assert_f64_close(
        &snapshot,
        source,
        "vehicle_attitude_euler",
        "roll",
        &[0.0, 90.0, 0.0, 0.0],
    );
    assert_f64_close(
        &snapshot,
        source,
        "vehicle_attitude_euler",
        "pitch",
        &[0.0, 0.0, 45.0, 0.0],
    );
    assert_f64_close(
        &snapshot,
        source,
        "vehicle_attitude_euler",
        "yaw",
        &[0.0, 0.0, 0.0, 90.0],
    );

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn declarative_operations_share_one_snapshot_and_source() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(numeric_batch(
            source,
            "ATTITUDE",
            &[100, 200],
            "roll",
            &[1.0, 2.0],
        ));
        sink.submit(numeric_batch(
            source,
            "GPS",
            &[100, 200],
            "alt",
            &[10.0, 20.0],
        ));
        sink.submit(param_batch(source));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "PARAM_VALUE")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::RunScript {
            name: "declarative".into(),
            source: r#"
delog.transform("ATTITUDE", multiplier=2.0, offset=10.0, output_topic="ATTITUDE_X")
delog.split_by("PARAM_VALUE", "param_id")
delog.merge({"ATTITUDE": ["roll"], "GPS": ["alt"]},
            base_topic="ATTITUDE", output_topic="STATE")
"#
            .into(),
        })
        .unwrap();
    wait_done(&engine);

    let snapshot = wait_for_source_topics(
        &store,
        "script:declarative",
        &["ATTITUDE_X", "PARAM_VALUE/A", "PARAM_VALUE/B", "STATE"],
    );
    let sources = snapshot
        .sources
        .iter()
        .filter(|source| source.entry.label == "script:declarative" && !source.entry.removed)
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 1, "all declarative outputs share one source");
    let source = sources[0].entry.id;
    assert_f64(&snapshot, source, "ATTITUDE_X", "roll", &[12.0, 14.0]);
    assert_f64(&snapshot, source, "PARAM_VALUE/A", "value", &[3.0, 5.0]);
    assert_f64(&snapshot, source, "PARAM_VALUE/B", "value", &[4.0]);
    assert_f64(&snapshot, source, "STATE", "roll", &[1.0, 2.0]);
    assert_f64(&snapshot, source, "STATE", "alt", &[10.0, 20.0]);

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn snapshot_preparation_failure_opens_no_declarative_source() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(numeric_batch(source, "ATTITUDE", &[100], "roll", &[1.0]));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "ATTITUDE")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::RunScript {
            name: "bad".into(),
            source: r#"delog.transform("ATTITUDE", fields=["missing"], mode="snapshot")"#.into(),
        })
        .unwrap();
    let errors = wait_done(&engine);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("field 'missing' not found"))
    );
    assert!(
        store
            .load()
            .sources
            .iter()
            .all(|source| source.entry.label != "script:bad"),
        "preparation must finish before a declarative source is opened"
    );

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn snapshot_topic_collision_never_opens_or_corrupts_a_derived_source() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = sender.file_sink().open_source("flight", SourceKind::File);
    sender.file_sink().submit(numeric_batch(
        raw_source,
        "ATTITUDE",
        &[100],
        "roll",
        &[1.0],
    ));
    sender
        .file_sink()
        .close_source(raw_source, ParseSummary::default());
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "ATTITUDE")
            .is_some()
    });

    let engine = spawn_engine(Arc::clone(&store), sender.clone());
    engine
        .send(ScriptCommand::RunScript {
            name: "collision".into(),
            source: r#"
delog.transform("ATTITUDE", output_topic="SHARED", mode="snapshot")
delog.merge({"ATTITUDE": ["roll"]}, base_topic="ATTITUDE",
            output_topic="SHARED", mode="snapshot")
"#
            .into(),
        })
        .unwrap();
    let errors = wait_done(&engine);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("output topic 'SHARED'") && error.contains("operation 0")),
        "{errors:?}"
    );
    assert!(
        store
            .load()
            .sources
            .iter()
            .all(|source| source.entry.label != "script:collision"),
        "collision must not expose a partial or corrupted store source"
    );

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

#[test]
fn declarative_rerun_prepares_without_matching_its_prior_generation() {
    let (store, sender, ingest_thread) = start_ingestor();
    let raw_source = {
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        sink.submit(numeric_batch(source, "ATTITUDE", &[100], "roll", &[1.0]));
        sink.close_source(source, ParseSummary::default());
        source
    };
    wait_until(|| {
        store
            .load()
            .topic_store_by_name(raw_source, "ATTITUDE")
            .is_some()
    });
    let engine = spawn_engine(Arc::clone(&store), sender.clone());

    for multiplier in [2.0, 3.0] {
        engine
            .send(ScriptCommand::RunScript {
                name: "rerun".into(),
                source: format!(
                    "delog.transform('ATTITUDE', multiplier={multiplier}, mode='snapshot')"
                ),
            })
            .unwrap();
        let errors = wait_done(&engine);
        assert!(errors.is_empty(), "rerun failed: {errors:?}");
        let snapshot = wait_for_source_topics(&store, "script:rerun", &["ATTITUDE"]);
        assert_f64(
            &snapshot,
            snapshot
                .sources
                .iter()
                .find(|source| source.entry.label == "script:rerun" && !source.entry.removed)
                .unwrap()
                .entry
                .id,
            "ATTITUDE",
            "roll",
            &[multiplier],
        );
    }

    let active = store
        .load()
        .sources
        .iter()
        .filter(|source| source.entry.label == "script:rerun" && !source.entry.removed)
        .count();
    assert_eq!(active, 1);

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
}

fn start_ingestor() -> (
    Arc<DataStore>,
    delog_core::ingest::IngestSender,
    std::thread::JoinHandle<()>,
) {
    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let thread = std::thread::spawn(move || ingestor.run(receiver));
    (store, sender, thread)
}

fn spawn_engine(store: Arc<DataStore>, sender: delog_core::ingest::IngestSender) -> ScriptEngine {
    ScriptEngine::spawn(
        store,
        sender,
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    )
}

fn numeric_batch(
    source: delog_core::identity::SourceId,
    topic: &str,
    times: &[i64],
    field: &str,
    values: &[f64],
) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            topic,
            [FieldSchema::new(field, DataType::Float64, None::<String>, 1.0).unwrap()],
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

fn nav_controller_batch(source: delog_core::identity::SourceId) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "NAV_CONTROLLER_OUTPUT",
            [
                FieldSchema::new("nav_roll", DataType::Float64, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("nav_pitch", DataType::Float64, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("nav_bearing", DataType::Float64, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("aspd_error", DataType::Float64, Some("m/s"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(vec![100, 200]),
        vec![
            Arc::new(Float64Array::from(vec![90.0, 180.0])) as ArrayRef,
            Arc::new(Float64Array::from(vec![-90.0, 0.0])) as ArrayRef,
            Arc::new(Float64Array::from(vec![180.0, 0.0])) as ArrayRef,
            Arc::new(Float64Array::from(vec![5.0, 10.0])) as ArrayRef,
        ],
    )
}

fn vehicle_attitude_batch(source: delog_core::identity::SourceId) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "vehicle_attitude[0]",
            [
                FieldSchema::new("q[0]", DataType::Float64, None::<String>, 1.0).unwrap(),
                FieldSchema::new("q[1]", DataType::Float64, None::<String>, 1.0).unwrap(),
                FieldSchema::new("q[2]", DataType::Float64, None::<String>, 1.0).unwrap(),
                FieldSchema::new("q[3]", DataType::Float64, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let half_sqrt = std::f64::consts::FRAC_1_SQRT_2;
    let pitch_w = (std::f64::consts::FRAC_PI_4 / 2.0).cos();
    let pitch_y = (std::f64::consts::FRAC_PI_4 / 2.0).sin();
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(vec![100, 200, 300, 400]),
        vec![
            Arc::new(Float64Array::from(vec![1.0, half_sqrt, pitch_w, half_sqrt])) as ArrayRef,
            Arc::new(Float64Array::from(vec![0.0, half_sqrt, 0.0, 0.0])) as ArrayRef,
            Arc::new(Float64Array::from(vec![0.0, 0.0, pitch_y, 0.0])) as ArrayRef,
            Arc::new(Float64Array::from(vec![0.0, 0.0, 0.0, half_sqrt])) as ArrayRef,
        ],
    )
}

fn ctun_batch(
    source: delog_core::identity::SourceId,
    times: &[i64],
    pitch: &[f64],
    roll: &[f64],
) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "CTUN",
            [
                FieldSchema::new("Pitch", DataType::Float64, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("Roll", DataType::Float64, Some("deg"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(times.to_vec()),
        vec![
            Arc::new(Float64Array::from(pitch.to_vec())) as ArrayRef,
            Arc::new(Float64Array::from(roll.to_vec())) as ArrayRef,
        ],
    )
}

fn param_batch(source: delog_core::identity::SourceId) -> ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "PARAM_VALUE",
            [
                FieldSchema::new("param_id", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(vec![100, 150, 200]),
        vec![
            Arc::new(StringArray::from(vec!["A", "B", "A"])) as ArrayRef,
            Arc::new(Float64Array::from(vec![3.0, 4.0, 5.0])) as ArrayRef,
        ],
    )
}

fn wait_done(engine: &ScriptEngine) -> Vec<String> {
    let mut errors = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            match event {
                ScriptEvent::Done => return errors,
                ScriptEvent::Error(error) => errors.push(error),
                _ => {}
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for Done"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_source_topics(store: &DataStore, label: &str, topics: &[&str]) -> StoreSnapshot {
    let mut found = None;
    wait_until(|| {
        let snapshot = store.load();
        let Some(source) = snapshot
            .sources
            .iter()
            .find(|source| source.entry.label == label && !source.entry.removed)
        else {
            return false;
        };
        let ready = topics.iter().all(|topic| {
            snapshot
                .topic_store_by_name(source.entry.id, topic)
                .is_some()
        });
        if ready {
            found = Some((*snapshot).clone());
        }
        ready
    });
    found.unwrap()
}

fn assert_f64(
    snapshot: &StoreSnapshot,
    source: delog_core::identity::SourceId,
    topic: &str,
    field: &str,
    expected: &[f64],
) {
    let store = snapshot.topic_store_by_name(source, topic).unwrap();
    let index = store.schema.field_index(field).unwrap();
    let values = store
        .chunks
        .iter()
        .flat_map(|chunk| {
            chunk.cols[index]
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .values()
                .iter()
                .copied()
        })
        .collect::<Vec<_>>();
    assert_eq!(values, expected);
}

fn assert_f64_close(
    snapshot: &StoreSnapshot,
    source: delog_core::identity::SourceId,
    topic: &str,
    field: &str,
    expected: &[f64],
) {
    let store = snapshot.topic_store_by_name(source, topic).unwrap();
    let index = store.schema.field_index(field).unwrap();
    let values = store
        .chunks
        .iter()
        .flat_map(|chunk| {
            chunk.cols[index]
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .values()
                .iter()
                .copied()
        })
        .collect::<Vec<_>>();
    assert_eq!(values.len(), expected.len());
    for (actual, expected) in values.iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 1e-10,
            "{topic}.{field}: expected {expected}, got {actual}"
        );
    }
}

fn assert_unit(
    snapshot: &StoreSnapshot,
    source: delog_core::identity::SourceId,
    topic: &str,
    field: &str,
    expected: Option<&str>,
) {
    let store = snapshot.topic_store_by_name(source, topic).unwrap();
    assert_eq!(
        store
            .schema
            .field_by_name(field)
            .and_then(|field| field.unit.as_deref()),
        expected,
    );
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

trait SnapshotByName {
    fn topic_store_by_name(
        &self,
        source: delog_core::identity::SourceId,
        topic: &str,
    ) -> Option<&Arc<delog_core::store::TopicStore>>;
}

impl SnapshotByName for StoreSnapshot {
    fn topic_store_by_name(
        &self,
        source: delog_core::identity::SourceId,
        topic: &str,
    ) -> Option<&Arc<delog_core::store::TopicStore>> {
        self.topics
            .iter()
            .find(|entry| {
                entry.entry.source == source && entry.entry.name == topic && !entry.entry.removed
            })
            .and_then(|entry| entry.store.as_ref())
    }
}

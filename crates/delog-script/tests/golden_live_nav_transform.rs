#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int16Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSink, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

fn read_store() -> Arc<DataStore> {
    Arc::new(DataStore::from_snapshot(StoreSnapshot::empty()))
}

fn nav_batch(source: delog_core::identity::SourceId) -> delog_core::ingest::ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "NAV_CONTROLLER_OUTPUT",
            [
                FieldSchema::new("nav_roll", DataType::Float32, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("nav_pitch", DataType::Float32, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("nav_bearing", DataType::Int16, Some("deg"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Float32Array::from(vec![0.0, 90.0])),
        Arc::new(Float32Array::from(vec![45.0, -45.0])),
        Arc::new(Int16Array::from(vec![180, -90])),
    ];
    delog_core::ingest::ParsedBatch::new(source, schema, Int64Array::from(vec![1, 2]), columns)
}

#[test]
fn live_transform_appends_derived_batches() {
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
    );
    let script = r#"
DEG_TO_RAD = 0.017453292519943295

@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
def convert(batch):
    return {
        "nav_roll_rad": batch.nav_roll * DEG_TO_RAD,
        "nav_pitch_rad": (batch.nav_pitch * DEG_TO_RAD, "rad"),
        "nav_bearing_rad": (batch.t, batch.nav_bearing * DEG_TO_RAD, "rad"),
    }
"#;
    engine.send(ScriptCommand::RunScript {
        name: "nav_rad".into(),
        source: script.into(),
    });

    wait_done(&engine);

    let raw_source = {
        let mut sink = sender.file_sink();
        sink.open_source("live", delog_core::ingest::SourceKind::Live)
    };
    engine.try_send_live_batch(nav_batch(raw_source)).unwrap();
    wait_live_processed(&engine);

    let snap = wait_for_topic(&write_store, "NAV_CONTROLLER_OUTPUT_RAD");
    let topic = snap
        .topics
        .iter()
        .find(|t| t.entry.name == "NAV_CONTROLLER_OUTPUT_RAD")
        .unwrap();
    let store = snap.topic_store(topic.entry.id).unwrap();
    let idx = store.schema.field_index("nav_bearing_rad").unwrap();
    let values = store.chunks[0].cols[idx]
        .as_any()
        .downcast_ref::<arrow::array::Float64Array>()
        .unwrap();
    assert!((values.value(0) - std::f64::consts::PI).abs() < 1e-12);
    assert!((values.value(1) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

fn wait_done(engine: &ScriptEngine) {
    wait_for(engine, ScriptEvent::Done, "ScriptEvent::Done");
}

fn wait_live_processed(engine: &ScriptEngine) {
    wait_for(
        engine,
        ScriptEvent::LiveBatchProcessed,
        "ScriptEvent::LiveBatchProcessed",
    );
}

fn wait_for(engine: &ScriptEngine, expected: ScriptEvent, label: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            if event == expected {
                return;
            }
            if let ScriptEvent::Error(err) = event {
                panic!("script error: {err}");
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_topic(store: &DataStore, topic: &str) -> StoreSnapshot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let snap = store.load();
        if snap.topics.iter().any(|t| t.entry.name == topic) {
            return (*snap).clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for topic {topic}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

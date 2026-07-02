#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int64Array, StringArray, UInt32Array};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSink, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

const SPLIT_SCRIPT: &str = include_str!("../../../scripts/named_values_live_split.py");

fn read_store() -> Arc<DataStore> {
    Arc::new(DataStore::from_snapshot(StoreSnapshot::empty()))
}

fn named_value_batch(source: delog_core::identity::SourceId) -> delog_core::ingest::ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "NAMED_VALUE_FLOAT",
            [
                FieldSchema::new("time_boot_ms", DataType::UInt32, Some("ms"), 1.0).unwrap(),
                FieldSchema::new("name", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("value", DataType::Float32, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(vec![1, 2, 3])),
        Arc::new(StringArray::from(vec!["airspd", "clbrate", "airspd"])),
        Arc::new(Float32Array::from(vec![1.5, 2.5, 3.5])),
    ];
    delog_core::ingest::ParsedBatch::new(source, schema, Int64Array::from(vec![100, 200, 300]), columns)
}

#[test]
fn named_value_split_creates_one_topic_per_name() {
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
    );
    let _ = engine.send(ScriptCommand::RunScript {
        name: "named_values".into(),
        source: SPLIT_SCRIPT.into(),
    });
    wait_done(&engine);

    let raw_source = {
        let mut sink = sender.file_sink();
        sink.open_source("live", delog_core::ingest::SourceKind::Live)
    };
    engine.try_send_live_batch(named_value_batch(raw_source)).unwrap();
    wait_live_processed(&engine);

    let snap = wait_for_topic(&write_store, "NAMED_VALUE_FLOAT/airspd");
    let assert_topic = |snap: &StoreSnapshot, name: &str, times: &[i64], values: &[f64]| {
        let topic = snap.topics.iter().find(|t| t.entry.name == name).unwrap();
        let store = snap.topic_store(topic.entry.id).unwrap();
        let idx = store.schema.field_index("value").unwrap();
        let chunk = &store.chunks[0];
        let got: Vec<i64> = (0..chunk.len()).map(|r| chunk.t.value(r)).collect();
        assert_eq!(got, times, "times for {name}");
        let col = chunk.cols[idx]
            .as_any()
            .downcast_ref::<arrow::array::Float64Array>()
            .unwrap();
        let got: Vec<f64> = (0..col.len()).map(|r| col.value(r)).collect();
        assert_eq!(got, values, "values for {name}");
    };
    assert_topic(&snap, "NAMED_VALUE_FLOAT/airspd", &[100, 300], &[1.5, 3.5]);
    let snap = wait_for_topic(&write_store, "NAMED_VALUE_FLOAT/clbrate");
    assert_topic(&snap, "NAMED_VALUE_FLOAT/clbrate", &[200], &[2.5]);

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

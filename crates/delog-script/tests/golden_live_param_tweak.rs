#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSink, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::params::{self, ParamValue};
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

const TUNABLE_LOWPASS_SCRIPT: &str = include_str!("../../../scripts/live/tunable_lowpass.py");

fn read_store() -> Arc<DataStore> {
    Arc::new(DataStore::from_snapshot(StoreSnapshot::empty()))
}

fn imu_batch(
    source: delog_core::identity::SourceId,
    times: &[i64],
    ax: &[f32],
) -> delog_core::ingest::ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "IMU",
            [FieldSchema::new("AccX", DataType::Float32, Some("m/s^2"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = vec![Arc::new(Float32Array::from(ax.to_vec()))];
    delog_core::ingest::ParsedBatch::new(source, schema, Int64Array::from(times.to_vec()), columns)
}

#[test]
fn bundled_tunable_lowpass_sees_param_edit_on_next_batch() {
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let store = params::shared_empty();
    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        Arc::clone(&store),
    );

    engine
        .send(ScriptCommand::RunScript {
            name: "lowpass".into(),
            source: TUNABLE_LOWPASS_SCRIPT.into(),
        })
        .unwrap();
    wait_for(&engine, ScriptEvent::Done, "Done");

    let raw = {
        let mut s = sender.file_sink();
        s.open_source("live", delog_core::ingest::SourceKind::Live)
    };

    // Batch 1 with default alpha=0.2: [0, 10] -> [0, 2].
    engine
        .try_send_live_batch("live", imu_batch(raw, &[1, 2], &[0.0, 10.0]))
        .unwrap();
    wait_for(
        &engine,
        ScriptEvent::LiveBatchProcessed,
        "LiveBatchProcessed",
    );

    // UI edit: alpha -> 0.5.
    store
        .lock()
        .unwrap()
        .set_value("lowpass", "alpha", ParamValue::Float(0.5));

    // Batch 2 with alpha=0.5: [10, 20] -> [10, 15].
    engine
        .try_send_live_batch("live", imu_batch(raw, &[3, 4], &[10.0, 20.0]))
        .unwrap();
    wait_for(
        &engine,
        ScriptEvent::LiveBatchProcessed,
        "LiveBatchProcessed",
    );

    let snap = wait_for_topic(&write_store, "IMU_LPF");
    let topic = snap
        .topics
        .iter()
        .find(|t| t.entry.name == "IMU_LPF")
        .unwrap();
    let ts = snap.topic_store(topic.entry.id).unwrap();
    let idx = ts.schema.field_index("AccX_lpf").unwrap();
    // Two chunks appended (one per batch); assert the values across them.
    let mut vals = Vec::new();
    for chunk in ts.chunks.iter() {
        let a = chunk.cols[idx]
            .as_any()
            .downcast_ref::<arrow::array::Float64Array>()
            .unwrap();
        for i in 0..a.len() {
            vals.push(a.value(i));
        }
    }
    assert_eq!(
        vals,
        vec![0.0, 2.0, 10.0, 15.0],
        "alpha edit must apply to the second batch"
    );

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
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

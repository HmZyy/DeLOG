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

fn read_store() -> Arc<DataStore> {
    Arc::new(DataStore::from_snapshot(StoreSnapshot::empty()))
}

fn imu_batch(
    source: delog_core::identity::SourceId,
    t: i64,
    ax: f32,
) -> delog_core::ingest::ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "IMU",
            [FieldSchema::new("AccX", DataType::Float32, Some("m/s^2"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = vec![Arc::new(Float32Array::from(vec![ax]))];
    delog_core::ingest::ParsedBatch::new(source, schema, Int64Array::from(vec![t]), columns)
}

#[test]
fn live_callback_sees_param_edit_on_next_batch() {
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

    let script = r#"
gain = delog.slider("gain", 2.0, min=0.0, max=10.0)

@delog.live_transform(topic="IMU", fields=["AccX"], output_topic="IMU_SCALED")
def scale(batch):
    g = delog.param("gain")
    return {"AccX_scaled": (batch.AccX * g, "m/s^2")}
"#;
    engine
        .send(ScriptCommand::RunScript {
            name: "scaler".into(),
            source: script.into(),
        })
        .unwrap();
    wait_for(&engine, ScriptEvent::Done, "Done");

    let raw = {
        let mut s = sender.file_sink();
        s.open_source("live", delog_core::ingest::SourceKind::Live)
    };

    // Batch 1 with default gain=2.0 -> 5.0 * 2.0 = 10.0
    engine
        .try_send_live_batch("live", imu_batch(raw, 1, 5.0))
        .unwrap();
    wait_for(
        &engine,
        ScriptEvent::LiveBatchProcessed,
        "LiveBatchProcessed",
    );

    // UI edit: gain -> 3.0
    store
        .lock()
        .unwrap()
        .set_value("scaler", "gain", ParamValue::Float(3.0));

    // Batch 2 with gain=3.0 -> 5.0 * 3.0 = 15.0
    engine
        .try_send_live_batch("live", imu_batch(raw, 2, 5.0))
        .unwrap();
    wait_for(
        &engine,
        ScriptEvent::LiveBatchProcessed,
        "LiveBatchProcessed",
    );

    let snap = wait_for_topic(&write_store, "IMU_SCALED");
    let topic = snap
        .topics
        .iter()
        .find(|t| t.entry.name == "IMU_SCALED")
        .unwrap();
    let ts = snap.topic_store(topic.entry.id).unwrap();
    let idx = ts.schema.field_index("AccX_scaled").unwrap();
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
        vec![10.0, 15.0],
        "gain edit must apply to the 2nd batch"
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

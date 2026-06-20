#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::chunk::Chunk;
use delog_core::identity::IdentityRegistry;
use delog_core::ingest::ingest_channel;
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_core::store::TopicStore;
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

/// Build a read store with IMU.AccX/Y/Z = (3, 4, 0) at t=0.
/// The 3-4-0 right triangle gives magnitude 5 (3-4-5 Pythagorean triple).
fn read_store() -> Arc<DataStore> {
    let mut id = IdentityRegistry::new();
    let src = id.add_source("flight");
    let topic = id.add_topic(src, "IMU").unwrap();
    for f in ["AccX", "AccY", "AccZ"] {
        id.add_field(topic, f).unwrap();
    }
    let schema = Arc::new(
        TopicSchema::new(
            "IMU",
            ["AccX", "AccY", "AccZ"]
                .iter()
                .map(|n| FieldSchema::new(*n, DataType::Float64, Some("m/s^2"), 1.0).unwrap()),
        )
        .unwrap(),
    );
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Float64Array::from(vec![3.0])),
        Arc::new(Float64Array::from(vec![4.0])),
        Arc::new(Float64Array::from(vec![0.0])),
    ];
    let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![0]), cols, &schema).unwrap());
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();
    Arc::new(DataStore::from_snapshot(snap))
}

#[test]
fn accel_magnitude_script_emits_expected_values() {
    // Write side: a real ingestor thread to receive derived data.
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    // Read side: the IMU store built above.
    let engine = ScriptEngine::spawn(read_store(), sender, Arc::new(MetricsRegistry::new()));
    let script = r#"
import numpy as np
x = delog.field('flight/IMU/AccX').v
y = delog.field('flight/IMU/AccY').v
z = delog.field('flight/IMU/AccZ').v
t = delog.field('flight/IMU/AccX').t
out = delog.output(t, "AccMag")
out.add_field("mag", np.sqrt(x*x + y*y + z*z), unit="m/s^2")
"#;
    let _ = engine.send(ScriptCommand::RunScript {
        name: "accel_mag".into(),
        source: script.into(),
    });

    // drain_events() is the public non-test API; poll until Done or Error.
    // recv_blocking() is #[cfg(test)]-gated inside the engine module and
    // therefore not accessible from integration test crates.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let events = engine.drain_events();
        let mut done = false;
        for ev in events {
            match ev {
                ScriptEvent::Done => {
                    done = true;
                }
                ScriptEvent::Error(e) => panic!("script error: {e}"),
                _ => {}
            }
        }
        if done {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for ScriptEvent::Done"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // Drop the engine so its sender clone is released; the ingest thread will
    // drain the channel and exit when the last sender is dropped.
    drop(engine);

    // Emission is async through the ingest channel; poll write_store for the
    // derived AccMag topic.
    let out = {
        let mut snap = write_store.load();
        for _ in 0..100 {
            if snap.topics.iter().any(|t| t.entry.name == "AccMag") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            snap = write_store.load();
        }
        snap
    };

    let derived_topic = out
        .topics
        .iter()
        .find(|t| t.entry.name == "AccMag")
        .expect("AccMag topic emitted");
    let ts = out.topic_store(derived_topic.entry.id).unwrap();

    assert_eq!(ts.chunks[0].cols.len(), 1, "expected 1 field column");
    let mag = ts.chunks[0].cols[0]
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap()
        .value(0);
    assert!(
        (mag - 5.0).abs() < 1e-9,
        "expected magnitude 5.0, got {mag}"
    );

    let _ = ingest_thread;
}

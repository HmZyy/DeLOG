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

static SCRIPT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// AccX/Y/Z = (3, 4, 0) so the magnitude is exactly 5 (3-4-5 triple).
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

fn read_store_with_baro_gps() -> Arc<DataStore> {
    let mut id = IdentityRegistry::new();
    let src = id.add_source("flight");
    let baro = id.add_topic(src, "BARO").unwrap();
    let gps = id.add_topic(src, "GPS").unwrap();
    id.add_field(baro, "Alt").unwrap();
    id.add_field(gps, "Alt").unwrap();

    let baro_schema = Arc::new(
        TopicSchema::new(
            "BARO",
            [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let gps_schema = Arc::new(
        TopicSchema::new(
            "GPS",
            [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let baro_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![0, 10, 20]),
            vec![Arc::new(Float64Array::from(vec![100.0, 101.0, 102.0])) as ArrayRef],
            &baro_schema,
        )
        .unwrap(),
    );
    let gps_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![0, 15]),
            vec![Arc::new(Float64Array::from(vec![90.0, 95.0])) as ArrayRef],
            &gps_schema,
        )
        .unwrap(),
    );
    let baro_store = Arc::new(TopicStore::from_chunks(baro_schema, [baro_chunk]).unwrap());
    let gps_store = Arc::new(TopicStore::from_chunks(gps_schema, [gps_chunk]).unwrap());
    let snap = StoreSnapshot::from_registry(&id, [(baro, baro_store), (gps, gps_store)], 0).unwrap();
    Arc::new(DataStore::from_snapshot(snap))
}

#[test]
fn accel_magnitude_script_emits_expected_values() {
    let _guard = SCRIPT_TEST_LOCK.lock().unwrap();
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender,
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
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

    // recv_blocking() is #[cfg(test)]-gated and unreachable from this crate,
    // so poll drain_events() instead.
    wait_done(&engine);

    // Releases the engine's sender clone so the ingest thread can exit.
    drop(engine);

    // Emission is async through the ingest channel; poll until AccMag appears.
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

fn wait_done(engine: &ScriptEngine) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for ev in engine.drain_events() {
            match ev {
                ScriptEvent::Done => return,
                ScriptEvent::Error(e) => panic!("script error: {e}"),
                _ => {}
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for ScriptEvent::Done"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn run_script_capture_output(engine: &ScriptEngine, name: &str, source: &str) -> String {
    engine
        .send(ScriptCommand::RunScript {
            name: name.into(),
            source: source.into(),
        })
        .unwrap();
    let mut captured = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for ev in engine.drain_events() {
            match ev {
                ScriptEvent::Output(s) => captured.push_str(&s),
                ScriptEvent::Done => return captured,
                ScriptEvent::Error(e) => panic!("script error: {e}"),
                _ => {}
            }
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn run_script_capture_error(engine: &ScriptEngine, name: &str, source: &str) -> String {
    engine
        .send(ScriptCommand::RunScript {
            name: name.into(),
            source: source.into(),
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for ev in engine.drain_events() {
            match ev {
                ScriptEvent::Error(e) => return e,
                ScriptEvent::Done => panic!("script unexpectedly succeeded"),
                _ => {}
            }
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn discovery_refs_expose_paths_and_metadata() {
    let _guard = SCRIPT_TEST_LOCK.lock().unwrap();
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    let output = run_script_capture_output(
        &engine,
        "discovery",
        r#"
topic = delog.topic("IMU")
field = delog.find("IMU", "AccX")
all_fields = topic.fields()
catalog_fields = delog.catalog().fields()
print(topic.path)
print(field.path)
print(field.unit)
print(",".join(f.name for f in all_fields))
print(len(catalog_fields))
"#,
    );

    assert!(output.contains("flight/IMU"));
    assert!(output.contains("flight/IMU/AccX"));
    assert!(output.contains("m/s^2"));
    assert!(output.contains("AccX,AccY,AccZ"));
    assert!(output.contains("3"));

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

#[test]
fn topic_ref_reads_table_columns() {
    let _guard = SCRIPT_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    let output = run_script_capture_output(
        &engine,
        "table_read",
        r#"
imu = delog.topic("IMU").read("AccX", "AccY", "AccZ")
accx_ref = delog.topic("IMU").field("AccX")
accx = accx_ref.read()
accx_again = delog.field(accx_ref)
print(list(imu.fields()))
print(float(imu.AccX[0]))
print(float(imu["AccY"][0]))
print(int(imu.t[0]))
print(float(accx.v[0]))
print(float(accx_again.v[0]))
"#,
    );

    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(
        lines,
        ["['AccX', 'AccY', 'AccZ']", "3.0", "4.0", "0", "3.0", "3.0"]
    );

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

#[test]
fn field_align_prev_matches_resample_prev() {
    let _guard = SCRIPT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store_with_baro_gps(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    let output = run_script_capture_output(
        &engine,
        "align_prev",
        r#"
baro = delog.topic("BARO").field("Alt").read()
gps = delog.topic("GPS").field("Alt").read()
aligned = gps.align_prev(baro)
print(",".join(str(float(v)) for v in aligned))
"#,
    );

    assert!(output.contains("90.0,90.0,95.0"));

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

#[test]
fn emit_helper_publishes_derived_topic() {
    let _guard = SCRIPT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    engine
        .send(ScriptCommand::RunScript {
            name: "ergonomic_accel".into(),
            source: r#"
import numpy as np
imu = delog.topic("IMU").read("AccX", "AccY", "AccZ")
norm = np.sqrt(imu.AccX**2 + imu.AccY**2 + imu.AccZ**2)
delog.emit("imu_derived", imu.t, {
    "Acc_norm": (norm, "m/s^2"),
})
"#
            .into(),
        })
        .unwrap();
    wait_done(&engine);

    drop(engine);

    let mut snap = write_store.load();
    for _ in 0..100 {
        if snap.topics.iter().any(|t| t.entry.name == "imu_derived") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        snap = write_store.load();
    }
    let topic = snap
        .topics
        .iter()
        .find(|t| t.entry.name == "imu_derived")
        .expect("imu_derived topic emitted");
    let store = snap.topic_store(topic.entry.id).unwrap();
    let idx = store.schema.field_index("Acc_norm").unwrap();
    let values = store.chunks[0].cols[idx]
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap();
    assert!((values.value(0) - 5.0).abs() < 1e-9);

    drop(sender);
    let _ = ingest_thread.join();
}

#[test]
fn discovery_missing_lookup_errors_include_candidates() {
    let _guard = SCRIPT_TEST_LOCK.lock().unwrap();
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );

    let missing_topic = run_script_capture_error(
        &engine,
        "missing_topic",
        r#"
delog.topic("GPS")
"#,
    );
    assert!(missing_topic.contains("flight/IMU"), "{missing_topic}");

    let missing_field = run_script_capture_error(
        &engine,
        "missing_field",
        r#"
delog.find("IMU", "Nope")
"#,
    );
    assert!(missing_field.contains("flight/IMU/AccX"), "{missing_field}");

    let missing_topic_ref_field = run_script_capture_error(
        &engine,
        "missing_topic_ref_field",
        r#"
delog.topic("IMU").field("Nope")
"#,
    );
    assert!(
        missing_topic_ref_field.contains("flight/IMU/AccX"),
        "{missing_topic_ref_field}"
    );

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

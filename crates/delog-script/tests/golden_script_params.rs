#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::params::{self, ParamKind, ParamValue};
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

/// Serializes the tests in this file: each spawns a `ScriptEngine`, and all
/// engines in one process share the embedded CPython interpreter's global
/// `sys.stdout` (see `engine::tests::ENGINE_LOCK`). Running two concurrently
/// makes one engine's stdout capture clobber the other's.
static ENGINE_LOCK: Mutex<()> = Mutex::new(());

fn read_store() -> Arc<DataStore> {
    Arc::new(DataStore::from_snapshot(StoreSnapshot::empty()))
}

fn run(engine: &ScriptEngine, name: &str, src: &str) {
    engine
        .send(ScriptCommand::RunScript { name: name.into(), source: src.into() })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for e in engine.drain_events() {
            if let ScriptEvent::Error(err) = &e {
                panic!("script error: {err}");
            }
            if e == ScriptEvent::Done {
                return;
            }
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn declarations_register_specs_and_defaults() {
    let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // A real Ingestor drains the sink so nothing blocks (matches the pattern in
    // golden_live_nav_transform.rs).
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = delog_core::ingest::ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let store = params::shared_empty();
    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        Arc::clone(&store),
    );

    run(
        &engine,
        "demo",
        r#"
gain   = delog.slider("gain", 1.5, min=0.0, max=10.0, step=0.5)
window = delog.slider("window", 8, min=1, max=64)
smooth = delog.checkbox("smooth", True)
mode   = delog.combo("mode", ["raw", "lpf"], default="lpf")
label  = delog.text("label", "speed")
"#,
    );

    let s = store.lock().unwrap();
    let sp = &s.scripts["demo"];
    let names: Vec<_> = sp.specs.iter().map(|x| x.name.clone()).collect();
    assert_eq!(names, vec!["gain", "window", "smooth", "mode", "label"]);
    assert_eq!(s.value("demo", "gain"), Some(ParamValue::Float(1.5)));
    assert!(matches!(
        s.spec("demo", "window").unwrap().kind,
        ParamKind::Slider { integer: true, .. }
    ));
    assert_eq!(s.value("demo", "smooth"), Some(ParamValue::Bool(true)));
    assert_eq!(s.value("demo", "mode"), Some(ParamValue::Text("lpf".into())));
    assert_eq!(s.value("demo", "label"), Some(ParamValue::Text("speed".into())));

    drop(s);
    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

#[test]
fn param_read_reflects_store_edits() {
    let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = delog_core::ingest::ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let store = params::shared_empty();
    let engine = ScriptEngine::spawn(
        read_store(),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        Arc::clone(&store),
    );

    // First run declares and prints the current value (default 2.0).
    run(&engine, "demo", r#"
gain = delog.slider("gain", 2.0, min=0.0, max=10.0)
print(f"gain={delog.param('gain')}")
"#);

    // Simulate a UI edit.
    store.lock().unwrap().set_value("demo", "gain", ParamValue::Float(6.0));

    // Re-run: the declaration keeps the edited value, and delog.param sees it.
    let mut captured = String::new();
    engine.send(ScriptCommand::RunScript {
        name: "demo".into(),
        source: "print(f\"gain={delog.param('gain')}\")".into(),
    }).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    'outer: loop {
        for e in engine.drain_events() {
            match e {
                ScriptEvent::Output(s) => captured.push_str(&s),
                ScriptEvent::Error(err) => panic!("script error: {err}"),
                ScriptEvent::Done => break 'outer,
                _ => {}
            }
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(captured.contains("gain=6.0"), "captured: {captured:?}");

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

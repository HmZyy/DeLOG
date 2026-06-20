#![cfg(feature = "python")]

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use arrow::array::Float32Array;
use delog_core::diagnostics::Diag;
use delog_core::ingest::ingest_channel;
use delog_core::ingestor::{IngestObserver, Ingestor};
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::DataStore;
use delog_script::{ParserEvent, ScriptCommand, ScriptEngine, ScriptEvent};

const REPRESENTATIVE_PARSER: &str = r#"import numpy as np

def Parse(raw_data):
    n = len(raw_data) // 2
    raw_data = raw_data[:n * 2].reshape(-1, 2)
    index = np.linspace(0, 1, n)
    return [
        ("AP.index", index, "normalized index"),
        ("AP.rtc", raw_data[:, 0], "seconds"),
        ("AP.value", raw_data[:, 1].view("int32"), "raw integer view"),
        ("AP.value", raw_data[:, 1], "last duplicate wins"),
    ]
"#;

struct Observer(mpsc::Sender<Diag>);

impl IngestObserver for Observer {
    fn on_diagnostic(&mut self, diag: Diag) {
        let _ = self.0.send(diag);
    }
}

#[test]
fn representative_custom_parser_reaches_canonical_store_transactionally() {
    let root =
        std::env::temp_dir().join(format!("delog-golden-custom-parser-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("golden-flight.bin");
    let mut bytes = Vec::new();
    for value in [1.25_f32, 10.0, 2.5, 20.0] {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    std::fs::write(&path, bytes).unwrap();

    let (diag_tx, diag_rx) = mpsc::channel();
    let ingestor = Ingestor::new(Observer(diag_tx));
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let metrics = Arc::new(MetricsRegistry::new());
    let engine = ScriptEngine::spawn(
        Arc::new(DataStore::new()),
        sender.clone(),
        Arc::clone(&metrics),
    );

    engine.send(ScriptCommand::ParseFile {
        parser_name: "representative.py".into(),
        source: REPRESENTATIVE_PARSER.into(),
        path: path.clone(),
    });
    let events = wait_for_parser_terminal(&engine);
    assert!(matches!(events[0], ParserEvent::Reading { .. }));
    assert!(matches!(events[1], ParserEvent::Running { .. }));
    assert_eq!(
        events[2],
        ParserEvent::Succeeded {
            parser_name: "representative.py".into(),
            path: path.clone(),
            topics: 1,
            rows: 2,
        }
    );

    let snapshot = wait_for_source(&store, "golden-flight", 1);
    let source = snapshot
        .sources
        .iter()
        .find(|source| source.entry.label == "golden-flight")
        .unwrap();
    let topic = snapshot
        .topics
        .iter()
        .find(|topic| topic.entry.source == source.entry.id && topic.entry.name == "AP")
        .unwrap();
    let topic_store = snapshot.topic_store(topic.entry.id).unwrap();
    assert_eq!(topic_store.chunks[0].t.values(), &[1_250_000, 2_500_000]);
    assert!(topic_store.schema.field_by_name("index").is_some());
    assert!(topic_store.schema.field_by_name("rtc").is_some());
    let value_index = topic_store.schema.field_index("value").unwrap();
    assert_eq!(
        topic_store
            .schema
            .field(value_index)
            .unwrap()
            .description
            .as_deref(),
        Some("last duplicate wins")
    );
    let values = topic_store.chunks[0].cols[value_index]
        .as_any()
        .downcast_ref::<Float32Array>()
        .unwrap();
    assert_eq!(values.values(), &[10.0, 20.0]);

    let warning = diag_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(warning.code, "python-parser-duplicate");
    assert_eq!(warning.source, Some(source.entry.id));
    for key in [
        "python_parser_input_bytes",
        "python_parser_output_bytes",
        "python_parser_read",
        "python_parser_python",
        "python_parser_convert",
        "python_parser_total",
    ] {
        assert_eq!(metrics.stats(key).unwrap().n, 1, "missing metric {key}");
    }
    assert_eq!(
        metrics.stats("python_parser_input_bytes").unwrap().last,
        16.0
    );

    engine.send(ScriptCommand::ParseFile {
        parser_name: "invalid.py".into(),
        source: "def Parse(raw_data):\n    return [('BROKEN.value', raw_data, '')]\n".into(),
        path: path.clone(),
    });
    let failure_events = wait_for_parser_terminal(&engine);
    assert!(matches!(
        failure_events.last(),
        Some(ParserEvent::Failed { .. })
    ));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        store
            .load()
            .sources
            .iter()
            .filter(|source| !source.entry.removed)
            .count(),
        1,
        "failed parser must not open a source"
    );
    assert_eq!(metrics.counter("python_parser_failures"), Some(1));

    drop(engine);
    drop(sender);
    ingest_thread.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn wait_for_parser_terminal(engine: &ScriptEngine) -> Vec<ParserEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut parser_events = Vec::new();
    loop {
        for event in engine.drain_events() {
            match event {
                ScriptEvent::Parser(event) => {
                    let terminal = matches!(
                        event,
                        ParserEvent::Succeeded { .. } | ParserEvent::Failed { .. }
                    );
                    parser_events.push(event);
                    if terminal {
                        return parser_events;
                    }
                }
                ScriptEvent::Done => panic!("parser command emitted ScriptEvent::Done"),
                ScriptEvent::Error(error) => panic!("ordinary script error: {error}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for parser");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_source(
    store: &DataStore,
    label: &str,
    count: usize,
) -> Arc<delog_core::snapshot::StoreSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = store.load();
        if snapshot
            .sources
            .iter()
            .filter(|source| source.entry.label == label && !source.entry.removed)
            .count()
            == count
        {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for source");
        std::thread::sleep(Duration::from_millis(5));
    }
}

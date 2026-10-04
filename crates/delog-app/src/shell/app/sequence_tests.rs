use delog_core::ingest::ingest_channel;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[test]
fn sequence_script_dataflow_script_and_layout_share_published_results() {
    use delog_script::{ScriptCommand, ScriptEngine};
    let ingestor = delog_core::ingestor::Ingestor::new(delog_core::ingestor::NullObserver);
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let thread = std::thread::spawn(move || ingestor.run(receiver));
    let engine = ScriptEngine::spawn(
        Arc::clone(&store),
        sender.clone(),
        Arc::new(delog_core::metrics::MetricsRegistry::new()),
        delog_api::params::shared_empty(),
    );
    let run_script = |name: &str, source: &str| {
        let (reply, receipt) = mpsc::channel();
        engine
            .send(ScriptCommand::Tracked {
                command: Box::new(ScriptCommand::RunScript {
                    name: name.into(),
                    source: source.into(),
                }),
                reply,
            })
            .unwrap();
        receipt
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
    };
    run_script(
        "seed",
        "import numpy as np\ndelog.emit('GPS', np.array([100, 200, 300], dtype=np.int64), {'Alt': np.array([1., 2., 3.])})",
    );
    let graph = delog_flow::doc::from_json(&serde_json::json!({
        "delog_dataflow": 1, "name": "double", "next_id": 4,
        "viewport": {"offset": [0.0, 0.0], "zoom": 1.0},
        "nodes": [
            {"id": 1, "pos": [0.0, 0.0], "type": "data_field", "source": "script:seed", "topic": "GPS", "instance": null, "field": "Alt"},
            {"id": 2, "pos": [200.0, 0.0], "type": "scale_offset", "multiplier": 2.0, "offset": 0.0},
            {"id": 3, "pos": [400.0, 0.0], "type": "output", "topic": "DERIVED", "fields": [{"name": "value", "unit": null}]}
        ],
        "edges": [{"from": 1, "to": 2, "to_port": 0}, {"from": 2, "to": 3, "to_port": 0}]
    })).unwrap();
    let mut flow = crate::dataflow::headless::HeadlessFlow::new(graph, "sequence-step".into());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, result) = flow.drive(&store.load(), &sender, false, 0.0, 0, 2.0);
        if let Some(result) = result {
            result.unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    run_script(
        "check",
        "values = delog.topic('DERIVED').read('value')\nassert list(values.value) == [2., 4., 6.]\ndelog.emit('CHECKED', values.t, {'value': values.value})",
    );
    let doc = crate::config::layout::doc::decode_doc(r#"{
        "delog_layout": 1, "name": "result", "playback": {"speed": 1.0, "follow_live": false},
        "workspace": {"root": {"plot": {"traces": [{"field": {"topic": "CHECKED", "field": "value"}, "color": [0.3, 0.7, 1.0, 1.0], "width_px": 1.5, "mode": "line", "visible": true}], "show_legend": true, "show_tooltip": true}}}, "vehicles": []
    }"#).unwrap();
    let layout = crate::shell::layout_apply::load_doc(doc, &store.load()).unwrap();
    assert!(matches!(
        layout,
        crate::shell::layout_apply::LoadOutcome::Applied(_)
    ));
    flow.controller
        .stop_owned(&sender)
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    drop(flow);
    drop(engine);
    drop(sender);
    thread.join().unwrap();
}

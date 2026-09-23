#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSender, IngestSink, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::{
    ControlHost, ControlRequest, ControlResponse, PlotRequest, ScriptCommand, ScriptEngine,
    ScriptEvent,
};

struct PlotsHost;

impl ControlHost for PlotsHost {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String> {
        match request {
            ControlRequest::Plots(PlotRequest::List) => Ok(ControlResponse::Plots(Vec::new())),
            other => Err(format!("unexpected request: {other:?}")),
        }
    }
}

const LIVE_SCRIPT: &str = r#"
@delog.live_transform(topic="IMU", fields=["AccX"], output_topic="IMU_OUT")
def touch_the_ui(batch):
    delog.plots()
    return {"AccX": batch.AccX}
"#;

fn engine_with_host() -> (ScriptEngine, IngestSender, std::thread::JoinHandle<()>) {
    let ingestor = Ingestor::new(NullObserver);
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let engine = ScriptEngine::spawn(
        Arc::new(DataStore::from_snapshot(StoreSnapshot::empty())),
        sender.clone(),
        Arc::new(MetricsRegistry::new()),
        delog_script::params::shared_empty(),
    );
    let host: Arc<dyn ControlHost> = Arc::new(PlotsHost);
    engine.set_control_host(host);
    (engine, sender, ingest_thread)
}

fn errors_until_done(engine: &ScriptEngine) -> Vec<String> {
    let mut errors = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            match event {
                ScriptEvent::Error(error) => errors.push(error),
                ScriptEvent::Done => return errors,
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

fn imu_batch(source: delog_core::identity::SourceId) -> delog_core::ingest::ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "IMU",
            [FieldSchema::new("AccX", DataType::Float32, Some("m/s^2"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = vec![Arc::new(Float32Array::from(vec![0.0, 1.0]))];
    delog_core::ingest::ParsedBatch::new(source, schema, Int64Array::from(vec![1, 2]), columns)
}

fn drive_live_batches_until_disabled(engine: &ScriptEngine, sender: &IngestSender) -> Vec<String> {
    let mut errors = Vec::new();
    for _ in 0..3 {
        let raw_source = {
            let mut sink = sender.file_sink();
            sink.open_source("live", delog_core::ingest::SourceKind::Live)
        };
        engine
            .try_send_live_batch("live", imu_batch(raw_source))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        'wait: loop {
            for event in engine.drain_events() {
                match event {
                    ScriptEvent::Error(error) => errors.push(error),
                    ScriptEvent::LiveBatchProcessed => break 'wait,
                    _ => {}
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for ScriptEvent::LiveBatchProcessed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    errors
}

#[test]
fn a_named_run_can_reach_the_control_api() {
    let (engine, _sender, _ingest) = engine_with_host();
    engine
        .send(ScriptCommand::RunScript {
            name: "probe.py".into(),
            source: "delog.plots()\n".into(),
        })
        .unwrap();
    assert_eq!(errors_until_done(&engine), Vec::<String>::new());
}

#[test]
fn a_live_callback_cannot_reach_the_control_api() {
    let (engine, sender, _ingest) = engine_with_host();
    engine
        .send(ScriptCommand::RunScript {
            name: "live_probe.py".into(),
            source: LIVE_SCRIPT.into(),
        })
        .unwrap();
    let _ = errors_until_done(&engine);

    let errors = drive_live_batches_until_disabled(&engine, &sender);
    assert!(
        errors.iter().any(|e| e.contains("not available")),
        "{errors:?}"
    );
}

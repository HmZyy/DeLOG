use super::doc::{SequenceDoc, StepKind};
use super::runner::{RunStatus, SequenceRunner, StepOutcome, StepToken};
use super::store::SequenceStore;

#[test]
fn sequence_storage_reorders_repeated_references_and_preserves_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = SequenceStore::new(dir.path().into());
    let mut doc = SequenceDoc::new("flight");
    doc.push(StepKind::Script, "derive");
    doc.push(StepKind::Layout, "overview");
    let moved = doc.push(StepKind::Script, "derive");
    doc.move_step(2, 0).unwrap();
    assert_eq!(doc.steps[0].id, moved);
    assert_eq!(doc.steps[2].reference, "overview");
    store.save(&doc).unwrap();
    assert_eq!(store.load("flight").unwrap(), doc);
    store.rename("flight", "renamed").unwrap();
    assert_eq!(store.load("renamed").unwrap().id, doc.id);
    assert!(store.load("flight").is_err());
    store.delete("renamed").unwrap();
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn sequence_storage_rejects_bad_documents_and_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let store = SequenceStore::new(dir.path().into());
    for name in ["", "..", "../escape", "a/b", "a\\b"] {
        assert!(store.save(&SequenceDoc::new(name)).is_err());
    }
    store.save(&SequenceDoc::new("one")).unwrap();
    store.save(&SequenceDoc::new("two")).unwrap();
    assert!(store.rename("one", "two").is_err());
    let mut doc = SequenceDoc::new("bad");
    doc.push(StepKind::Script, "derive");
    doc.steps.push(doc.steps[0].clone());
    assert!(store.save(&doc).is_err());
    std::fs::write(dir.path().join("corrupt.json"), "{").unwrap();
    assert!(store.load("corrupt").is_err());
}

#[test]
fn sequence_runner_waits_and_stops_on_failure() {
    let mut doc = SequenceDoc::new("flight");
    doc.push(StepKind::Script, "first");
    doc.push(StepKind::Layout, "last");
    let mut runner = SequenceRunner::start(doc, 1);
    let request = runner.take_request().unwrap();
    assert!(runner.take_request().is_none());
    runner.complete(
        StepToken {
            run: 2,
            step: request.token.step,
        },
        StepOutcome::Succeeded,
    );
    assert!(runner.take_request().is_none());
    runner.complete(request.token, StepOutcome::Failed("broken".into()));
    assert_eq!(runner.status, RunStatus::Failed);
    assert!(runner.take_request().is_none());
    assert_eq!(runner.error.as_deref(), Some("broken"));
}

#[test]
fn sequence_runner_advances_once_and_cancels() {
    let mut doc = SequenceDoc::new("flight");
    doc.push(StepKind::Script, "first");
    doc.push(StepKind::Script, "first");
    let mut runner = SequenceRunner::start(doc, 3);
    let first = runner.take_request().unwrap();
    runner.complete(first.token, StepOutcome::Succeeded);
    let second = runner.take_request().unwrap();
    assert_ne!(first.token, second.token);
    runner.complete(first.token, StepOutcome::Failed("stale".into()));
    assert_eq!(runner.status, RunStatus::Running);
    runner.complete(second.token, StepOutcome::Cancelled);
    assert_eq!(runner.status, RunStatus::Cancelled);
    assert!(runner.take_request().is_none());
    assert_eq!(
        SequenceRunner::start(SequenceDoc::new("empty"), 4).status,
        RunStatus::Succeeded
    );
}

#[test]
fn sequence_runtime_rejects_overlap_and_logs_failure_context() {
    let mut runtime = super::runtime::SequenceRuntime::default();
    let mut doc = SequenceDoc::new("flight");
    doc.push(StepKind::Script, "broken-script");
    runtime.start(doc.clone()).unwrap();
    assert!(runtime.start(doc.clone()).is_err());
    let request = runtime
        .runs
        .get_mut(&doc.id)
        .unwrap()
        .take_request()
        .unwrap();
    runtime.finish(
        &doc.id,
        request.token,
        StepOutcome::Failed("bad input".into()),
    );
    let message = &runtime.logs.last().unwrap().1;
    for text in ["flight", "1", "Script", "broken-script", "bad input"] {
        assert!(message.contains(text));
    }
    assert!(runtime.start(doc).is_ok());
}

#[test]
fn sequence_manager_renders_without_data_or_scripting() {
    let mut manager = super::window::SequenceManager::default();
    manager.open = true;
    manager.draft = Some(SequenceDoc::new("flight"));
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
        manager.show(
            ui.ctx(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
    });
}

#[test]
fn sequence_restart_waits_for_cleanup_and_rejects_stale_completion() {
    let mut runtime = super::runtime::SequenceRuntime::default();
    let mut doc = SequenceDoc::new("restart");
    doc.push(StepKind::Script, "derive");
    runtime.start(doc.clone()).unwrap();
    let (_, old) = runtime.take_request().unwrap();
    runtime.finish(&doc.id, old.token, StepOutcome::Succeeded);
    runtime.start(doc.clone()).unwrap();
    let (reply, receipt) = std::sync::mpsc::channel();
    runtime.cleanup.insert(doc.id.clone(), vec![receipt]);
    assert!(runtime.take_request().is_none());
    reply.send(Ok(())).unwrap();
    runtime.poll_cleanup();
    let (_, current) = runtime.take_request().unwrap();
    let log_count = runtime.logs.len();
    runtime.finish(&doc.id, old.token, StepOutcome::Failed("stale".into()));
    assert_eq!(runtime.logs.len(), log_count);
    assert_eq!(runtime.runs[&doc.id].status, RunStatus::Running);
    runtime.finish(&doc.id, current.token, StepOutcome::Succeeded);
    assert_eq!(runtime.runs[&doc.id].status, RunStatus::Succeeded);
}

#[test]
fn sequence_cleanup_failure_prevents_dispatch() {
    let mut runtime = super::runtime::SequenceRuntime::default();
    let mut doc = SequenceDoc::new("restart");
    doc.push(StepKind::Script, "derive");
    runtime.start(doc.clone()).unwrap();
    let (reply, receipt) = std::sync::mpsc::channel();
    runtime.cleanup.insert(doc.id.clone(), vec![receipt]);
    reply.send(Err("cleanup failed".into())).unwrap();
    runtime.poll_cleanup();
    assert!(runtime.take_request().is_none());
    assert_eq!(runtime.runs[&doc.id].status, RunStatus::Failed);
}

#[test]
fn sequence_layout_interruption_is_reported_before_another_mapping_completes() {
    let mut runtime = super::runtime::SequenceRuntime::default();
    let mut doc = SequenceDoc::new("layout-run");
    doc.push(StepKind::Layout, "first");
    runtime.start(doc.clone()).unwrap();
    let (_, request) = runtime.take_request().unwrap();
    runtime.active = Some(super::runtime::ActiveStep {
        sequence: doc.id,
        request,
        operation: super::runtime::ActiveOperation::Layout,
    });
    runtime.interrupt_layout();
    assert!(matches!(
        runtime.layout_result,
        Some(StepOutcome::Failed(_))
    ));
}

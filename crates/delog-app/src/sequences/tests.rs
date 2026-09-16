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

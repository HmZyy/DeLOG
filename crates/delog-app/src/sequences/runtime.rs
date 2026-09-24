use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use super::doc::SequenceDoc;
use super::runner::{RunStatus, SequenceRunner, StepOutcome, StepRequest, StepToken};
use super::window::SequenceManager;
use crate::dataflow::headless::HeadlessFlow;
use crate::ui::logging::LogLevel;

pub type Receipt = Receiver<Result<(), String>>;

pub enum ActiveOperation {
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    Picker(Receiver<Option<PathBuf>>),
    ScriptReady(Option<PathBuf>),
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    Script(Receipt),
    Flow,
    Layout,
}

pub struct ActiveStep {
    pub sequence: String,
    pub request: StepRequest,
    pub operation: ActiveOperation,
}

pub struct OwnedFlow {
    pub sequence_name: String,
    pub step_index: usize,
    pub reference: String,
    pub flow: HeadlessFlow,
}

#[derive(Default)]
pub struct SequenceRuntime {
    pub manager: SequenceManager,
    pub runs: BTreeMap<String, SequenceRunner>,
    pub flows: BTreeMap<(String, u64), OwnedFlow>,
    pub cleanup: BTreeMap<String, Vec<Receipt>>,
    pub active: Option<ActiveStep>,
    pub layout_result: Option<StepOutcome>,
    pub logs: Vec<(LogLevel, String)>,
    next_run: u64,
}

impl SequenceRuntime {
    pub fn interrupt_layout(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| matches!(active.operation, ActiveOperation::Layout))
        {
            self.layout_result = Some(StepOutcome::Failed(
                "layout step interrupted by another layout change".into(),
            ));
        }
    }
    pub fn take_request(&mut self) -> Option<(String, StepRequest)> {
        if self.active.is_some() {
            return None;
        }
        self.runs
            .iter_mut()
            .filter(|(id, _)| !self.cleanup.contains_key(*id))
            .find_map(|(id, run)| run.take_request().map(|request| (id.clone(), request)))
    }
    pub fn poll_cleanup(&mut self) {
        let mut cleanup_errors = Vec::new();
        for (id, receipts) in &mut self.cleanup {
            receipts.retain(|receipt| match receipt.try_recv() {
                Ok(Ok(())) => false,
                Ok(Err(error)) => {
                    cleanup_errors.push((id.clone(), error));
                    false
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    cleanup_errors.push((id.clone(), "dataflow cleanup worker stopped".into()));
                    false
                }
                Err(mpsc::TryRecvError::Empty) => true,
            });
        }
        self.cleanup.retain(|_, receipts| !receipts.is_empty());
        for (id, error) in cleanup_errors {
            if let Some(run) = self.runs.get_mut(&id) {
                run.status = RunStatus::Failed;
                run.error = Some(error.clone());
                self.logs.push((
                    LogLevel::Error,
                    format!("Sequence '{}': restart failed: {error}", run.doc.name),
                ));
            }
        }
    }

    pub fn start(&mut self, doc: SequenceDoc) -> Result<(), String> {
        doc.validate()?;
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.sequence == doc.id)
        {
            return Err("this sequence is still running or stopping".into());
        }
        if self
            .runs
            .get(&doc.id)
            .is_some_and(|run| run.status == RunStatus::Running)
        {
            return Err("this sequence is already running".into());
        }
        self.next_run += 1;
        self.logs.push((
            LogLevel::Info,
            format!("Sequence '{}': run started", doc.name),
        ));
        if doc.steps.is_empty() {
            self.logs.push((
                LogLevel::Info,
                format!("Sequence '{}': completed (no steps)", doc.name),
            ));
        }
        self.runs
            .insert(doc.id.clone(), SequenceRunner::start(doc, self.next_run));
        Ok(())
    }
    pub fn finish(&mut self, sequence: &str, token: StepToken, outcome: StepOutcome) {
        let Some(run) = self.runs.get_mut(sequence) else {
            return;
        };
        if !run.accepts(token) {
            return;
        }
        let Some(step) = run.doc.steps.get(run.cursor) else {
            return;
        };
        let prefix = format!(
            "Sequence '{}', step {} ({} '{}')",
            run.doc.name,
            run.cursor + 1,
            step.kind.label(),
            step.reference
        );
        let message = match &outcome {
            StepOutcome::Succeeded => (LogLevel::Info, format!("{prefix}: succeeded")),
            StepOutcome::Failed(error) => (LogLevel::Error, format!("{prefix}: failed: {error}")),
            StepOutcome::Cancelled => (LogLevel::Warning, format!("{prefix}: cancelled")),
        };
        run.complete(token, outcome);
        self.logs.push(message);
        if run.status == RunStatus::Succeeded {
            self.logs.push((
                LogLevel::Info,
                format!("Sequence '{}': completed", run.doc.name),
            ));
        }
    }
    pub fn stop_flows(&mut self, sequence: &str, sender: &delog_core::ingest::IngestSender) {
        let keys: Vec<_> = self
            .flows
            .keys()
            .filter(|(id, _)| id == sequence)
            .cloned()
            .collect();
        for key in keys {
            if let Some(mut owned) = self.flows.remove(&key) {
                self.cleanup
                    .entry(sequence.into())
                    .or_default()
                    .push(owned.flow.controller.stop_owned(sender));
            }
        }
    }
}

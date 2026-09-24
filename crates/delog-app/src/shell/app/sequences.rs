#[cfg(feature = "scripting")]
use std::sync::Arc;
use std::sync::mpsc;

use super::DelogApp;
use crate::sequences::doc::StepKind;
use crate::sequences::runner::{RunStatus, StepOutcome};
use crate::sequences::runtime::{ActiveOperation, ActiveStep, OwnedFlow};
use crate::sequences::window::{Catalog, ManagerAction};
use crate::ui::logging::{LogLevel, PendingLog};

impl DelogApp {
    pub(super) fn run_named_sequence(&mut self, name: &str) {
        let result = crate::sequences::store::SequenceStore::default_store()
            .ok_or_else(|| "application data directory is unavailable".to_owned())
            .and_then(|store| store.load(name));
        match result {
            Ok(doc) => {
                let id = doc.id.clone();
                match self.sequences.start(doc) {
                    Ok(()) => self
                        .sequences
                        .stop_flows(&id, &self.session.ingest_sender()),
                    Err(error) => self.sequences.logs.push((LogLevel::Error, error)),
                }
            }
            Err(error) => self
                .sequences
                .logs
                .push((LogLevel::Error, format!("Sequence '{name}': {error}"))),
        }
    }

    pub(super) fn cancel_sequences(&mut self) {
        for run in self.sequences.runs.values_mut() {
            if run.status == RunStatus::Running {
                run.cancel();
                self.sequences.logs.push((
                    LogLevel::Warning,
                    format!("Sequence '{}': cancelled", run.doc.name),
                ));
            }
        }
        if let Some(active) = self.sequences.active.take() {
            match active.operation {
                ActiveOperation::Flow => {
                    let key = (active.sequence.clone(), active.request.step.id);
                    if let Some(mut owned) = self.sequences.flows.remove(&key) {
                        self.sequences
                            .cleanup
                            .entry(active.sequence)
                            .or_default()
                            .push(
                                owned
                                    .flow
                                    .controller
                                    .stop_owned(&self.session.ingest_sender()),
                            );
                    }
                }
                ActiveOperation::Layout => {
                    self.pending_layout = None;
                }
                ActiveOperation::Picker(_) => {
                    self.sequences.active = Some(active);
                }
                ActiveOperation::Script(_) => {
                    #[cfg(feature = "scripting")]
                    {
                        if active.request.step.kind == StepKind::Parser {
                            self.scripts.cancel_parsers();
                        } else {
                            self.scripts.request_interrupt();
                        }
                    }
                    self.sequences.active = Some(active);
                }
                _ => {}
            }
        }
        self.sequences.layout_result = None;
    }

    pub(super) fn drive_sequences(&mut self, ctx: &egui::Context) {
        let mut sequences = std::mem::take(&mut self.sequences);
        let sender = self.session.ingest_sender();
        let snapshot = self.session.snapshot();
        let now = ctx.input(|input| input.time);
        let live = self.session.has_connected_live();
        let mut flow_completions = Vec::new();
        for ((sequence, step), owned) in &mut sequences.flows {
            let (logs, result) = owned.flow.drive(
                &snapshot,
                &sender,
                live,
                now,
                self.settings.dataflow.live_throttle_ms,
                self.settings.dataflow.live_overlap_secs,
            );
            for (level, message) in logs {
                sequences.logs.push((
                    level,
                    format!(
                        "Sequence '{}', step {} (Dataflow '{}'): {message}",
                        owned.sequence_name, owned.step_index, owned.reference
                    ),
                ));
            }
            if let Some(result) = result {
                flow_completions.push((sequence.clone(), *step, result));
            }
        }

        sequences.poll_cleanup();

        if let Some(mut active) = sequences.active.take() {
            let cancelled = sequences
                .runs
                .get(&active.sequence)
                .is_none_or(|run| !run.accepts(active.request.token));
            let outcome = match &mut active.operation {
                ActiveOperation::Picker(receipt) => match receipt.try_recv() {
                    Ok(Some(_)) if cancelled => Some(StepOutcome::Cancelled),
                    Ok(Some(path)) => {
                        active.operation = ActiveOperation::ScriptReady(Some(path));
                        None
                    }
                    Ok(None) => Some(StepOutcome::Cancelled),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(StepOutcome::Failed("file picker stopped".into()))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                },
                ActiveOperation::ScriptReady(path) => {
                    #[cfg(feature = "scripting")]
                    {
                        if self.scripts.ordinary_dispatch_enabled() {
                            match self.scripts.run_sequence_step(
                                &active.request.step.reference,
                                path.take(),
                                self.session.store(),
                                sender.clone(),
                                Arc::clone(self.session.metrics()),
                            ) {
                                Ok(receipt) => {
                                    active.operation = ActiveOperation::Script(receipt);
                                    None
                                }
                                Err(error) => Some(StepOutcome::Failed(error)),
                            }
                        } else {
                            None
                        }
                    }
                    #[cfg(not(feature = "scripting"))]
                    {
                        let _ = path;
                        Some(StepOutcome::Failed(
                            "this build does not support Python scripting or parsers".into(),
                        ))
                    }
                }
                ActiveOperation::Script(receipt) => match receipt.try_recv() {
                    Ok(Ok(())) => Some(StepOutcome::Succeeded),
                    Ok(Err(error)) => Some(StepOutcome::Failed(error)),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(StepOutcome::Failed("script worker stopped".into()))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                },
                ActiveOperation::Flow => flow_completions
                    .iter()
                    .find(|(id, step, _)| id == &active.sequence && *step == active.request.step.id)
                    .map(|(_, _, result)| match result {
                        Ok(()) => StepOutcome::Succeeded,
                        Err(error) => StepOutcome::Failed(error.clone()),
                    }),
                ActiveOperation::Layout => sequences.layout_result.take(),
            };
            if let Some(outcome) = outcome {
                if matches!(&outcome, StepOutcome::Failed(_))
                    && matches!(active.operation, ActiveOperation::Flow)
                {
                    if let Some(mut owned) = sequences
                        .flows
                        .remove(&(active.sequence.clone(), active.request.step.id))
                    {
                        sequences
                            .cleanup
                            .entry(active.sequence.clone())
                            .or_default()
                            .push(owned.flow.controller.stop_owned(&sender));
                    }
                }
                sequences.finish(&active.sequence, active.request.token, outcome);
            } else {
                sequences.active = Some(active);
            }
        }

        if sequences.active.is_none() && self.pending_layout.is_none() {
            let next = sequences.take_request();
            if let Some((id, request)) = next {
                let run = &sequences.runs[&id];
                sequences.logs.push((
                    LogLevel::Info,
                    format!(
                        "Sequence '{}', step {} ({} '{}'): started",
                        run.doc.name,
                        run.cursor + 1,
                        request.step.kind.label(),
                        request.step.reference
                    ),
                ));
                let operation = match request.step.kind {
                    StepKind::Parser => {
                        #[cfg(feature = "scripting")]
                        {
                            let (reply, receipt) = mpsc::channel();
                            let ctx = ctx.clone();
                            let title = format!(
                                "Sequence '{}': input for {}",
                                run.doc.name, request.step.reference
                            );
                            std::thread::spawn(move || {
                                let path = rfd::FileDialog::new().set_title(title).pick_file();
                                let _ = reply.send(path);
                                ctx.request_repaint();
                            });
                            Ok(Some(ActiveOperation::Picker(receipt)))
                        }
                        #[cfg(not(feature = "scripting"))]
                        {
                            Err("this build does not support Python parsers".into())
                        }
                    }
                    StepKind::Script => Ok(Some(ActiveOperation::ScriptReady(None))),
                    StepKind::Dataflow => {
                        let graph = crate::dataflow::store::GraphStore::default_dir()
                            .ok_or("application data directory is unavailable".to_owned())
                            .and_then(|dir| {
                                crate::dataflow::store::GraphStore::new(dir)
                                    .load(&request.step.reference)
                            });
                        match graph {
                            Ok(graph) => {
                                let key = (id.clone(), request.step.id);
                                #[allow(unused_mut)]
                                let mut flow = crate::dataflow::headless::HeadlessFlow::new(
                                    graph,
                                    format!("{}:{}", id, request.step.id),
                                );
                                #[cfg(feature = "scripting")]
                                if flow.controller.graph.nodes.iter().any(|node| {
                                    matches!(node.kind, delog_flow::graph::NodeKind::Script(_))
                                }) {
                                    let host = self.scripts.engine_flow_host(
                                        self.session.store(),
                                        sender.clone(),
                                        Arc::clone(self.session.metrics()),
                                    );
                                    flow.controller.set_script_host(Some(Arc::new(host)));
                                }
                                sequences.flows.insert(
                                    key,
                                    OwnedFlow {
                                        sequence_name: run.doc.name.clone(),
                                        step_index: run.cursor + 1,
                                        reference: request.step.reference.clone(),
                                        flow,
                                    },
                                );
                                Ok(Some(ActiveOperation::Flow))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    StepKind::Layout => {
                        crate::config::layout::doc::load_named_doc(&request.step.reference)
                            .map_err(|e| e.to_string())
                            .and_then(|doc| {
                                match crate::shell::layout_apply::load_doc(doc.clone(), &snapshot)
                                    .map_err(|e| e.to_string())?
                                {
                                    crate::shell::layout_apply::LoadOutcome::Applied(layout) => {
                                        self.apply_layout(layout);
                                        self.deferred_layout_doc =
                                            (!Self::snapshot_has_fields(&snapshot)).then_some(doc);
                                        Ok(None)
                                    }
                                    crate::shell::layout_apply::LoadOutcome::NeedsMapping(
                                        pending,
                                    ) => {
                                        self.deferred_layout_doc = None;
                                        self.pending_layout = Some(pending);
                                        sequences.layout_result = None;
                                        Ok(Some(ActiveOperation::Layout))
                                    }
                                }
                            })
                    }
                };
                match operation {
                    Ok(Some(operation)) => {
                        sequences.active = Some(ActiveStep {
                            sequence: id,
                            request,
                            operation,
                        })
                    }
                    Ok(None) => sequences.finish(&id, request.token, StepOutcome::Succeeded),
                    Err(error) => sequences.finish(&id, request.token, StepOutcome::Failed(error)),
                }
            }
        }

        let mut catalog = Catalog::default();
        if sequences.manager.open {
            catalog.layouts = crate::config::layout::doc::list_layouts();
            if let Some(dir) = crate::dataflow::store::GraphStore::default_dir() {
                catalog.dataflows = crate::dataflow::store::GraphStore::new(dir).list();
            }
            #[cfg(feature = "scripting")]
            {
                catalog.scripts = self.scripts.script_names();
                catalog.parsers = self.scripts.parser_names().unwrap_or_default();
            }
        }
        let live_ids = sequences.flows.keys().map(|(id, _)| id.clone()).collect();
        let actions = sequences
            .manager
            .show(ctx, &catalog, &sequences.runs, &live_ids);
        let mut cancel = false;
        for action in actions {
            match action {
                ManagerAction::Run(doc) => {
                    let id = doc.id.clone();
                    match sequences.start(doc) {
                        Ok(()) => sequences.stop_flows(&id, &sender),
                        Err(error) => sequences.logs.push((LogLevel::Error, error)),
                    }
                }
                ManagerAction::Delete(id) => {
                    sequences.stop_flows(&id, &sender);
                    sequences.runs.remove(&id);
                }
                ManagerAction::Cancel => cancel = true,
            }
        }
        if sequences.active.is_some()
            || !sequences.cleanup.is_empty()
            || !sequences.flows.is_empty()
            || sequences
                .runs
                .values()
                .any(|run| run.status == RunStatus::Running)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        for (level, message) in sequences.logs.drain(..) {
            self.push_log(PendingLog::with_target(level, "sequences", message));
        }
        self.sequences = sequences;
        if cancel {
            self.cancel_sequences();
        }
    }
}

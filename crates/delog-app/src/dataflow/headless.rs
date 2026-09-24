use std::hash::{Hash, Hasher};
use std::sync::Arc;

use delog_core::ingest::IngestSender;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::graph::{Graph, NodeKind};

use super::controller::DataFlowController;
use crate::ui::logging::LogLevel;

pub struct HeadlessFlow {
    pub controller: DataFlowController,
    pub initialized: bool,
    last_input: Option<u64>,
    last_snapshot: Option<Arc<StoreSnapshot>>,
    last_run: f64,
}

impl HeadlessFlow {
    pub fn new(graph: Graph, key: String) -> Self {
        let mut controller = DataFlowController::new(graph);
        controller.set_publication_key(key);
        Self {
            controller,
            initialized: false,
            last_input: None,
            last_snapshot: None,
            last_run: f64::NEG_INFINITY,
        }
    }

    pub fn drive(
        &mut self,
        snapshot: &Arc<StoreSnapshot>,
        sender: &IngestSender,
        live: bool,
        now: f64,
        throttle_ms: u32,
        overlap_secs: f32,
    ) -> (Vec<(LogLevel, String)>, Option<Result<(), String>>) {
        let logs = self.controller.poll(sender);
        let result = self.controller.take_publication_result();
        if result.as_ref().is_some_and(Result::is_ok) {
            self.initialized = true;
        }
        if result.is_some() {
            return (logs, result);
        }
        if !self.controller.is_evaluating()
            && (self.last_input.is_none() || (live && self.initialized))
        {
            let stamp = input_stamp(&self.controller.graph, snapshot);
            if self.last_input != Some(stamp) && now - self.last_run >= throttle_ms as f64 / 1000.0
            {
                self.last_input = Some(stamp);
                self.last_snapshot = Some(Arc::clone(snapshot));
                self.last_run = now;
                self.controller
                    .request_live(Arc::clone(snapshot), overlap_secs, true);
            }
        }
        (logs, None)
    }
}

fn input_stamp(graph: &Graph, snapshot: &StoreSnapshot) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for node in &graph.nodes {
        if let NodeKind::DataField(selector) = &node.kind {
            match delog_flow::resolve::resolve_field(snapshot, selector) {
                Ok(field) => {
                    field.field.hash(&mut hash);
                    field.multiplier.to_bits().hash(&mut hash);
                    field.unit.hash(&mut hash);
                    if let Some(source) = snapshot.source(field.source) {
                        source.entry.offset_us.hash(&mut hash);
                    }
                    if let Some(store) = snapshot
                        .topic(field.topic)
                        .and_then(|topic| topic.store.as_ref())
                    {
                        (Arc::as_ptr(store) as usize).hash(&mut hash);
                    }
                }
                Err(error) => error.hash(&mut hash),
            }
        }
    }
    hash.finish()
}

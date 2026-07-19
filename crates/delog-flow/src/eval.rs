use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use delog_core::field_view::{FieldView, array_row_as_f64};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::doc::node_kind_json;
use crate::graph::{Graph, NodeId, NodeKind};
use crate::resolve::resolve_field;
use crate::types::{Signal, SignalMeta, TimelineId, Value};

const TIMELINE_MISMATCH: &str = "Timeline mismatch: the inputs do not share the same timestamps. Add an Align node before this operation.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub node: NodeId,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvalReport {
    pub values: HashMap<NodeId, Vec<Value>>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    fingerprint: u64,
    value: Vec<Value>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Default)]
pub struct EvalCache {
    entries: HashMap<NodeId, CacheEntry>,
}

impl EvalCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn evaluate(
    graph: &Graph,
    snapshot: &StoreSnapshot,
    targets: &[NodeId],
    cancel: &AtomicBool,
    cache: &mut EvalCache,
) -> EvalReport {
    let order = match topological_order(graph, targets) {
        Ok(order) => order,
        Err(node) => {
            return EvalReport {
                values: HashMap::new(),
                diagnostics: vec![Diagnostic {
                    node,
                    message: "Graph contains a cycle.".to_owned(),
                }],
            };
        }
    };
    let mut report = EvalReport::default();
    let mut fingerprints = HashMap::new();

    for id in order {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(node) = graph.node(id) else {
            continue;
        };

        match &node.kind {
            NodeKind::DataField(selector) => {
                let resolved = match resolve_field(snapshot, selector) {
                    Ok(resolved) => resolved,
                    Err(message) => {
                        report.diagnostics.push(Diagnostic { node: id, message });
                        continue;
                    }
                };
                if resolved.is_string {
                    report.diagnostics.push(Diagnostic {
                        node: id,
                        message: format!(
                            "Field '{}' is a string field; math nodes require numeric input.",
                            selector.field
                        ),
                    });
                    continue;
                }
                let fingerprint = fingerprint(
                    &node.kind,
                    &[],
                    Some((
                        snapshot.epoch,
                        resolved.source.0,
                        resolved.topic.0,
                        resolved.field.0,
                    )),
                );
                if restore_cached(id, fingerprint, cache, &mut report) {
                    fingerprints.insert(id, fingerprint);
                    continue;
                }
                match read_field(snapshot, resolved.field, resolved.multiplier) {
                    Ok((times, values)) => {
                        let value = Value::Signal(Signal {
                            t: Arc::new(times),
                            v: Arc::new(values),
                            meta: SignalMeta {
                                timeline: TimelineId::Topic(resolved.topic),
                                unit: resolved.unit,
                            },
                        });
                        store_value(id, fingerprint, vec![value], Vec::new(), cache, &mut report);
                        fingerprints.insert(id, fingerprint);
                    }
                    Err(message) => report.diagnostics.push(Diagnostic { node: id, message }),
                }
            }
            NodeKind::Constant { value } => {
                if !value.is_finite() {
                    report.diagnostics.push(Diagnostic {
                        node: id,
                        message: "Value must be finite.".to_owned(),
                    });
                    continue;
                }
                let fingerprint = fingerprint(&node.kind, &[], None);
                if !restore_cached(id, fingerprint, cache, &mut report) {
                    store_value(
                        id,
                        fingerprint,
                        vec![Value::Scalar(*value)],
                        Vec::new(),
                        cache,
                        &mut report,
                    );
                }
                fingerprints.insert(id, fingerprint);
            }
            NodeKind::Unknown(_) => report.diagnostics.push(Diagnostic {
                node: id,
                message: "Unknown node type; this graph was saved by a newer version.".to_owned(),
            }),
            kind => {
                let ports = kind.inputs();
                let mut inputs = Vec::with_capacity(ports.len());
                let mut input_fingerprints = Vec::with_capacity(ports.len());
                let mut blocked = false;
                for (port_index, port) in ports.iter().enumerate() {
                    let Some((upstream, from_port)) = graph.incoming(id, port_index as u32) else {
                        report.diagnostics.push(Diagnostic {
                            node: id,
                            message: format!("Input {} has no connection.", port.name),
                        });
                        blocked = true;
                        continue;
                    };
                    let Some(value) = report
                        .values
                        .get(&upstream)
                        .and_then(|values| values.get(from_port as usize))
                        .cloned()
                    else {
                        blocked = true;
                        continue;
                    };
                    let Some(&upstream_fingerprint) = fingerprints.get(&upstream) else {
                        blocked = true;
                        continue;
                    };
                    inputs.push(value);
                    input_fingerprints.push(upstream_fingerprint);
                }
                if blocked {
                    continue;
                }
                if matches!(kind, NodeKind::Output(_)) {
                    continue;
                }
                if let NodeKind::ScaleOffset { multiplier, offset } = kind
                    && (!multiplier.is_finite() || !offset.is_finite())
                {
                    report.diagnostics.push(Diagnostic {
                        node: id,
                        message: "Value must be finite.".to_owned(),
                    });
                    continue;
                }
                let fingerprint = fingerprint(kind, &input_fingerprints, None);
                if restore_cached(id, fingerprint, cache, &mut report) {
                    fingerprints.insert(id, fingerprint);
                    continue;
                }
                match evaluate_kernel(kind, &inputs) {
                    Ok((value, messages)) => {
                        store_value(id, fingerprint, value, messages, cache, &mut report);
                        fingerprints.insert(id, fingerprint);
                    }
                    Err(message) => report.diagnostics.push(Diagnostic { node: id, message }),
                }
            }
        }
    }
    report
}

pub fn read_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
    multiplier: f64,
) -> Result<(Vec<i64>, Vec<f64>), String> {
    let view = FieldView::new(snapshot, field).map_err(|error| error.to_string())?;
    let Some(range) = snapshot.global_time_range() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let col = view.col_index();
    let offset = view.offset_us_for_export();
    let mut times = Vec::new();
    let mut values = Vec::new();
    for chunk in view.chunks_overlapping(range) {
        for row in 0..chunk.len() {
            let time = chunk
                .t
                .value(row)
                .checked_add(offset)
                .ok_or_else(|| "source offset overflows a data-flow timestamp".to_owned())?;
            times.push(time);
            values.push(array_row_as_f64(chunk.cols[col].as_ref(), row) * multiplier);
        }
    }
    if !times.windows(2).all(|pair| pair[0] <= pair[1]) {
        let mut order: Vec<_> = (0..times.len()).collect();
        order.sort_by_key(|&index| times[index]);
        times = order.iter().map(|&index| times[index]).collect();
        values = order.iter().map(|&index| values[index]).collect();
    }
    Ok((times, values))
}

fn evaluate_kernel(kind: &NodeKind, inputs: &[Value]) -> Result<(Vec<Value>, Vec<String>), String> {
    let (value, messages) = match kind {
        NodeKind::Add => signal_binary(inputs, |a, b| a + b, true)?,
        NodeKind::Subtract => signal_binary(inputs, |a, b| a - b, true)?,
        NodeKind::Multiply => signal_or_scalar_binary(inputs, |a, b| a * b, true)?,
        NodeKind::Divide => signal_or_scalar_binary(inputs, |a, b| a / b, true)?,
        NodeKind::ScaleOffset { multiplier, offset } => {
            let input = require_signal(inputs.first())?;
            let values = input
                .v
                .iter()
                .map(|value| value * multiplier + offset)
                .collect();
            (
                Value::Signal(Signal {
                    t: Arc::clone(&input.t),
                    v: Arc::new(values),
                    meta: input.meta.clone(),
                }),
                Vec::new(),
            )
        }
        NodeKind::Align { mode } => {
            let data = require_signal(inputs.first())?;
            let reference = require_signal(inputs.get(1))?;
            let values = delog_core::align::align_values(&data.t, &data.v, &reference.t, *mode);
            (
                Value::Signal(Signal {
                    t: Arc::clone(&reference.t),
                    v: Arc::new(values),
                    meta: SignalMeta {
                        timeline: reference.meta.timeline,
                        unit: data.meta.unit.clone(),
                    },
                }),
                Vec::new(),
            )
        }
        _ => return Err("This operation requires numeric input.".to_owned()),
    };
    Ok((vec![value], messages))
}

fn signal_binary(
    inputs: &[Value],
    operation: impl Fn(f64, f64) -> f64,
    preserve_matching_unit: bool,
) -> Result<(Value, Vec<String>), String> {
    let a = require_signal(inputs.first())?;
    let b = require_signal(inputs.get(1))?;
    require_same_timeline(a, b)?;
    let mut messages = Vec::new();
    let unit = if preserve_matching_unit && a.meta.unit == b.meta.unit {
        a.meta.unit.clone()
    } else {
        if let (Some(a_unit), Some(b_unit)) = (&a.meta.unit, &b.meta.unit)
            && a_unit != b_unit
        {
            messages.push(format!(
                "Units differ ({a_unit} vs {b_unit}); output unit cleared."
            ));
        }
        None
    };
    let values =
        a.v.iter()
            .zip(b.v.iter())
            .map(|(&a, &b)| operation(a, b))
            .collect();
    Ok((
        Value::Signal(Signal {
            t: Arc::clone(&a.t),
            v: Arc::new(values),
            meta: SignalMeta {
                timeline: a.meta.timeline,
                unit,
            },
        }),
        messages,
    ))
}

fn signal_or_scalar_binary(
    inputs: &[Value],
    operation: impl Fn(f64, f64) -> f64,
    preserve_scalar_unit: bool,
) -> Result<(Value, Vec<String>), String> {
    let a = require_signal(inputs.first())?;
    let Some(b) = inputs.get(1) else {
        return Err("This operation requires numeric input.".to_owned());
    };
    match b {
        Value::Scalar(scalar) => {
            let values = a.v.iter().map(|&value| operation(value, *scalar)).collect();
            Ok((
                Value::Signal(Signal {
                    t: Arc::clone(&a.t),
                    v: Arc::new(values),
                    meta: SignalMeta {
                        timeline: a.meta.timeline,
                        unit: preserve_scalar_unit.then(|| a.meta.unit.clone()).flatten(),
                    },
                }),
                Vec::new(),
            ))
        }
        Value::Signal(b) => {
            require_same_timeline(a, b)?;
            let values =
                a.v.iter()
                    .zip(b.v.iter())
                    .map(|(&a, &b)| operation(a, b))
                    .collect();
            Ok((
                Value::Signal(Signal {
                    t: Arc::clone(&a.t),
                    v: Arc::new(values),
                    meta: SignalMeta {
                        timeline: a.meta.timeline,
                        unit: None,
                    },
                }),
                Vec::new(),
            ))
        }
    }
}

fn require_signal(value: Option<&Value>) -> Result<&Signal, String> {
    match value {
        Some(Value::Signal(signal)) => Ok(signal),
        _ => Err("This operation requires numeric input.".to_owned()),
    }
}

fn require_same_timeline(a: &Signal, b: &Signal) -> Result<(), String> {
    if a.meta.timeline != b.meta.timeline {
        return Err(TIMELINE_MISMATCH.to_owned());
    }
    if a.t.len() != b.t.len() || a.v.len() != b.v.len() {
        return Err("Signals on the same timeline have different lengths.".to_owned());
    }
    Ok(())
}

fn fingerprint(kind: &NodeKind, inputs: &[u64], data: Option<(u64, u32, u32, u32)>) -> u64 {
    let mut hasher = DefaultHasher::new();
    node_kind_json(kind).to_string().hash(&mut hasher);
    inputs.hash(&mut hasher);
    data.hash(&mut hasher);
    hasher.finish()
}

fn restore_cached(
    id: NodeId,
    fingerprint: u64,
    cache: &EvalCache,
    report: &mut EvalReport,
) -> bool {
    let Some(entry) = cache
        .entries
        .get(&id)
        .filter(|entry| entry.fingerprint == fingerprint)
    else {
        return false;
    };
    report.values.insert(id, entry.value.clone());
    report.diagnostics.extend(
        entry
            .diagnostics
            .iter()
            .cloned()
            .map(|message| Diagnostic { node: id, message }),
    );
    true
}

fn store_value(
    id: NodeId,
    fingerprint: u64,
    value: Vec<Value>,
    diagnostics: Vec<String>,
    cache: &mut EvalCache,
    report: &mut EvalReport,
) {
    report.values.insert(id, value.clone());
    report.diagnostics.extend(
        diagnostics
            .iter()
            .cloned()
            .map(|message| Diagnostic { node: id, message }),
    );
    cache.entries.insert(
        id,
        CacheEntry {
            fingerprint,
            value,
            diagnostics,
        },
    );
}

fn topological_order(graph: &Graph, targets: &[NodeId]) -> Result<Vec<NodeId>, NodeId> {
    fn visit(
        graph: &Graph,
        id: NodeId,
        visiting: &mut HashSet<NodeId>,
        visited: &mut HashSet<NodeId>,
        order: &mut Vec<NodeId>,
    ) -> Result<(), NodeId> {
        if visited.contains(&id) || graph.node(id).is_none() {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(id);
        }
        let mut incoming: Vec<_> = graph.edges.iter().filter(|edge| edge.to == id).collect();
        incoming.sort_by_key(|edge| edge.to_port);
        for edge in incoming {
            visit(graph, edge.from, visiting, visited, order)?;
        }
        visiting.remove(&id);
        visited.insert(id);
        order.push(id);
        Ok(())
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut order = Vec::new();
    for &target in targets {
        visit(graph, target, &mut visiting, &mut visited, &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use delog_core::align::AlignMode;

    use super::*;
    use crate::command::{GraphCommand, apply};
    use crate::graph::{FieldSelector, Graph, Node, NodeId, NodeKind};
    use crate::test_util::{
        snapshot_duplicate_times, snapshot_gps_baro, snapshot_overlapping_chunks,
        snapshot_scaled_i16,
    };
    use crate::types::{Signal, Value};

    fn add_node(graph: &mut Graph, kind: NodeKind) -> NodeId {
        let id = graph.alloc_id();
        graph.insert_node(Node {
            id,
            pos: [0.0; 2],
            kind,
        });
        id
    }

    fn field(topic: &str, name: &str) -> NodeKind {
        NodeKind::DataField(FieldSelector {
            source: Some("flight".into()),
            topic: topic.into(),
            instance: (topic == "IMU").then_some(0),
            field: name.into(),
        })
    }

    fn eval_single(graph: &Graph, snapshot: &StoreSnapshot, target: NodeId) -> EvalReport {
        evaluate(
            graph,
            snapshot,
            &[target],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        )
    }

    fn signal(report: &EvalReport, id: NodeId) -> &Signal {
        signal_at(report, id, 0)
    }

    fn signal_at(report: &EvalReport, id: NodeId, port: usize) -> &Signal {
        match report.values.get(&id).and_then(|v| v.get(port)).unwrap() {
            Value::Signal(signal) => signal,
            Value::Scalar(_) => panic!("expected signal"),
        }
    }

    #[test]
    fn source_multipliers_are_applied_before_arithmetic() {
        let snapshot = snapshot_scaled_i16();
        let mut graph = Graph::new("g");
        let a = add_node(&mut graph, field("SCALED", "A"));
        let b = add_node(&mut graph, field("SCALED", "B"));
        let add = add_node(&mut graph, NodeKind::Add);
        let sub = add_node(&mut graph, NodeKind::Subtract);
        let mul = add_node(&mut graph, NodeKind::Multiply);
        let div = add_node(&mut graph, NodeKind::Divide);
        for operation in [add, sub, mul, div] {
            graph.connect(a, 0, operation, 0).unwrap();
            graph.connect(b, 0, operation, 1).unwrap();
        }
        let report = evaluate(
            &graph,
            &snapshot,
            &[a, b, add, sub, mul, div],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        assert_eq!(signal(&report, a).v.as_slice(), &[1.0, 2.0]);
        assert_eq!(signal(&report, b).v.as_slice(), &[0.5, 1.0]);
        assert_eq!(signal(&report, add).v.as_slice(), &[1.5, 3.0]);
        assert_eq!(signal(&report, sub).v.as_slice(), &[0.5, 1.0]);
        assert_eq!(signal(&report, mul).v.as_slice(), &[0.5, 2.0]);
        assert_eq!(signal(&report, div).v.as_slice(), &[2.0, 2.0]);
    }

    #[test]
    fn scale_offset_and_arithmetic_compute_elementwise() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let x = add_node(&mut graph, field("IMU", "AccX"));
        let y = add_node(&mut graph, field("IMU", "AccY"));
        let scale = add_node(
            &mut graph,
            NodeKind::ScaleOffset {
                multiplier: 2.0,
                offset: 1.0,
            },
        );
        let add = add_node(&mut graph, NodeKind::Add);
        graph.connect(x, 0, scale, 0).unwrap();
        graph.connect(x, 0, add, 0).unwrap();
        graph.connect(y, 0, add, 1).unwrap();

        let report = evaluate(
            &graph,
            &snapshot,
            &[scale, add],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let scaled = &signal(&report, scale).v;
        assert_eq!(scaled[0..2], [3.0, 5.0]);
        assert!(scaled[2].is_nan());
        let sum = &signal(&report, add).v;
        assert_eq!(sum[0..2], [11.0, 22.0]);
        assert!(sum[2].is_nan());
    }

    #[test]
    fn divide_by_zero_follows_ieee() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let data = add_node(&mut graph, field("GPS", "Alt"));
        let zero = add_node(&mut graph, NodeKind::Constant { value: 0.0 });
        let divide = add_node(&mut graph, NodeKind::Divide);
        graph.connect(data, 0, divide, 0).unwrap();
        graph.connect(zero, 0, divide, 1).unwrap();

        let report = eval_single(&graph, &snapshot, divide);
        let values = &signal(&report, divide).v;
        assert_eq!(values[0], f64::INFINITY);
        assert_eq!(values[1], f64::NEG_INFINITY);
        assert!(values[2].is_nan());
    }

    #[test]
    fn mismatched_timelines_are_rejected_with_align_hint() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, field("GPS", "Alt"));
        let baro = add_node(&mut graph, field("BARO", "Alt"));
        let add = add_node(&mut graph, NodeKind::Add);
        graph.connect(gps, 0, add, 0).unwrap();
        graph.connect(baro, 0, add, 1).unwrap();

        let report = eval_single(&graph, &snapshot, add);
        assert!(!report.values.contains_key(&add));
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("Add an Align node"))
        );
    }

    #[test]
    fn align_adopts_reference_timeline_and_unit_survives() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, field("GPS", "Alt"));
        let baro = add_node(&mut graph, field("BARO", "Alt"));
        let align = add_node(
            &mut graph,
            NodeKind::Align {
                mode: AlignMode::Prev,
            },
        );
        let add = add_node(&mut graph, NodeKind::Add);
        graph.connect(gps, 0, align, 0).unwrap();
        graph.connect(baro, 0, align, 1).unwrap();
        graph.connect(align, 0, add, 0).unwrap();
        graph.connect(baro, 0, add, 1).unwrap();

        let report = eval_single(&graph, &snapshot, add);
        let aligned = signal(&report, align);
        assert_eq!(aligned.meta.timeline, signal(&report, baro).meta.timeline);
        assert_eq!(aligned.meta.unit.as_deref(), Some("m"));
        assert_eq!(signal(&report, add).v.len(), 2);
    }

    #[test]
    fn missing_input_and_unreachable_nodes() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, field("GPS", "Alt"));
        let add = add_node(&mut graph, NodeKind::Add);
        let invalid = add_node(&mut graph, field("MISSING", "Nope"));
        graph.connect(gps, 0, add, 0).unwrap();

        let report = eval_single(&graph, &snapshot, add);
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.node == add && diagnostic.message == "Input B has no connection."
        }));
        assert!(
            report
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.node != invalid)
        );
    }

    #[test]
    fn unit_rules() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let x = add_node(&mut graph, field("IMU", "AccX"));
        let y = add_node(&mut graph, field("IMU", "AccY"));
        let other = add_node(&mut graph, field("IMU", "Other"));
        let scalar = add_node(&mut graph, NodeKind::Constant { value: 2.0 });
        let same = add_node(&mut graph, NodeKind::Add);
        let different = add_node(&mut graph, NodeKind::Add);
        let multiply = add_node(&mut graph, NodeKind::Multiply);
        graph.connect(x, 0, same, 0).unwrap();
        graph.connect(y, 0, same, 1).unwrap();
        graph.connect(x, 0, different, 0).unwrap();
        graph.connect(other, 0, different, 1).unwrap();
        graph.connect(x, 0, multiply, 0).unwrap();
        graph.connect(scalar, 0, multiply, 1).unwrap();

        let report = evaluate(
            &graph,
            &snapshot,
            &[same, different, multiply],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        assert_eq!(signal(&report, same).meta.unit.as_deref(), Some("m/s^2"));
        assert_eq!(signal(&report, different).meta.unit, None);
        assert_eq!(
            signal(&report, multiply).meta.unit.as_deref(),
            Some("m/s^2")
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("Units differ"))
        );
    }

    #[test]
    fn cache_reuses_untouched_branches() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let data = add_node(&mut graph, field("GPS", "Alt"));
        let scale = add_node(
            &mut graph,
            NodeKind::ScaleOffset {
                multiplier: 2.0,
                offset: 0.0,
            },
        );
        graph.connect(data, 0, scale, 0).unwrap();
        let mut cache = EvalCache::default();
        let first = evaluate(
            &graph,
            &snapshot,
            &[scale],
            &AtomicBool::new(false),
            &mut cache,
        );
        assert_eq!(signal(&first, scale).v[0], 2.0);
        assert_eq!(cache.len(), 2);

        apply(
            &mut graph,
            GraphCommand::SetKind {
                id: scale,
                kind: NodeKind::ScaleOffset {
                    multiplier: 3.0,
                    offset: 0.0,
                },
            },
        )
        .unwrap();
        let second = evaluate(
            &graph,
            &snapshot,
            &[scale],
            &AtomicBool::new(false),
            &mut cache,
        );
        assert_eq!(signal(&second, scale).v[0], 3.0);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn duplicate_timestamps_use_last_sample_via_align() {
        let snapshot = snapshot_duplicate_times();
        let mut graph = Graph::new("g");
        let source = add_node(&mut graph, field("SRC", "Value"));
        let base = add_node(&mut graph, field("BASE", "Value"));
        let align = add_node(
            &mut graph,
            NodeKind::Align {
                mode: AlignMode::Prev,
            },
        );
        graph.connect(source, 0, align, 0).unwrap();
        graph.connect(base, 0, align, 1).unwrap();

        let report = eval_single(&graph, &snapshot, align);
        assert_eq!(signal(&report, align).v.as_slice(), &[22.0]);
    }

    #[test]
    fn overlapping_source_chunks_are_sorted_before_alignment() {
        let snapshot = snapshot_overlapping_chunks();
        let mut graph = Graph::new("g");
        let source = add_node(&mut graph, field("SRC", "Value"));
        let base = add_node(&mut graph, field("BASE", "Value"));
        let align = add_node(
            &mut graph,
            NodeKind::Align {
                mode: AlignMode::Prev,
            },
        );
        graph.connect(source, 0, align, 0).unwrap();
        graph.connect(base, 0, align, 1).unwrap();

        let report = eval_single(&graph, &snapshot, align);

        assert_eq!(signal(&report, align).v.as_slice(), &[1.0, 33.0, 33.0, 4.0]);
    }
}

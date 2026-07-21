use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::graph::NodeId;
use crate::types::{Signal, SignalMeta, TimelineId, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptInputSpec {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptOutputSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptSpec {
    pub name: String,
    pub inputs: Vec<ScriptInputSpec>,
    pub outputs: Vec<ScriptOutputSpec>,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct ScriptInput {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct ScriptRequest {
    pub node_label: String,
    pub code: String,
    pub inputs: Vec<ScriptInput>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptOutput {
    pub times: Option<Vec<i64>>,
    pub values: Vec<f64>,
    pub unit: Option<String>,
}

/// Implemented outside `delog-flow` (by the scripting engine). Kept dependency-free here so
/// the evaluator is testable with a fake host and never links Python.
pub trait ScriptNodeHost: Send {
    fn eval(&self, request: ScriptRequest, cancel: &AtomicBool) -> Result<Vec<ScriptOutput>, String>;
}

pub const HOST_UNAVAILABLE: &str = "Python scripting is not available in this build.";

pub fn validate_spec(spec: &ScriptSpec) -> Result<(), String> {
    if spec.outputs.is_empty() {
        return Err("Script node must declare at least one output.".to_owned());
    }
    if spec.code.trim().is_empty() {
        return Err("Script code is empty.".to_owned());
    }
    let mut seen = HashSet::new();
    for input in &spec.inputs {
        if !is_valid_identifier(&input.name) {
            return Err(format!(
                "Input port name '{}' is not a valid Python identifier.",
                input.name
            ));
        }
        if !seen.insert(input.name.as_str()) {
            return Err(format!("Duplicate input port name '{}'.", input.name));
        }
    }
    let mut seen = HashSet::new();
    for output in &spec.outputs {
        if !is_valid_identifier(&output.name) {
            return Err(format!(
                "Output port name '{}' is not a valid Python identifier.",
                output.name
            ));
        }
        if !seen.insert(output.name.as_str()) {
            return Err(format!("Duplicate output port name '{}'.", output.name));
        }
    }
    Ok(())
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn request_for(node_label: &str, spec: &ScriptSpec, inputs: &[Value]) -> ScriptRequest {
    let inputs = spec
        .inputs
        .iter()
        .zip(inputs)
        .map(|(port, value)| ScriptInput {
            name: port.name.clone(),
            value: value.clone(),
        })
        .collect();
    ScriptRequest {
        node_label: node_label.to_owned(),
        code: spec.code.clone(),
        inputs,
        outputs: spec.outputs.iter().map(|output| output.name.clone()).collect(),
    }
}

/// Assigns `TimelineId`s per output in declared order (rules 1-4 of the design doc), then
/// applies unit precedence (port override, else the script-returned unit).
pub fn bind_outputs(
    node: NodeId,
    spec: &ScriptSpec,
    inputs: &[Value],
    raw: Vec<ScriptOutput>,
) -> Result<Vec<Value>, String> {
    if raw.len() != spec.outputs.len() {
        return Err(format!(
            "Script returned {} output(s), expected {}.",
            raw.len(),
            spec.outputs.len()
        ));
    }
    let signal_inputs: Vec<&Signal> = inputs
        .iter()
        .filter_map(|value| match value {
            Value::Signal(signal) => Some(signal),
            Value::Scalar(_) => None,
        })
        .collect();
    // Rule 1 precondition: every wired signal input must share one TimelineId.
    let shared_timeline = {
        let mut timelines = signal_inputs.iter().map(|signal| signal.meta.timeline);
        match timelines.next() {
            Some(first) if timelines.all(|other| other == first) => Some(first),
            _ => None,
        }
    };

    let mut bound: Vec<Value> = Vec::with_capacity(raw.len());
    for (output_spec, raw_output) in spec.outputs.iter().zip(raw) {
        if let Some(times) = &raw_output.times {
            if times.len() != raw_output.values.len() {
                return Err(format!(
                    "Output '{}': times and values must have the same length.",
                    output_spec.name
                ));
            }
            if !times.windows(2).all(|pair| pair[0] <= pair[1]) {
                return Err(format!(
                    "Output '{}': times must be sorted ascending.",
                    output_spec.name
                ));
            }
        }

        let (timeline, t) = match &raw_output.times {
            // Rule 1: bare values inherit the inputs' shared timeline.
            None => {
                let Some(timeline) = shared_timeline else {
                    return Err(format!(
                        "Output '{}' must return explicit times because the inputs are on different timelines.",
                        output_spec.name
                    ));
                };
                let signal = signal_inputs
                    .iter()
                    .find(|signal| signal.meta.timeline == timeline)
                    .expect("shared_timeline came from a signal input");
                if signal.v.len() != raw_output.values.len() {
                    return Err(format!(
                        "Output '{}': values length does not match the shared input timeline.",
                        output_spec.name
                    ));
                }
                (timeline, Arc::clone(&signal.t))
            }
            Some(times) => {
                // Rule 2: explicit times matching a wired input reuse its timeline.
                if let Some(signal) = signal_inputs.iter().find(|signal| signal.t.as_ref() == times) {
                    (signal.meta.timeline, Arc::clone(&signal.t))
                // Rule 3: explicit times matching an earlier output of this node share its id.
                } else if let Some((timeline, t)) = bound.iter().find_map(|value| match value {
                    Value::Signal(signal) if signal.t.as_ref() == times => {
                        Some((signal.meta.timeline, Arc::clone(&signal.t)))
                    }
                    _ => None,
                }) {
                    (timeline, t)
                // Rule 4: otherwise, a fresh per-output timeline.
                } else {
                    (
                        TimelineId::NodeOutput(node, bound.len() as u16),
                        Arc::new(times.clone()),
                    )
                }
            }
        };

        let unit = output_spec.unit.clone().or(raw_output.unit);
        bound.push(Value::Signal(Signal {
            t,
            v: Arc::new(raw_output.values),
            meta: SignalMeta { timeline, unit },
        }));
    }
    Ok(bound)
}

#[cfg(all(test, feature = "scripting"))]
mod script_tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use delog_core::snapshot::StoreSnapshot;

    use super::*;
    use crate::command::{GraphCommand, apply};
    use crate::doc::{from_json, to_json};
    use crate::eval::{EvalCache, evaluate};
    use crate::graph::{FieldSelector, Graph, Node, NodeId, NodeKind};
    use crate::test_util::snapshot_gps_baro;

    struct FakeHost {
        responses: Mutex<VecDeque<Result<Vec<ScriptOutput>, String>>>,
        requests: Mutex<Vec<ScriptRequest>>,
    }

    impl FakeHost {
        fn new(responses: Vec<Result<Vec<ScriptOutput>, String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl ScriptNodeHost for FakeHost {
        fn eval(&self, request: ScriptRequest, _cancel: &AtomicBool) -> Result<Vec<ScriptOutput>, String> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("fake host ran out of scripted responses")
        }
    }

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

    fn script(name: &str, inputs: &[&str], outputs: &[(&str, Option<&str>)], code: &str) -> NodeKind {
        NodeKind::Script(ScriptSpec {
            name: name.to_owned(),
            inputs: inputs
                .iter()
                .map(|name| ScriptInputSpec {
                    name: (*name).to_owned(),
                })
                .collect(),
            outputs: outputs
                .iter()
                .map(|(name, unit)| ScriptOutputSpec {
                    name: (*name).to_owned(),
                    unit: unit.map(str::to_owned),
                })
                .collect(),
            code: code.to_owned(),
        })
    }

    fn timeline_of(value: &Value) -> TimelineId {
        match value {
            Value::Signal(signal) => signal.meta.timeline,
            Value::Scalar(_) => panic!("expected signal"),
        }
    }

    #[test]
    fn bare_values_inherit_a_shared_input_timeline() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let x = add_node(&mut graph, field("IMU", "AccX"));
        let y = add_node(&mut graph, field("IMU", "AccY"));
        let node = add_node(&mut graph, script("Sum", &["a", "b"], &[("out", None)], "code"));
        graph.connect(x, 0, node, 0).unwrap();
        graph.connect(y, 0, node, 1).unwrap();

        let host = FakeHost::new(vec![Ok(vec![ScriptOutput {
            times: None,
            values: vec![11.0, 22.0, f64::NAN],
            unit: None,
        }])]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        let expected = timeline_of(&report.values[&x][0]);
        assert_eq!(timeline_of(&report.values[&node][0]), expected);
        match &report.values[&node][0] {
            Value::Signal(signal) => {
                assert_eq!(signal.v[0], 11.0);
                assert_eq!(signal.v[1], 22.0);
            }
            Value::Scalar(_) => panic!("expected signal"),
        }
    }

    #[test]
    fn bare_values_with_mixed_input_timelines_is_a_diagnostic() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, field("GPS", "Alt"));
        let baro = add_node(&mut graph, field("BARO", "Alt"));
        let node = add_node(&mut graph, script("Mix", &["a", "b"], &[("out", None)], "code"));
        graph.connect(gps, 0, node, 0).unwrap();
        graph.connect(baro, 0, node, 1).unwrap();

        let host = FakeHost::new(vec![Ok(vec![ScriptOutput {
            times: None,
            values: vec![1.0, 2.0],
            unit: None,
        }])]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        assert!(!report.values.contains_key(&node));
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.node == node
                    && diagnostic.message.contains("different timelines"))
        );
    }

    #[test]
    fn explicit_times_matching_an_input_reuse_its_timeline() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, field("GPS", "Alt"));
        let node = add_node(&mut graph, script("Half", &["a"], &[("out", None)], "code"));
        graph.connect(gps, 0, node, 0).unwrap();

        let host = FakeHost::new(vec![Ok(vec![ScriptOutput {
            times: Some(vec![100, 200, 300]),
            values: vec![0.5, -0.5, 0.0],
            unit: None,
        }])]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        let expected = timeline_of(&report.values[&gps][0]);
        assert_eq!(timeline_of(&report.values[&node][0]), expected);
    }

    #[test]
    fn matching_times_across_two_outputs_share_one_timeline() {
        let snapshot = StoreSnapshot::empty();
        let mut graph = Graph::new("g");
        let node = add_node(&mut graph, script("Solo", &[], &[("a", None), ("b", None)], "code"));

        let host = FakeHost::new(vec![Ok(vec![
            ScriptOutput {
                times: Some(vec![5, 6, 7]),
                values: vec![1.0, 2.0, 3.0],
                unit: None,
            },
            ScriptOutput {
                times: Some(vec![5, 6, 7]),
                values: vec![4.0, 5.0, 6.0],
                unit: None,
            },
        ])]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        let values = &report.values[&node];
        assert_eq!(timeline_of(&values[0]), TimelineId::NodeOutput(node, 0));
        assert_eq!(timeline_of(&values[1]), timeline_of(&values[0]));
    }

    #[test]
    fn novel_times_get_a_node_output_timeline() {
        let snapshot = StoreSnapshot::empty();
        let mut graph = Graph::new("g");
        let node = add_node(&mut graph, script("Solo", &[], &[("out", None)], "code"));

        let host = FakeHost::new(vec![Ok(vec![ScriptOutput {
            times: Some(vec![10, 20, 30]),
            values: vec![1.0, 2.0, 3.0],
            unit: None,
        }])]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        assert_eq!(
            timeline_of(&report.values[&node][0]),
            TimelineId::NodeOutput(node, 0)
        );
    }

    #[test]
    fn port_unit_override_beats_script_unit() {
        let snapshot = StoreSnapshot::empty();
        let mut graph = Graph::new("g");
        let node = add_node(
            &mut graph,
            script("Solo", &[], &[("out", Some("m/s^2"))], "code"),
        );

        let host = FakeHost::new(vec![Ok(vec![ScriptOutput {
            times: Some(vec![1, 2, 3]),
            values: vec![1.0, 2.0, 3.0],
            unit: Some("ft/s".to_owned()),
        }])]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        match &report.values[&node][0] {
            Value::Signal(signal) => assert_eq!(signal.meta.unit.as_deref(), Some("m/s^2")),
            Value::Scalar(_) => panic!("expected signal"),
        }
    }

    #[test]
    fn host_error_becomes_node_diagnostic() {
        let snapshot = StoreSnapshot::empty();
        let mut graph = Graph::new("g");
        let node = add_node(&mut graph, script("Solo", &[], &[("out", None)], "code"));

        let host = FakeHost::new(vec![Err("boom".to_owned())]);
        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            Some(&host),
        );

        assert!(!report.values.contains_key(&node));
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.node == node && diagnostic.message == "boom")
        );
    }

    #[test]
    fn missing_host_reports_unavailable() {
        let snapshot = StoreSnapshot::empty();
        let mut graph = Graph::new("g");
        let node = add_node(&mut graph, script("Solo", &[], &[("out", None)], "code"));

        let report = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
            None,
        );

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.node == node && diagnostic.message == HOST_UNAVAILABLE)
        );
    }

    #[test]
    fn code_edit_invalidates_cache_but_upstream_stays_cached() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, field("GPS", "Alt"));
        let node = add_node(&mut graph, script("Solo", &["a"], &[("out", None)], "code v1"));
        graph.connect(gps, 0, node, 0).unwrap();

        let host = FakeHost::new(vec![
            Ok(vec![ScriptOutput {
                times: Some(vec![100, 200, 300]),
                values: vec![1.0, 2.0, 3.0],
                unit: None,
            }]),
            Ok(vec![ScriptOutput {
                times: Some(vec![100, 200, 300]),
                values: vec![9.0, 8.0, 7.0],
                unit: None,
            }]),
        ]);
        let mut cache = EvalCache::default();

        let first = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut cache,
            Some(&host),
        );
        let _second = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut cache,
            Some(&host),
        );
        assert_eq!(host.call_count(), 1, "unchanged code should hit the node's cache");

        let gps_before = match &first.values[&gps][0] {
            Value::Signal(signal) => Arc::clone(&signal.v),
            Value::Scalar(_) => panic!("expected signal"),
        };

        apply(
            &mut graph,
            GraphCommand::SetKind {
                id: node,
                kind: script("Solo", &["a"], &[("out", None)], "code v2"),
            },
        )
        .unwrap();

        let third = evaluate(
            &graph,
            &snapshot,
            &[node],
            &AtomicBool::new(false),
            &mut cache,
            Some(&host),
        );
        assert_eq!(host.call_count(), 2, "edited code should invalidate this node");

        let gps_after = match &third.values[&gps][0] {
            Value::Signal(signal) => Arc::clone(&signal.v),
            Value::Scalar(_) => panic!("expected signal"),
        };
        assert!(
            Arc::ptr_eq(&gps_before, &gps_after),
            "upstream DataField must stay cached across the code edit"
        );
    }

    #[test]
    fn unsorted_or_wrong_length_times_are_rejected() {
        let snapshot = StoreSnapshot::empty();
        for output in [
            ScriptOutput {
                times: Some(vec![1, 2]),
                values: vec![1.0, 2.0, 3.0],
                unit: None,
            },
            ScriptOutput {
                times: Some(vec![3, 1, 2]),
                values: vec![1.0, 2.0, 3.0],
                unit: None,
            },
        ] {
            let mut graph = Graph::new("g");
            let node = add_node(&mut graph, script("Solo", &[], &[("out", None)], "code"));
            let host = FakeHost::new(vec![Ok(vec![output])]);
            let report = evaluate(
                &graph,
                &snapshot,
                &[node],
                &AtomicBool::new(false),
                &mut EvalCache::default(),
                Some(&host),
            );
            assert!(!report.values.contains_key(&node));
            assert!(report.diagnostics.iter().any(|diagnostic| diagnostic.node == node));
        }
    }

    #[test]
    fn script_tag_round_trips_through_doc() {
        let mut graph = Graph::new("g");
        let source = add_node(&mut graph, field("GPS", "Alt"));
        let node = add_node(
            &mut graph,
            script(
                "Double",
                &["a"],
                &[("out", Some("m/s"))],
                "def flow(inputs):\n    return {\"out\": inputs.a.v * 2}\n",
            ),
        );
        graph.connect(source, 0, node, 0).unwrap();

        let doc = to_json(&graph);
        assert_eq!(doc["nodes"][1]["type"], "script");
        let restored = from_json(&doc).unwrap();
        assert_eq!(restored, graph);
    }

    #[test]
    fn duplicate_or_invalid_port_names_are_diagnostics() {
        let snapshot = StoreSnapshot::empty();
        for kind in [
            script("Bad", &[], &[("1bad", None)], "code"),
            script("Dup", &[], &[("a", None), ("a", None)], "code"),
        ] {
            let mut graph = Graph::new("g");
            let node = add_node(&mut graph, kind);
            let host = FakeHost::new(Vec::new());
            let report = evaluate(
                &graph,
                &snapshot,
                &[node],
                &AtomicBool::new(false),
                &mut EvalCache::default(),
                Some(&host),
            );
            assert!(!report.values.contains_key(&node));
            assert!(report.diagnostics.iter().any(|diagnostic| diagnostic.node == node));
            assert_eq!(host.call_count(), 0, "invalid spec must not reach the host");
        }
    }

    #[test]
    fn insert_and_remove_script_output_remap_from_ports_with_undo() {
        let mut graph = Graph::new("g");
        let source = add_node(
            &mut graph,
            script("Source", &[], &[("first", None), ("second", None)], "code"),
        );
        let consumer_a = add_node(&mut graph, NodeKind::Add);
        let consumer_b = add_node(&mut graph, NodeKind::Add);
        graph.connect(source, 0, consumer_a, 0).unwrap();
        graph.connect(source, 1, consumer_b, 0).unwrap();
        let original = graph.clone();

        let inverse = apply(
            &mut graph,
            GraphCommand::RemoveScriptOutput {
                id: source,
                index: 0,
            },
        )
        .unwrap();

        assert_eq!(graph.incoming(consumer_b, 0), Some((source, 0)));
        assert_eq!(graph.edges.len(), 1);
        apply(&mut graph, inverse).unwrap();
        assert_eq!(graph, original);
    }

    #[test]
    fn set_kind_dropping_an_output_port_drops_and_restores_outgoing_edges() {
        let mut graph = Graph::new("g");
        let source = add_node(
            &mut graph,
            script("Source", &[], &[("first", None), ("second", None)], "code"),
        );
        let consumer_a = add_node(&mut graph, NodeKind::Add);
        let consumer_b = add_node(&mut graph, NodeKind::Add);
        graph.connect(source, 0, consumer_a, 0).unwrap();
        graph.connect(source, 1, consumer_b, 0).unwrap();
        let original = graph.clone();

        let inverse = apply(
            &mut graph,
            GraphCommand::SetKind {
                id: source,
                kind: NodeKind::Add,
            },
        )
        .unwrap();

        assert!(graph.edges.iter().all(|edge| edge.from != source));
        apply(&mut graph, inverse).unwrap();
        assert_eq!(graph, original);
    }
}

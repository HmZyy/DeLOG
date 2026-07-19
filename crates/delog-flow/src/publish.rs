use std::collections::HashSet;

use delog_core::derived::{PendingField, PendingTopic};

use crate::eval::{Diagnostic, EvalReport};
use crate::graph::{Graph, NodeId, NodeKind};
use crate::types::{TimelineId, Value};

pub fn source_key(graph_name: &str) -> String {
    format!("dataflow:{graph_name}")
}

pub fn build_outputs(
    graph: &Graph,
    report: &EvalReport,
) -> Result<Vec<PendingTopic>, Vec<Diagnostic>> {
    let outputs: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|node| match &node.kind {
            NodeKind::Output(spec) => Some((node.id, spec)),
            _ => None,
        })
        .collect();
    if outputs.is_empty() {
        return Err(vec![Diagnostic {
            node: NodeId(0),
            message: "Connect a Derived Topic Output node to publish.".to_owned(),
        }]);
    }

    let mut errors = Vec::new();
    let mut topics = Vec::new();
    let mut topic_names = HashSet::new();
    let mut total_fields = 0;

    for (node_id, spec) in outputs.iter().copied() {
        total_fields += spec.fields.len();
        let mut valid = true;
        if spec.topic.trim().is_empty() {
            errors.push(Diagnostic {
                node: node_id,
                message: "Output topic name must not be empty.".to_owned(),
            });
            valid = false;
        } else if !topic_names.insert(spec.topic.as_str()) {
            errors.push(Diagnostic {
                node: node_id,
                message: "Output topic names must be unique.".to_owned(),
            });
            valid = false;
        }

        let mut field_names = HashSet::new();
        if spec
            .fields
            .iter()
            .any(|field| field.name.trim().is_empty() || !field_names.insert(field.name.as_str()))
        {
            errors.push(Diagnostic {
                node: node_id,
                message: "Output field names must be unique.".to_owned(),
            });
            valid = false;
        }

        let mut timeline: Option<TimelineId> = None;
        let mut times = None;
        let mut pending_fields = Vec::with_capacity(spec.fields.len());
        for (port, field) in spec.fields.iter().enumerate() {
            let Some((upstream, from_port)) = graph.incoming(node_id, port as u32) else {
                errors.push(Diagnostic {
                    node: node_id,
                    message: format!("Input {} has no connection.", field.name),
                });
                valid = false;
                continue;
            };
            let Some(Value::Signal(signal)) = report
                .values
                .get(&upstream)
                .and_then(|values| values.get(from_port as usize))
            else {
                errors.push(Diagnostic {
                    node: node_id,
                    message: "Upstream node has errors.".to_owned(),
                });
                valid = false;
                continue;
            };
            if timeline.is_some_and(|timeline| timeline != signal.meta.timeline) {
                errors.push(Diagnostic {
                    node: node_id,
                    message: "All fields of one output topic must share the same timeline. Align the inputs first."
                        .to_owned(),
                });
                valid = false;
            } else if timeline.is_none() {
                timeline = Some(signal.meta.timeline);
                times = Some((*signal.t).clone());
            }
            pending_fields.push(PendingField::numeric(
                field.name.clone(),
                (*signal.v).clone(),
                field.unit.clone().or_else(|| signal.meta.unit.clone()),
            ));
        }

        if valid && !pending_fields.is_empty() {
            let mut topic = PendingTopic::new(spec.topic.clone(), times.unwrap_or_default());
            for field in pending_fields {
                if let Err(message) = topic.add_field(field) {
                    errors.push(Diagnostic {
                        node: node_id,
                        message,
                    });
                    valid = false;
                }
            }
            if valid {
                topics.push(topic);
            }
        }
    }

    if total_fields == 0 {
        errors.push(Diagnostic {
            node: outputs[0].0,
            message: "Connect a Derived Topic Output node to publish.".to_owned(),
        });
    }
    if errors.is_empty() {
        Ok(topics)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use delog_core::derived::{PendingColumn, emit_prepared_topics, prepare_topics};
    use delog_core::diagnostics::Diag;
    use delog_core::identity::SourceId;
    use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};

    use super::*;
    use crate::command::{GraphCommand, apply};
    use crate::eval::{EvalCache, evaluate};
    use crate::graph::{FieldSelector, Graph, Node, NodeId, NodeKind, OutputFieldSpec, OutputSpec};
    use crate::test_util::{snapshot_gps_baro, snapshot_scaled_i16};

    struct RecordingSink {
        opened: Vec<(String, SourceKind)>,
        batches: usize,
        closed: Vec<SourceId>,
    }

    impl IngestSink for RecordingSink {
        fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId {
            self.opened.push((key.into(), kind));
            SourceId(42)
        }

        fn submit(&mut self, _batch: ParsedBatch) {
            self.batches += 1;
        }

        fn diagnostic(&mut self, _diagnostic: Diag) {}

        fn progress(&mut self, _source: SourceId, _fraction: f32) {}

        fn close_source(&mut self, source: SourceId, _summary: ParseSummary) {
            self.closed.push(source);
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

    fn data(topic: &str) -> NodeKind {
        NodeKind::DataField(FieldSelector {
            source: Some("flight".into()),
            topic: topic.into(),
            instance: None,
            field: "Alt".into(),
        })
    }

    fn output(topic: &str, fields: &[(&str, Option<&str>)]) -> NodeKind {
        NodeKind::Output(OutputSpec {
            topic: topic.into(),
            fields: fields
                .iter()
                .map(|(name, unit)| OutputFieldSpec {
                    name: (*name).into(),
                    unit: unit.map(str::to_owned),
                })
                .collect(),
        })
    }

    #[test]
    fn publish_produces_one_derived_source_with_all_topics() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, data("GPS"));
        let scale = add_node(
            &mut graph,
            NodeKind::ScaleOffset {
                multiplier: 2.0,
                offset: 0.0,
            },
        );
        let out = add_node(&mut graph, output("alt_scaled", &[("alt", None)]));
        graph.connect(gps, 0, scale, 0).unwrap();
        graph.connect(scale, 0, out, 0).unwrap();
        let report = evaluate(
            &graph,
            &snapshot,
            &[out],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let topics = build_outputs(&graph, &report).unwrap();
        assert_eq!(topics.len(), 1);

        let mut sink = RecordingSink {
            opened: Vec::new(),
            batches: 0,
            closed: Vec::new(),
        };
        let prepared = prepare_topics(&topics).unwrap();
        emit_prepared_topics(&mut sink, &source_key("g"), prepared);
        assert_eq!(
            sink.opened,
            vec![("dataflow:g".to_owned(), SourceKind::Derived)]
        );
        assert_eq!(sink.batches, 1);
        assert_eq!(sink.closed, vec![SourceId(42)]);
    }

    #[test]
    fn published_scaled_source_uses_normalized_values_and_unit_multiplier() {
        let snapshot = snapshot_scaled_i16();
        let mut graph = Graph::new("g");
        let data = add_node(
            &mut graph,
            NodeKind::DataField(FieldSelector {
                source: Some("flight".into()),
                topic: "SCALED".into(),
                instance: None,
                field: "A".into(),
            }),
        );
        let out = add_node(&mut graph, output("scaled", &[("value", None)]));
        graph.connect(data, 0, out, 0).unwrap();
        let report = evaluate(
            &graph,
            &snapshot,
            &[out],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let topics = build_outputs(&graph, &report).unwrap();
        assert!(matches!(
            &topics[0].fields[0].values,
            PendingColumn::F64(values) if values == &[1.0, 2.0]
        ));
        let batches = prepare_topics(&topics).unwrap().into_batches(SourceId(42));
        assert_eq!(batches[0].schema.field(0).unwrap().multiplier, 1.0);
    }

    #[test]
    fn mixed_timelines_in_one_output_block_publication() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, data("GPS"));
        let baro = add_node(&mut graph, data("BARO"));
        let out = add_node(
            &mut graph,
            output("mixed", &[("gps", None), ("baro", None)]),
        );
        graph.connect(gps, 0, out, 0).unwrap();
        graph.connect(baro, 0, out, 1).unwrap();
        let report = evaluate(
            &graph,
            &snapshot,
            &[out],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let errors = build_outputs(&graph, &report).err().unwrap();
        assert!(
            errors
                .iter()
                .any(|diagnostic| diagnostic.message.contains("share the same timeline"))
        );
    }

    #[test]
    fn duplicate_field_names_block_publication() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, data("GPS"));
        let out = add_node(&mut graph, output("duplicate", &[("a", None), ("a", None)]));
        graph.connect(gps, 0, out, 0).unwrap();
        graph.connect(gps, 0, out, 1).unwrap();
        let report = evaluate(
            &graph,
            &snapshot,
            &[out],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let errors = build_outputs(&graph, &report).err().unwrap();
        assert!(
            errors
                .iter()
                .any(|diagnostic| diagnostic.message.contains("must be unique"))
        );
    }

    #[test]
    fn unit_override_wins() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, data("GPS"));
        let out = add_node(&mut graph, output("altitude", &[("alt", Some("ft"))]));
        graph.connect(gps, 0, out, 0).unwrap();
        let report = evaluate(
            &graph,
            &snapshot,
            &[out],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let topics = build_outputs(&graph, &report).unwrap();
        assert_eq!(topics[0].fields[0].unit.as_deref(), Some("ft"));
        assert!(matches!(topics[0].fields[0].values, PendingColumn::F64(_)));
    }

    #[test]
    fn removing_first_connected_output_field_keeps_second_signal_bound_to_second_name() {
        let snapshot = snapshot_gps_baro();
        let mut graph = Graph::new("g");
        let gps = add_node(&mut graph, data("GPS"));
        let baro = add_node(&mut graph, data("BARO"));
        let output = add_node(
            &mut graph,
            output("altitude", &[("gps", None), ("baro", None)]),
        );
        graph.connect(gps, 0, output, 0).unwrap();
        graph.connect(baro, 0, output, 1).unwrap();

        apply(
            &mut graph,
            GraphCommand::RemoveOutputField {
                id: output,
                index: 0,
            },
        )
        .unwrap();
        let report = evaluate(
            &graph,
            &snapshot,
            &[output],
            &AtomicBool::new(false),
            &mut EvalCache::default(),
        );
        let topics = build_outputs(&graph, &report).unwrap();

        assert_eq!(topics[0].fields[0].name, "baro");
        assert!(matches!(
            &topics[0].fields[0].values,
            PendingColumn::F64(values) if values == &[10.0, 20.0]
        ));
    }
}

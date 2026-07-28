use std::collections::HashSet;
use std::fmt;

use serde_json::{Map, Value, json};

use crate::graph::{Edge, Graph, Node, NodeKind, OutputSpec, Viewport};

pub const DATAFLOW_VERSION: u32 = 2;

pub(crate) fn required_version(graph: &Graph) -> u32 {
    if graph.edges.iter().any(|edge| edge.from_port != 0) {
        2
    } else {
        1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocError {
    MissingVersion,
    UnsupportedVersion(u32),
    Invalid(String),
}

impl fmt::Display for DocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVersion => formatter.write_str("missing delog_dataflow version"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported data-flow document version {version}"
                )
            }
            Self::Invalid(message) => write!(formatter, "invalid data-flow document: {message}"),
        }
    }
}

impl std::error::Error for DocError {}

pub fn to_json(graph: &Graph) -> Value {
    let nodes: Vec<Value> = graph.nodes.iter().map(node_to_json).collect();
    json!({
        "delog_dataflow": required_version(graph),
        "name": graph.name,
        "next_id": graph.next_id,
        "viewport": graph.viewport,
        "nodes": nodes,
        "edges": graph.edges,
    })
}

pub fn from_json(value: &Value) -> Result<Graph, DocError> {
    let object = value
        .as_object()
        .ok_or_else(|| DocError::Invalid("document must be an object".to_owned()))?;
    let version_value = object
        .get("delog_dataflow")
        .ok_or(DocError::MissingVersion)?;
    let version = version_value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| DocError::Invalid("delog_dataflow must be an integer".to_owned()))?;
    if !(1..=DATAFLOW_VERSION).contains(&version) {
        return Err(DocError::UnsupportedVersion(version));
    }

    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| DocError::Invalid("name must be a string".to_owned()))?
        .to_owned();
    let serialized_next = object
        .get("next_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| DocError::Invalid("next_id must be an integer".to_owned()))?;
    let viewport: Viewport = decode_field(object, "viewport")?;
    if !viewport.offset.iter().all(|value| value.is_finite()) {
        return Err(DocError::Invalid(
            "viewport offset values must be finite".to_owned(),
        ));
    }
    if !viewport.zoom.is_finite() || viewport.zoom <= 0.0 {
        return Err(DocError::Invalid(
            "viewport zoom must be finite and positive".to_owned(),
        ));
    }
    let node_values = object
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| DocError::Invalid("nodes must be an array".to_owned()))?;
    let nodes: Vec<Node> = node_values
        .iter()
        .map(node_from_json)
        .collect::<Result<_, _>>()?;
    if nodes
        .iter()
        .any(|node| !node.pos.iter().all(|value| value.is_finite()))
    {
        return Err(DocError::Invalid(
            "node position values must be finite".to_owned(),
        ));
    }
    let edges: Vec<Edge> = decode_field(object, "edges")?;
    let next_id = nodes
        .iter()
        .map(|node| node.id.0)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(serialized_next);

    let mut graph = Graph {
        name,
        nodes,
        edges: Vec::new(),
        viewport,
        next_id,
    };
    let mut node_ids = HashSet::new();
    for node in &graph.nodes {
        if !node_ids.insert(node.id) {
            return Err(DocError::Invalid(format!(
                "duplicate node id {}",
                node.id.0
            )));
        }
    }
    for edge in edges {
        let invalid = |message: &str| {
            DocError::Invalid(format!(
                "edge {} -> {} port {} is invalid: {message}",
                edge.from.0, edge.to.0, edge.to_port
            ))
        };
        let from = graph
            .node(edge.from)
            .ok_or_else(|| invalid("unknown source node"))?;
        let to = graph
            .node(edge.to)
            .ok_or_else(|| invalid("unknown destination node"))?;
        if !matches!(from.kind, NodeKind::Unknown(_)) && !matches!(to.kind, NodeKind::Unknown(_)) {
            graph
                .connect(edge.from, edge.from_port, edge.to, edge.to_port)
                .map_err(|error| invalid(&format!("{error:?}")))?;
            continue;
        }
        if edge.from == edge.to {
            return Err(invalid("self loop"));
        }
        if !matches!(from.kind, NodeKind::Unknown(_))
            && from.kind.outputs().get(edge.from_port as usize).is_none()
        {
            return Err(invalid("source node has no output"));
        }
        if !matches!(to.kind, NodeKind::Unknown(_))
            && to.kind.inputs().get(edge.to_port as usize).is_none()
        {
            return Err(invalid("invalid destination port"));
        }
        if graph.incoming(edge.to, edge.to_port).is_some() {
            return Err(invalid("destination port is occupied"));
        }
        if graph.would_cycle(edge.from, edge.to) {
            return Err(invalid("cycle"));
        }
        graph.edges.push(edge);
    }
    Ok(graph)
}

pub(crate) fn node_kind_json(kind: &NodeKind) -> Value {
    let mut object = match kind {
        NodeKind::Unknown(raw) => return raw.clone(),
        NodeKind::DataField(selector) => serialized_object(selector),
        NodeKind::Constant { value } => Map::from_iter([("value".to_owned(), json!(value))]),
        NodeKind::Add | NodeKind::Subtract | NodeKind::Multiply | NodeKind::Divide => Map::new(),
        NodeKind::ScaleOffset { multiplier, offset } => Map::from_iter([
            ("multiplier".to_owned(), json!(multiplier)),
            ("offset".to_owned(), json!(offset)),
        ]),
        NodeKind::Convert { kind } => {
            Map::from_iter([("conversion".to_owned(), json!(kind.as_str()))])
        }
        NodeKind::Align { mode } => Map::from_iter([("mode".to_owned(), json!(mode.as_str()))]),
        NodeKind::Output(spec) => serialized_object(spec),
        #[cfg(feature = "scripting")]
        NodeKind::Script(spec) => serialized_object(spec),
    };
    let tag = match kind {
        NodeKind::DataField(_) => "data_field",
        NodeKind::Constant { .. } => "constant",
        NodeKind::Add => "add",
        NodeKind::Subtract => "subtract",
        NodeKind::Multiply => "multiply",
        NodeKind::Divide => "divide",
        NodeKind::ScaleOffset { .. } => "scale_offset",
        NodeKind::Convert { .. } => "convert",
        NodeKind::Align { .. } => "align",
        NodeKind::Output(_) => "output",
        #[cfg(feature = "scripting")]
        NodeKind::Script(_) => "script",
        NodeKind::Unknown(_) => unreachable!(),
    };
    object.insert("type".to_owned(), Value::String(tag.to_owned()));
    Value::Object(object)
}

fn node_to_json(node: &Node) -> Value {
    let mut object = node_kind_json(&node.kind)
        .as_object()
        .cloned()
        .unwrap_or_else(|| {
            Map::from_iter([
                ("type".to_owned(), Value::String("unknown".to_owned())),
                ("raw".to_owned(), node_kind_json(&node.kind)),
            ])
        });
    object.insert("id".to_owned(), json!(node.id));
    object.insert("pos".to_owned(), json!(node.pos));
    Value::Object(object)
}

fn node_from_json(value: &Value) -> Result<Node, DocError> {
    let object = value
        .as_object()
        .ok_or_else(|| DocError::Invalid("node must be an object".to_owned()))?;
    let id = object
        .get("id")
        .and_then(Value::as_u64)
        .map(crate::graph::NodeId)
        .ok_or_else(|| DocError::Invalid("node id must be an integer".to_owned()))?;
    let pos: [f32; 2] = decode_field(object, "pos")?;
    let tag = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| DocError::Invalid("node type must be a string".to_owned()))?;
    let invalid = |message: &str| DocError::Invalid(format!("node {id:?}: {message}"));
    let number = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| invalid(&format!("{name} must be a number")))
    };
    let kind = match tag {
        "data_field" => NodeKind::DataField(decode_value(value)?),
        "constant" => NodeKind::Constant {
            value: number("value")?,
        },
        "add" => NodeKind::Add,
        "subtract" => NodeKind::Subtract,
        "multiply" => NodeKind::Multiply,
        "divide" => NodeKind::Divide,
        "scale_offset" => NodeKind::ScaleOffset {
            multiplier: number("multiplier")?,
            offset: number("offset")?,
        },
        "convert" => {
            let conversion = object
                .get("conversion")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("conversion must be a string"))?;
            NodeKind::Convert {
                kind: crate::graph::ConversionKind::parse(conversion).map_err(|message| invalid(&message))?,
            }
        }
        "align" => {
            let mode = object
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mode must be a string"))?;
            NodeKind::Align {
                mode: delog_core::align::AlignMode::parse(mode)
                    .map_err(|message| invalid(&message))?,
            }
        }
        "output" => NodeKind::Output(decode_value::<OutputSpec>(value)?),
        #[cfg(feature = "scripting")]
        "script" => NodeKind::Script(decode_value::<crate::script::ScriptSpec>(value)?),
        _ => NodeKind::Unknown(value.clone()),
    };
    Ok(Node { id, pos, kind })
}

fn serialized_object(value: &impl serde::Serialize) -> Map<String, Value> {
    serde_json::to_value(value)
        .expect("graph types serialize to JSON")
        .as_object()
        .cloned()
        .expect("graph structures serialize as objects")
}

fn decode_field<T: serde::de::DeserializeOwned>(
    object: &Map<String, Value>,
    field: &str,
) -> Result<T, DocError> {
    let value = object
        .get(field)
        .ok_or_else(|| DocError::Invalid(format!("missing {field}")))?;
    decode_value(value)
}

fn decode_value<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, DocError> {
    serde_json::from_value(value.clone()).map_err(|error| DocError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{FieldSelector, Node, NodeKind, OutputFieldSpec, OutputSpec, Viewport};
    use delog_core::align::AlignMode;

    fn document(nodes: Value, edges: Value) -> Value {
        serde_json::json!({
            "delog_dataflow": 1,
            "name": "damaged",
            "next_id": 4,
            "viewport": {"offset": [0.0, 0.0], "zoom": 1.0},
            "nodes": nodes,
            "edges": edges,
        })
    }

    fn add_node_json(id: u64) -> Value {
        serde_json::json!({"id": id, "pos": [0.0, 0.0], "type": "add"})
    }

    fn add(g: &mut Graph, kind: NodeKind) -> crate::graph::NodeId {
        let id = g.alloc_id();
        g.insert_node(Node {
            id,
            pos: [0.0, 0.0],
            kind,
        });
        id
    }

    #[test]
    fn v1_docs_still_load_and_single_output_graphs_still_write_v1() {
        let mut g = Graph::new("g");
        let a = add(&mut g, NodeKind::Add);
        let s = add(
            &mut g,
            NodeKind::ScaleOffset {
                multiplier: 1.0,
                offset: 0.0,
            },
        );
        g.connect(a, 0, s, 0).unwrap();
        let doc = to_json(&g);
        assert_eq!(doc["delog_dataflow"], 1);
        assert!(doc["edges"][0].get("from_port").is_none());
        assert_eq!(from_json(&doc).unwrap().edges, g.edges);
    }

    #[test]
    fn nonzero_from_port_writes_v2_and_round_trips() {
        // The source is an unknown kind so `from_json` takes the raw-edge
        // fallback path (it preserves `from_port` as-is) instead of routing
        // through `Graph::connect`, whose bounds check would reject
        // `from_port: 1` on a single-output node.
        let mut g = Graph::new("g");
        let a = add(
            &mut g,
            NodeKind::Unknown(serde_json::json!({"type": "resample", "hz": 50})),
        );
        let s = add(
            &mut g,
            NodeKind::ScaleOffset {
                multiplier: 1.0,
                offset: 0.0,
            },
        );
        g.edges.push(Edge {
            from: a,
            from_port: 1,
            to: s,
            to_port: 0,
        });

        let doc = to_json(&g);
        assert_eq!(doc["delog_dataflow"], 2);
        assert_eq!(doc["edges"][0]["from_port"], 1);

        let restored = from_json(&doc).unwrap();
        assert_eq!(restored.edges.len(), 1);
        assert_eq!(restored.edges[0].from_port, 1);
    }

    #[test]
    fn versions_above_current_are_rejected() {
        assert!(matches!(
            from_json(&serde_json::json!({"delog_dataflow": 3})),
            Err(DocError::UnsupportedVersion(3))
        ));
    }

    #[test]
    fn round_trip_preserves_everything() {
        let mut g = Graph::new("alt-compare");
        let d = g.alloc_id();
        g.insert_node(Node {
            id: d,
            pos: [10.0, 20.0],
            kind: NodeKind::DataField(FieldSelector {
                source: Some("flight_01".into()),
                topic: "GPS".into(),
                instance: Some(0),
                field: "Alt".into(),
            }),
        });
        let a = g.alloc_id();
        g.insert_node(Node {
            id: a,
            pos: [200.0, 20.0],
            kind: NodeKind::Align {
                mode: AlignMode::Linear,
            },
        });
        let o = g.alloc_id();
        g.insert_node(Node {
            id: o,
            pos: [400.0, 20.0],
            kind: NodeKind::Output(OutputSpec {
                topic: "cmp".into(),
                fields: vec![OutputFieldSpec {
                    name: "alt".into(),
                    unit: Some("m".into()),
                }],
            }),
        });
        g.connect(d, 0, a, 0).unwrap();
        g.connect(a, 0, o, 0).unwrap();
        g.viewport = Viewport {
            offset: [5.0, -3.0],
            zoom: 1.5,
        };

        let restored = from_json(&to_json(&g)).unwrap();
        assert_eq!(restored.name, "alt-compare");
        assert_eq!(restored.nodes.len(), 3);
        assert_eq!(restored.edges, g.edges);
        assert_eq!(restored.viewport.zoom, 1.5);
        assert!(matches!(
            &restored.node(a).unwrap().kind,
            NodeKind::Align {
                mode: AlignMode::Linear
            }
        ));
        let fresh = restored.clone();
        let mut fresh = fresh;
        let n = fresh.alloc_id();
        assert!(n.0 > o.0);
    }

    #[test]
    #[cfg(not(feature = "scripting"))]
    fn script_docs_degrade_to_unknown_without_the_feature() {
        let raw = serde_json::json!({
            "delog_dataflow": 1, "name": "g", "next_id": 2, "viewport": {"offset": [0.0, 0.0], "zoom": 1.0},
            "nodes": [{
                "id": 1, "pos": [0.0, 0.0], "type": "script", "name": "Double",
                "inputs": [{"name": "a"}], "outputs": [{"name": "out"}], "code": "x = 1"
            }],
            "edges": []
        });
        let g = from_json(&raw).unwrap();
        assert!(matches!(&g.nodes[0].kind, NodeKind::Unknown(_)));
        assert_eq!(to_json(&g), raw);
    }

    #[test]
    fn data_field_source_is_never_saved_and_legacy_source_is_ignored() {
        // An in-memory source binding is not written to the document.
        let mut graph = crate::graph::Graph::new("g");
        let id = graph.alloc_id();
        graph.insert_node(Node {
            id,
            pos: [0.0, 0.0],
            kind: NodeKind::DataField(FieldSelector {
                source: Some("flight".into()),
                topic: "GPS".into(),
                instance: None,
                field: "Alt".into(),
            }),
        });
        let json = to_json(&graph);
        let node = &json["nodes"][0];
        assert!(node.get("source").is_none());
        assert_eq!(node["topic"], "GPS");
        assert_eq!(node["field"], "Alt");

        // A document that still carries a legacy source loads it as agnostic.
        let raw = serde_json::json!({
            "delog_dataflow": 1, "name": "g", "next_id": 2, "viewport": {"offset": [0.0, 0.0], "zoom": 1.0},
            "nodes": [{"id": 1, "pos": [0.0, 0.0], "type": "data_field", "source": "legacy", "topic": "GPS", "field": "Alt"}],
            "edges": []
        });
        let loaded = from_json(&raw).unwrap();
        match &loaded.nodes[0].kind {
            NodeKind::DataField(selector) => {
                assert_eq!(selector.source, None);
                assert_eq!(selector.topic, "GPS");
                assert_eq!(selector.field, "Alt");
            }
            other => panic!("expected data_field, got {other:?}"),
        }
    }

    #[test]
    fn convert_node_round_trips() {
        let raw = document(
            serde_json::json!([{"id": 1, "pos": [0.0, 0.0], "type": "convert", "conversion": "m_to_ft"}]),
            serde_json::json!([]),
        );
        let graph = from_json(&raw).unwrap();
        match &graph.nodes[0].kind {
            NodeKind::Convert { kind } => {
                assert_eq!(*kind, crate::graph::ConversionKind::MToFt)
            }
            other => panic!("expected convert, got {other:?}"),
        }
        let back = to_json(&graph);
        assert_eq!(back["nodes"][0]["type"], "convert");
        assert_eq!(back["nodes"][0]["conversion"], "m_to_ft");
    }

    #[test]
    fn unknown_node_kinds_survive_round_trip() {
        let raw = serde_json::json!({
            "delog_dataflow": 1, "name": "g", "next_id": 2, "viewport": {"offset": [0.0, 0.0], "zoom": 1.0},
            "nodes": [{"id": 1, "pos": [0.0, 0.0], "type": "resample", "hz": 50}],
            "edges": []
        });
        let g = from_json(&raw).unwrap();
        assert!(matches!(&g.nodes[0].kind, NodeKind::Unknown(_)));
        let back = to_json(&g);
        assert_eq!(back["nodes"][0]["type"], "resample");
        assert_eq!(back["nodes"][0]["hz"], 50);
    }

    #[test]
    fn unknown_node_edges_survive_round_trip_when_endpoints_are_present() {
        let raw = document(
            serde_json::json!([
                {"id": 1, "pos": [0.0, 0.0], "type": "resample", "hz": 50},
                add_node_json(2)
            ]),
            serde_json::json!([{"from": 1, "to": 2, "to_port": 0}]),
        );

        let graph = from_json(&raw).unwrap();
        let back = to_json(&graph);

        assert_eq!(back["nodes"][0]["type"], "resample");
        assert_eq!(back["edges"], raw["edges"]);
    }

    #[test]
    fn missing_and_unsupported_versions_are_rejected() {
        assert!(matches!(
            from_json(&serde_json::json!({"name": "g"})),
            Err(DocError::MissingVersion)
        ));
        assert!(matches!(
            from_json(&serde_json::json!({"delog_dataflow": 99})),
            Err(DocError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn duplicate_node_ids_are_rejected() {
        let value = document(
            serde_json::json!([add_node_json(1), add_node_json(1)]),
            serde_json::json!([]),
        );
        assert!(matches!(from_json(&value), Err(DocError::Invalid(_))));
    }

    #[test]
    fn dangling_edges_are_rejected() {
        let value = document(
            serde_json::json!([add_node_json(1)]),
            serde_json::json!([{"from": 1, "to": 2, "to_port": 0}]),
        );
        assert!(matches!(from_json(&value), Err(DocError::Invalid(_))));
    }

    #[test]
    fn invalid_edge_ports_are_rejected() {
        let value = document(
            serde_json::json!([add_node_json(1), add_node_json(2)]),
            serde_json::json!([{"from": 1, "to": 2, "to_port": 9}]),
        );
        assert!(matches!(from_json(&value), Err(DocError::Invalid(_))));
    }

    #[test]
    fn cyclic_edges_are_rejected() {
        let value = document(
            serde_json::json!([add_node_json(1), add_node_json(2)]),
            serde_json::json!([
                {"from": 1, "to": 2, "to_port": 0},
                {"from": 2, "to": 1, "to_port": 0}
            ]),
        );
        assert!(matches!(from_json(&value), Err(DocError::Invalid(_))));
    }

    #[test]
    fn non_finite_node_positions_are_rejected() {
        let mut value = document(serde_json::json!([add_node_json(1)]), serde_json::json!([]));
        value["nodes"][0]["pos"] = serde_json::json!([1.0e100, 0.0]);
        assert!(matches!(from_json(&value), Err(DocError::Invalid(_))));
    }

    #[test]
    fn invalid_viewport_values_are_rejected() {
        for viewport in [
            serde_json::json!({"offset": [1.0e100, 0.0], "zoom": 1.0}),
            serde_json::json!({"offset": [0.0, 0.0], "zoom": 1.0e100}),
            serde_json::json!({"offset": [0.0, 0.0], "zoom": 0.0}),
            serde_json::json!({"offset": [0.0, 0.0], "zoom": -1.0}),
        ] {
            let mut value = document(serde_json::json!([]), serde_json::json!([]));
            value["viewport"] = viewport;
            assert!(matches!(from_json(&value), Err(DocError::Invalid(_))));
        }
    }
}

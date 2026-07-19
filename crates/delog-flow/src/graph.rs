use std::collections::HashSet;

use delog_core::align::AlignMode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<u32>,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFieldSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSpec {
    pub topic: String,
    pub fields: Vec<OutputFieldSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    DataField(FieldSelector),
    Constant { value: f64 },
    Add,
    Subtract,
    Multiply,
    Divide,
    ScaleOffset { multiplier: f64, offset: f64 },
    Align { mode: AlignMode },
    Output(OutputSpec),
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    Signal,
    Scalar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec {
    pub name: String,
    pub accepts: Vec<PortType>,
}

impl NodeKind {
    pub fn inputs(&self) -> Vec<PortSpec> {
        let port = |name: &str, accepts: Vec<PortType>| PortSpec {
            name: name.to_owned(),
            accepts,
        };
        match self {
            Self::Add | Self::Subtract => vec![
                port("A", vec![PortType::Signal]),
                port("B", vec![PortType::Signal]),
            ],
            Self::Multiply | Self::Divide => vec![
                port("A", vec![PortType::Signal]),
                port("B", vec![PortType::Signal, PortType::Scalar]),
            ],
            Self::ScaleOffset { .. } => vec![port("In", vec![PortType::Signal])],
            Self::Align { .. } => vec![
                port("Data", vec![PortType::Signal]),
                port("Reference", vec![PortType::Signal]),
            ],
            Self::Output(spec) => spec
                .fields
                .iter()
                .map(|field| port(&field.name, vec![PortType::Signal]))
                .collect(),
            Self::DataField(_) | Self::Constant { .. } | Self::Unknown(_) => Vec::new(),
        }
    }

    pub fn output(&self) -> Option<PortType> {
        match self {
            Self::Constant { .. } => Some(PortType::Scalar),
            Self::DataField(_)
            | Self::Add
            | Self::Subtract
            | Self::Multiply
            | Self::Divide
            | Self::ScaleOffset { .. }
            | Self::Align { .. } => Some(PortType::Signal),
            Self::Output(_) | Self::Unknown(_) => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::DataField(selector) => selector.field.clone(),
            Self::Constant { .. } => "Constant".to_owned(),
            Self::Add => "Add".to_owned(),
            Self::Subtract => "Subtract".to_owned(),
            Self::Multiply => "Multiply".to_owned(),
            Self::Divide => "Divide".to_owned(),
            Self::ScaleOffset { .. } => "Scale / Offset".to_owned(),
            Self::Align { .. } => "Align to Timeline".to_owned(),
            Self::Output(spec) => format!("Output: {}", spec.topic),
            Self::Unknown(_) => "Unknown node".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub pos: [f32; 2],
    pub kind: NodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub to_port: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub offset: [f32; 2],
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Graph {
    pub name: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub viewport: Viewport,
    pub(crate) next_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    UnknownNode,
    SelfLoop,
    NoOutput,
    BadPort,
    TypeMismatch,
    InputOccupied,
    Cycle,
}

impl Graph {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            viewport: Viewport::default(),
            next_id: 1,
        }
    }

    pub fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn insert_node(&mut self, node: Node) {
        self.next_id = self.next_id.max(node.id.0.saturating_add(1));
        self.nodes.push(node);
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    pub fn remove_node(&mut self, id: NodeId) -> Option<(Node, Vec<Edge>)> {
        let index = self.nodes.iter().position(|node| node.id == id)?;
        let node = self.nodes.remove(index);
        let mut removed = Vec::new();
        self.edges.retain(|edge| {
            if edge.from == id || edge.to == id {
                removed.push(edge.clone());
                false
            } else {
                true
            }
        });
        Some((node, removed))
    }

    pub fn check_connect(
        &self,
        from: NodeId,
        to: NodeId,
        to_port: u32,
    ) -> Result<(), ConnectError> {
        let Some(from_node) = self.node(from) else {
            return Err(ConnectError::UnknownNode);
        };
        let Some(to_node) = self.node(to) else {
            return Err(ConnectError::UnknownNode);
        };
        if from == to {
            return Err(ConnectError::SelfLoop);
        }
        let Some(output) = from_node.kind.output() else {
            return Err(ConnectError::NoOutput);
        };
        let inputs = to_node.kind.inputs();
        let Some(input) = inputs.get(to_port as usize) else {
            return Err(ConnectError::BadPort);
        };
        if !input.accepts.contains(&output) {
            return Err(ConnectError::TypeMismatch);
        }
        if self.incoming(to, to_port).is_some() {
            return Err(ConnectError::InputOccupied);
        }
        if self.would_cycle(from, to) {
            return Err(ConnectError::Cycle);
        }
        Ok(())
    }

    pub fn connect(&mut self, from: NodeId, to: NodeId, to_port: u32) -> Result<(), ConnectError> {
        self.check_connect(from, to, to_port)?;
        self.edges.push(Edge { from, to, to_port });
        Ok(())
    }

    pub fn disconnect(&mut self, to: NodeId, to_port: u32) -> Option<Edge> {
        let index = self
            .edges
            .iter()
            .position(|edge| edge.to == to && edge.to_port == to_port)?;
        Some(self.edges.remove(index))
    }

    pub fn incoming(&self, to: NodeId, to_port: u32) -> Option<NodeId> {
        self.edges
            .iter()
            .find(|edge| edge.to == to && edge.to_port == to_port)
            .map(|edge| edge.from)
    }

    pub fn would_cycle(&self, from: NodeId, to: NodeId) -> bool {
        let mut stack = vec![to];
        let mut visited = HashSet::new();
        while let Some(current) = stack.pop() {
            if current == from {
                return true;
            }
            if !visited.insert(current) {
                continue;
            }
            stack.extend(
                self.edges
                    .iter()
                    .filter(|edge| edge.from == current)
                    .map(|edge| edge.to),
            );
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_node(g: &mut Graph, kind: NodeKind) -> NodeId {
        let id = g.alloc_id();
        g.insert_node(Node {
            id,
            pos: [0.0, 0.0],
            kind,
        });
        id
    }

    #[test]
    fn ids_are_stable_and_monotonic() {
        let mut g = Graph::new("t");
        let a = add_node(&mut g, NodeKind::Add);
        let b = add_node(&mut g, NodeKind::Add);
        assert!(b.0 > a.0);
        g.remove_node(a);
        let c = add_node(&mut g, NodeKind::Add);
        assert!(c.0 > b.0, "removed ids are never reused");
    }

    #[test]
    fn connect_type_checks_ports() {
        let mut g = Graph::new("t");
        let konst = add_node(&mut g, NodeKind::Constant { value: 2.0 });
        let add = add_node(&mut g, NodeKind::Add);
        let mul = add_node(&mut g, NodeKind::Multiply);
        assert_eq!(g.connect(konst, add, 0), Err(ConnectError::TypeMismatch));
        assert_eq!(g.connect(konst, mul, 1), Ok(()));
        assert_eq!(g.connect(konst, mul, 7), Err(ConnectError::BadPort));
    }

    #[test]
    fn input_ports_accept_one_edge_and_disconnect_frees_them() {
        let mut g = Graph::new("t");
        let d1 = add_node(
            &mut g,
            NodeKind::ScaleOffset {
                multiplier: 1.0,
                offset: 0.0,
            },
        );
        let d2 = add_node(
            &mut g,
            NodeKind::ScaleOffset {
                multiplier: 1.0,
                offset: 0.0,
            },
        );
        let add = add_node(&mut g, NodeKind::Add);
        assert_eq!(g.connect(d1, add, 0), Ok(()));
        assert_eq!(g.connect(d2, add, 0), Err(ConnectError::InputOccupied));
        assert_eq!(
            g.disconnect(add, 0),
            Some(Edge {
                from: d1,
                to: add,
                to_port: 0
            })
        );
        assert_eq!(g.connect(d2, add, 0), Ok(()));
        assert_eq!(g.incoming(add, 0), Some(d2));
    }

    #[test]
    fn cycles_and_self_loops_are_rejected() {
        let mut g = Graph::new("t");
        let a = add_node(&mut g, NodeKind::Add);
        let b = add_node(&mut g, NodeKind::Add);
        assert_eq!(g.connect(a, a, 0), Err(ConnectError::SelfLoop));
        g.connect(a, b, 0).unwrap();
        assert_eq!(g.connect(b, a, 0), Err(ConnectError::Cycle));
    }

    #[test]
    fn remove_node_returns_node_and_all_touching_edges() {
        let mut g = Graph::new("t");
        let a = add_node(&mut g, NodeKind::Constant { value: 1.0 });
        let m = add_node(&mut g, NodeKind::Multiply);
        let s = add_node(
            &mut g,
            NodeKind::ScaleOffset {
                multiplier: 2.0,
                offset: 0.0,
            },
        );
        g.connect(a, m, 1).unwrap();
        g.connect(m, s, 0).unwrap();
        let (node, edges) = g.remove_node(m).unwrap();
        assert!(matches!(node.kind, NodeKind::Multiply));
        assert_eq!(edges.len(), 2);
        assert!(g.edges.is_empty());
    }

    #[test]
    fn output_ports_follow_field_specs() {
        let out = NodeKind::Output(OutputSpec {
            topic: "derived".into(),
            fields: vec![
                OutputFieldSpec {
                    name: "a".into(),
                    unit: None,
                },
                OutputFieldSpec {
                    name: "b".into(),
                    unit: Some("m".into()),
                },
            ],
        });
        let ports = out.inputs();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[1].name, "b");
        assert!(out.output().is_none());
    }
}

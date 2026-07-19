use crate::graph::{ConnectError, Edge, Graph, Node, NodeId, NodeKind, OutputFieldSpec};

#[derive(Debug, Clone, PartialEq)]
pub enum GraphCommand {
    AddNode {
        node: Node,
    },
    RemoveNode {
        id: NodeId,
    },
    MoveNode {
        id: NodeId,
        to: [f32; 2],
    },
    Connect {
        from: NodeId,
        from_port: u32,
        to: NodeId,
        to_port: u32,
    },
    Disconnect {
        to: NodeId,
        to_port: u32,
    },
    SetKind {
        id: NodeId,
        kind: NodeKind,
    },
    InsertOutputField {
        id: NodeId,
        index: usize,
        field: OutputFieldSpec,
        connection: Option<(usize, NodeId, u32)>,
    },
    RemoveOutputField {
        id: NodeId,
        index: usize,
    },
    RestoreEdges {
        edges: Vec<(usize, Edge)>,
    },
    Batch(Vec<GraphCommand>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyError {
    UnknownNode,
    Connect(ConnectError),
    NothingToDo,
    InvalidOutputField,
}

pub fn apply(graph: &mut Graph, cmd: GraphCommand) -> Result<GraphCommand, ApplyError> {
    match cmd {
        GraphCommand::AddNode { node } => {
            let id = node.id;
            graph.insert_node(node);
            Ok(GraphCommand::RemoveNode { id })
        }
        GraphCommand::RemoveNode { id } => {
            let (node, edges) = graph.remove_node(id).ok_or(ApplyError::UnknownNode)?;
            let mut inverse = Vec::with_capacity(edges.len() + 1);
            inverse.push(GraphCommand::AddNode { node });
            inverse.extend(edges.into_iter().map(|edge| GraphCommand::Connect {
                from: edge.from,
                from_port: edge.from_port,
                to: edge.to,
                to_port: edge.to_port,
            }));
            Ok(GraphCommand::Batch(inverse))
        }
        GraphCommand::MoveNode { id, to } => {
            let node = graph.node_mut(id).ok_or(ApplyError::UnknownNode)?;
            let old = node.pos;
            node.pos = to;
            Ok(GraphCommand::MoveNode { id, to: old })
        }
        GraphCommand::Connect {
            from,
            from_port,
            to,
            to_port,
        } => {
            graph
                .connect(from, from_port, to, to_port)
                .map_err(ApplyError::Connect)?;
            Ok(GraphCommand::Disconnect { to, to_port })
        }
        GraphCommand::Disconnect { to, to_port } => {
            let edge = graph
                .disconnect(to, to_port)
                .ok_or(ApplyError::NothingToDo)?;
            Ok(GraphCommand::Connect {
                from: edge.from,
                from_port: edge.from_port,
                to: edge.to,
                to_port: edge.to_port,
            })
        }
        GraphCommand::SetKind { id, kind } => {
            let node = graph.node_mut(id).ok_or(ApplyError::UnknownNode)?;
            let old = std::mem::replace(&mut node.kind, kind);
            let inputs_changed = old.inputs().iter().map(|input| &input.accepts).ne(node
                .kind
                .inputs()
                .iter()
                .map(|input| &input.accepts));
            if !inputs_changed {
                return Ok(GraphCommand::SetKind { id, kind: old });
            }
            let old_edges = std::mem::take(&mut graph.edges);
            let mut removed = Vec::new();
            for (index, edge) in old_edges.into_iter().enumerate() {
                if edge.to == id {
                    removed.push((index, edge));
                } else {
                    graph.edges.push(edge);
                }
            }
            if removed.is_empty() {
                Ok(GraphCommand::SetKind { id, kind: old })
            } else {
                Ok(GraphCommand::Batch(vec![
                    GraphCommand::SetKind { id, kind: old },
                    GraphCommand::RestoreEdges { edges: removed },
                ]))
            }
        }
        GraphCommand::InsertOutputField {
            id,
            index,
            field,
            connection,
        } => {
            let node = graph.node_mut(id).ok_or(ApplyError::UnknownNode)?;
            let NodeKind::Output(spec) = &mut node.kind else {
                return Err(ApplyError::InvalidOutputField);
            };
            if index > spec.fields.len() {
                return Err(ApplyError::InvalidOutputField);
            }
            spec.fields.insert(index, field);
            for edge in &mut graph.edges {
                if edge.to == id && edge.to_port >= index as u32 {
                    edge.to_port += 1;
                }
            }
            if let Some((edge_index, from, from_port)) = connection {
                graph.edges.insert(
                    edge_index.min(graph.edges.len()),
                    crate::graph::Edge {
                        from,
                        from_port,
                        to: id,
                        to_port: index as u32,
                    },
                );
            }
            Ok(GraphCommand::RemoveOutputField { id, index })
        }
        GraphCommand::RemoveOutputField { id, index } => {
            let node = graph.node_mut(id).ok_or(ApplyError::UnknownNode)?;
            let NodeKind::Output(spec) = &mut node.kind else {
                return Err(ApplyError::InvalidOutputField);
            };
            if index >= spec.fields.len() {
                return Err(ApplyError::InvalidOutputField);
            }
            let field = spec.fields.remove(index);
            let removed_connection = graph
                .edges
                .iter()
                .position(|edge| edge.to == id && edge.to_port == index as u32)
                .map(|edge_index| {
                    let edge = graph.edges.remove(edge_index);
                    (edge_index, edge.from, edge.from_port)
                });
            for edge in &mut graph.edges {
                if edge.to == id && edge.to_port > index as u32 {
                    edge.to_port -= 1;
                }
            }
            Ok(GraphCommand::InsertOutputField {
                id,
                index,
                field,
                connection: removed_connection,
            })
        }
        GraphCommand::RestoreEdges { mut edges } => {
            edges.sort_by_key(|(index, _)| *index);
            let mut validation = graph.clone();
            for (_, edge) in &edges {
                validation
                    .connect(edge.from, edge.from_port, edge.to, edge.to_port)
                    .map_err(ApplyError::Connect)?;
            }
            let mut disconnects = Vec::with_capacity(edges.len());
            for (index, edge) in edges {
                disconnects.push(GraphCommand::Disconnect {
                    to: edge.to,
                    to_port: edge.to_port,
                });
                graph.edges.insert(index.min(graph.edges.len()), edge);
            }
            Ok(GraphCommand::Batch(disconnects))
        }
        GraphCommand::Batch(commands) => {
            let mut inverses = Vec::with_capacity(commands.len());
            for command in commands {
                match apply(graph, command) {
                    Ok(inverse) => inverses.push(inverse),
                    Err(error) => {
                        for inverse in inverses.into_iter().rev() {
                            let _ = apply(graph, inverse);
                        }
                        return Err(error);
                    }
                }
            }
            inverses.reverse();
            Ok(GraphCommand::Batch(inverses))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{OutputFieldSpec, OutputSpec};

    fn output(fields: &[&str]) -> NodeKind {
        NodeKind::Output(OutputSpec {
            topic: "derived".into(),
            fields: fields
                .iter()
                .map(|name| OutputFieldSpec {
                    name: (*name).into(),
                    unit: None,
                })
                .collect(),
        })
    }

    #[test]
    fn every_command_round_trips_through_its_inverse() {
        let mut g = Graph::new("t");
        let id = g.alloc_id();
        let node = Node {
            id,
            pos: [1.0, 2.0],
            kind: NodeKind::Constant { value: 3.0 },
        };

        let inv_add = apply(&mut g, GraphCommand::AddNode { node: node.clone() }).unwrap();
        let mul = g.alloc_id();
        apply(
            &mut g,
            GraphCommand::AddNode {
                node: Node {
                    id: mul,
                    pos: [9.0, 9.0],
                    kind: NodeKind::Multiply,
                },
            },
        )
        .unwrap();
        let inv_conn = apply(
            &mut g,
            GraphCommand::Connect {
                from: id,
                from_port: 0,
                to: mul,
                to_port: 1,
            },
        )
        .unwrap();
        assert_eq!(
            g.edges,
            vec![Edge {
                from: id,
                from_port: 0,
                to: mul,
                to_port: 1
            }]
        );
        let inv_move = apply(&mut g, GraphCommand::MoveNode { id, to: [5.0, 5.0] }).unwrap();
        let inv_kind = apply(
            &mut g,
            GraphCommand::SetKind {
                id,
                kind: NodeKind::Constant { value: 7.0 },
            },
        )
        .unwrap();

        let reference = g.clone();
        for inverse in [inv_kind, inv_move] {
            apply(&mut g, inverse).unwrap();
        }
        assert_eq!(g.node(id).unwrap().pos, [1.0, 2.0]);
        assert!(matches!(g.node(id).unwrap().kind, NodeKind::Constant { value } if value == 3.0));
        apply(&mut g, inv_conn).unwrap();
        assert!(g.edges.is_empty());
        apply(&mut g, inv_add).unwrap();
        assert!(g.node(id).is_none());
        let _ = reference;
    }

    #[test]
    fn remove_node_inverse_restores_edges() {
        let mut g = Graph::new("t");
        let c = g.alloc_id();
        apply(
            &mut g,
            GraphCommand::AddNode {
                node: Node {
                    id: c,
                    pos: [0.0; 2],
                    kind: NodeKind::Constant { value: 1.0 },
                },
            },
        )
        .unwrap();
        let m = g.alloc_id();
        apply(
            &mut g,
            GraphCommand::AddNode {
                node: Node {
                    id: m,
                    pos: [0.0; 2],
                    kind: NodeKind::Multiply,
                },
            },
        )
        .unwrap();
        apply(
            &mut g,
            GraphCommand::Connect {
                from: c,
                from_port: 0,
                to: m,
                to_port: 1,
            },
        )
        .unwrap();

        let inverse = apply(&mut g, GraphCommand::RemoveNode { id: m }).unwrap();
        assert!(g.node(m).is_none() && g.edges.is_empty());
        apply(&mut g, inverse).unwrap();
        assert!(g.node(m).is_some());
        assert_eq!(g.incoming(m, 1), Some((c, 0)));
    }

    #[test]
    fn batch_inverse_applies_in_reverse_order() {
        let mut g = Graph::new("t");
        let id = g.alloc_id();
        let cmds = GraphCommand::Batch(vec![
            GraphCommand::AddNode {
                node: Node {
                    id,
                    pos: [0.0; 2],
                    kind: NodeKind::Add,
                },
            },
            GraphCommand::MoveNode { id, to: [4.0, 4.0] },
        ]);
        let inverse = apply(&mut g, cmds).unwrap();
        apply(&mut g, inverse).unwrap();
        assert!(g.node(id).is_none());
    }

    #[test]
    fn removing_output_field_remaps_later_edges_and_undo_restores_graph() {
        let mut graph = Graph::new("g");
        let first = graph.alloc_id();
        graph.insert_node(Node {
            id: first,
            pos: [0.0; 2],
            kind: NodeKind::Add,
        });
        let second = graph.alloc_id();
        graph.insert_node(Node {
            id: second,
            pos: [0.0; 2],
            kind: NodeKind::Add,
        });
        let output_id = graph.alloc_id();
        graph.insert_node(Node {
            id: output_id,
            pos: [0.0; 2],
            kind: output(&["first", "second"]),
        });
        graph.connect(first, 0, output_id, 0).unwrap();
        graph.connect(second, 0, output_id, 1).unwrap();
        let original = graph.clone();

        let inverse = apply(
            &mut graph,
            GraphCommand::RemoveOutputField {
                id: output_id,
                index: 0,
            },
        )
        .unwrap();

        assert_eq!(graph.incoming(output_id, 0), Some((second, 0)));
        assert_eq!(graph.edges.len(), 1);
        apply(&mut graph, inverse).unwrap();
        assert_eq!(graph, original);
    }

    #[test]
    fn set_kind_input_layout_change_undo_restores_exact_edge_order() {
        let mut graph = Graph::new("g");
        let first = graph.alloc_id();
        graph.insert_node(Node {
            id: first,
            pos: [0.0; 2],
            kind: NodeKind::Add,
        });
        let second = graph.alloc_id();
        graph.insert_node(Node {
            id: second,
            pos: [0.0; 2],
            kind: NodeKind::Add,
        });
        let target = graph.alloc_id();
        graph.insert_node(Node {
            id: target,
            pos: [0.0; 2],
            kind: NodeKind::Add,
        });
        let unrelated = graph.alloc_id();
        graph.insert_node(Node {
            id: unrelated,
            pos: [0.0; 2],
            kind: NodeKind::ScaleOffset {
                multiplier: 1.0,
                offset: 0.0,
            },
        });
        graph.connect(first, 0, target, 0).unwrap();
        graph.connect(first, 0, unrelated, 0).unwrap();
        graph.connect(second, 0, target, 1).unwrap();
        let original = graph.clone();

        let inverse = apply(
            &mut graph,
            GraphCommand::SetKind {
                id: target,
                kind: NodeKind::ScaleOffset {
                    multiplier: 1.0,
                    offset: 0.0,
                },
            },
        )
        .unwrap();
        apply(&mut graph, inverse).unwrap();

        assert_eq!(graph, original);
    }
}

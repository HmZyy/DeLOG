use std::collections::{HashMap, HashSet};

use delog_flow::graph::{Graph, NodeId, Viewport};
use egui::{Pos2, Rect, Vec2};
use egui_graph::{SocketKind, View};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompletedMove {
    pub id: NodeId,
    pub from: [f32; 2],
    pub to: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeStart {
    pub node: NodeId,
    pub kind: SocketKind,
    pub port: usize,
}

#[derive(Default)]
pub struct GraphCanvasState {
    pub viewport: Viewport,
    pub view: View,
    pub edge_start: Option<EdgeStart>,
    pub(crate) selected_edges: HashSet<(NodeId, NodeId, u32)>,
    drag_origins: HashMap<NodeId, [f32; 2]>,
    pending_positions: HashMap<NodeId, [f32; 2]>,
    viewport_initialized: bool,
    fit_requested: bool,
}

pub fn ui_node_id(id: NodeId) -> egui_graph::NodeId {
    egui_graph::NodeId::from_u64(id.0)
}

pub fn domain_node_id(id: egui_graph::NodeId) -> NodeId {
    NodeId(id.value())
}

pub fn complete_connection(
    first: EdgeStart,
    second: EdgeStart,
) -> Option<(NodeId, u32, NodeId, u32)> {
    let (output, input) = match (first.kind, second.kind) {
        (SocketKind::Output, SocketKind::Input) => (first, second),
        (SocketKind::Input, SocketKind::Output) => (second, first),
        _ => return None,
    };
    Some((
        output.node,
        u32::try_from(output.port).ok()?,
        input.node,
        u32::try_from(input.port).ok()?,
    ))
}

impl GraphCanvasState {
    pub fn request_fit(&mut self) {
        self.fit_requested = true;
    }

    pub fn fit_requested(&self) -> bool {
        self.fit_requested
    }

    pub fn apply_fit_request(
        &mut self,
        graph: &Graph,
        canvas_size: Vec2,
        fitted_scene_rect: Option<Rect>,
    ) -> bool {
        if !self.fit_requested {
            return false;
        }
        if graph.nodes.is_empty() {
            self.fit_requested = false;
            self.viewport.zoom = 1.0;
            self.view.scene_rect = Rect::from_min_size(
                Pos2::new(self.viewport.offset[0], self.viewport.offset[1]),
                canvas_size,
            );
            return true;
        }
        let Some(scene_rect) = fitted_scene_rect else {
            return false;
        };

        self.fit_requested = false;
        self.view.scene_rect = scene_rect;
        self.viewport.offset = [scene_rect.min.x, scene_rect.min.y];
        self.viewport.zoom = (canvas_size / scene_rect.size()).min_elem();
        true
    }

    pub fn prepare(&mut self, graph: &Graph, canvas_size: Vec2) {
        if !self.viewport_initialized {
            self.viewport = graph.viewport;
            self.viewport_initialized = true;
        }

        let zoom = self.viewport.zoom.max(f32::EPSILON);
        self.view.scene_rect = Rect::from_min_size(
            Pos2::new(self.viewport.offset[0], self.viewport.offset[1]),
            canvas_size / zoom,
        );
        for node in &graph.nodes {
            self.pending_positions.remove(&node.id);
            let position = Pos2::new(node.pos[0], node.pos[1]);
            let layout = self.view.layout.entry(ui_node_id(node.id));
            if self.drag_origins.contains_key(&node.id) {
                layout.or_insert(position);
            } else {
                layout
                    .and_modify(|current| *current = position)
                    .or_insert(position);
            }
        }
    }

    pub fn finish(
        &mut self,
        graph: &Graph,
        canvas_size: Vec2,
        primary_down: bool,
    ) -> Vec<CompletedMove> {
        self.viewport.offset = [self.view.scene_rect.min.x, self.view.scene_rect.min.y];
        let scene_size = self.view.scene_rect.size();
        if scene_size.x > 0.0 && scene_size.y > 0.0 {
            self.viewport.zoom = (canvas_size / scene_size).min_elem();
        }

        let live: HashSet<_> = graph.nodes.iter().map(|node| ui_node_id(node.id)).collect();
        self.view.layout.retain(|id, _| live.contains(id));
        self.drag_origins.retain(|id, _| graph.node(*id).is_some());
        self.pending_positions
            .retain(|id, _| graph.node(*id).is_some());

        if primary_down {
            for node in &graph.nodes {
                let Some(position) = self.view.layout.get(&ui_node_id(node.id)) else {
                    continue;
                };
                if [position.x, position.y] != node.pos {
                    self.drag_origins.entry(node.id).or_insert(node.pos);
                }
            }
            return Vec::new();
        }

        let mut origins = std::mem::take(&mut self.drag_origins);
        let mut moves = Vec::new();
        for node in &graph.nodes {
            let Some(position) = self.view.layout.get(&ui_node_id(node.id)) else {
                continue;
            };
            let to = [position.x, position.y];
            let from = origins.remove(&node.id).unwrap_or(node.pos);
            if to != from && self.pending_positions.get(&node.id) != Some(&to) {
                self.pending_positions.insert(node.id, to);
                moves.push(CompletedMove {
                    id: node.id,
                    from,
                    to,
                });
            }
        }
        moves
    }

    pub fn reset(&mut self, graph: &Graph) {
        self.viewport = graph.viewport;
        self.viewport_initialized = true;
        self.view = View::default();
        self.view.layout.extend(
            graph
                .nodes
                .iter()
                .map(|node| (ui_node_id(node.id), Pos2::new(node.pos[0], node.pos[1]))),
        );
        self.edge_start = None;
        self.selected_edges.clear();
        self.drag_origins.clear();
        self.pending_positions.clear();
        self.fit_requested = false;
    }
}

#[cfg(test)]
mod tests {
    use delog_flow::command::{GraphCommand, apply};
    use delog_flow::graph::{Graph, Node, NodeId, NodeKind, Viewport};
    use egui_graph::SocketKind;

    use super::*;

    fn graph_with_nodes() -> Graph {
        let mut graph = Graph::new("adapter");
        graph.insert_node(Node {
            id: NodeId(7),
            pos: [10.0, 20.0],
            kind: NodeKind::Constant { value: 1.0 },
        });
        graph.insert_node(Node {
            id: NodeId(11),
            pos: [30.0, 40.0],
            kind: NodeKind::Add,
        });
        graph
    }

    #[test]
    fn ui_node_ids_round_trip() {
        let domain = NodeId(42);
        assert_eq!(domain_node_id(ui_node_id(domain)), domain);
    }

    #[test]
    fn connection_translation_accepts_output_to_input_in_either_order() {
        let output = EdgeStart {
            node: NodeId(1),
            kind: SocketKind::Output,
            port: 3,
        };
        let input = EdgeStart {
            node: NodeId(2),
            kind: SocketKind::Input,
            port: 4,
        };

        assert_eq!(
            complete_connection(output, input),
            Some((NodeId(1), 3, NodeId(2), 4))
        );
        assert_eq!(
            complete_connection(input, output),
            Some((NodeId(1), 3, NodeId(2), 4))
        );
    }

    #[test]
    fn connection_translation_rejects_matching_socket_kinds() {
        let first = EdgeStart {
            node: NodeId(1),
            kind: SocketKind::Input,
            port: 0,
        };
        let second = EdgeStart {
            node: NodeId(2),
            kind: SocketKind::Input,
            port: 1,
        };

        assert_eq!(complete_connection(first, second), None);
    }

    #[test]
    fn connection_translation_rejects_unrepresentable_port_indices() {
        let output = EdgeStart {
            node: NodeId(1),
            kind: SocketKind::Output,
            port: usize::MAX,
        };
        let input = EdgeStart {
            node: NodeId(2),
            kind: SocketKind::Input,
            port: 0,
        };

        assert_eq!(complete_connection(output, input), None);
    }

    #[test]
    fn prepare_seeds_missing_layout_and_restores_viewport() {
        let mut graph = graph_with_nodes();
        graph.viewport = Viewport {
            offset: [4.0, 8.0],
            zoom: 2.0,
        };
        let mut state = GraphCanvasState::default();

        state.prepare(&graph, egui::vec2(800.0, 600.0));

        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(7))],
            egui::pos2(10.0, 20.0)
        );
        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(11))],
            egui::pos2(30.0, 40.0)
        );
        assert_eq!(state.view.scene_rect.min, egui::pos2(4.0, 8.0));
        assert_eq!(state.view.scene_rect.size(), egui::vec2(400.0, 300.0));
    }

    #[test]
    fn fit_request_waits_for_bounds_then_applies_once() {
        let graph = graph_with_nodes();
        let mut state = GraphCanvasState::default();
        let size = egui::vec2(800.0, 600.0);
        let fitted = egui::Rect::from_min_size(egui::pos2(-50.0, -25.0), size / 0.8);
        state.request_fit();

        assert!(state.fit_requested());
        assert!(!state.apply_fit_request(&graph, size, None));
        assert!(state.fit_requested());
        assert!(state.apply_fit_request(&graph, size, Some(fitted)));
        assert!(!state.fit_requested());
        assert_eq!(state.view.scene_rect, fitted);
        assert_eq!(state.viewport.offset, [-50.0, -25.0]);
        assert_eq!(state.viewport.zoom, 0.8);
        assert!(!state.apply_fit_request(&graph, size, Some(fitted)));
    }

    #[test]
    fn empty_fit_resets_zoom_but_preserves_pan() {
        let graph = Graph::new("empty");
        let mut state = GraphCanvasState::default();
        state.reset(&graph);
        state.viewport = Viewport {
            offset: [-42.0, 17.0],
            zoom: 2.0,
        };
        state.view.scene_rect =
            egui::Rect::from_min_size(egui::pos2(-42.0, 17.0), egui::vec2(400.0, 300.0));
        state.request_fit();

        assert!(state.apply_fit_request(&graph, egui::vec2(800.0, 600.0), None));
        assert_eq!(state.viewport.offset, [-42.0, 17.0]);
        assert_eq!(state.viewport.zoom, 1.0);
        assert_eq!(state.view.scene_rect.min, egui::pos2(-42.0, 17.0));
        assert_eq!(state.view.scene_rect.size(), egui::vec2(800.0, 600.0));
    }

    #[test]
    fn finish_emits_one_move_only_after_pointer_release() {
        let graph = graph_with_nodes();
        let mut state = GraphCanvasState::default();
        let size = egui::vec2(800.0, 600.0);
        state.prepare(&graph, size);
        state
            .view
            .layout
            .insert(ui_node_id(NodeId(7)), egui::pos2(50.0, 60.0));

        assert!(state.finish(&graph, size, true).is_empty());
        assert_eq!(
            state.finish(&graph, size, false),
            vec![CompletedMove {
                id: NodeId(7),
                from: [10.0, 20.0],
                to: [50.0, 60.0],
            }]
        );
        assert!(state.finish(&graph, size, false).is_empty());
    }

    #[test]
    fn finish_emits_move_when_release_is_first_changed_frame() {
        let graph = graph_with_nodes();
        let mut state = GraphCanvasState::default();
        let size = egui::vec2(800.0, 600.0);
        state.prepare(&graph, size);
        state
            .view
            .layout
            .insert(ui_node_id(NodeId(7)), egui::pos2(50.0, 60.0));

        assert_eq!(
            state.finish(&graph, size, false),
            vec![CompletedMove {
                id: NodeId(7),
                from: [10.0, 20.0],
                to: [50.0, 60.0],
            }]
        );
    }

    #[test]
    fn prepare_synchronizes_idle_layout_from_domain_graph() {
        let mut graph = graph_with_nodes();
        let mut state = GraphCanvasState::default();
        let size = egui::vec2(800.0, 600.0);
        state.prepare(&graph, size);
        state
            .view
            .layout
            .insert(ui_node_id(NodeId(7)), egui::pos2(50.0, 60.0));
        graph.node_mut(NodeId(7)).unwrap().pos = [5.0, 6.0];

        state.prepare(&graph, size);

        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(7))],
            egui::pos2(5.0, 6.0)
        );
    }

    #[test]
    fn prepare_honors_undo_before_pending_move_acknowledgment() {
        let mut graph = graph_with_nodes();
        let mut state = GraphCanvasState::default();
        let size = egui::vec2(800.0, 600.0);
        state.prepare(&graph, size);
        state
            .view
            .layout
            .insert(ui_node_id(NodeId(7)), egui::pos2(50.0, 60.0));
        let moves = state.finish(&graph, size, false);
        let undo = apply(
            &mut graph,
            GraphCommand::MoveNode {
                id: moves[0].id,
                to: moves[0].to,
            },
        )
        .unwrap();
        apply(&mut graph, undo).unwrap();

        state.prepare(&graph, size);

        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(7))],
            egui::pos2(10.0, 20.0)
        );
    }

    #[test]
    fn reset_discards_stale_layout_and_copies_graph_viewport() {
        let mut graph = graph_with_nodes();
        graph.viewport = Viewport {
            offset: [-5.0, 12.0],
            zoom: 1.5,
        };
        let mut state = GraphCanvasState::default();
        state
            .view
            .layout
            .insert(ui_node_id(NodeId(99)), egui::pos2(1.0, 2.0));

        state.reset(&graph);

        assert_eq!(state.viewport, graph.viewport);
        assert!(!state.view.layout.contains_key(&ui_node_id(NodeId(99))));
        assert_eq!(state.view.layout.len(), 2);
        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(7))],
            egui::pos2(10.0, 20.0)
        );
        assert!(state.edge_start.is_none());
    }
}

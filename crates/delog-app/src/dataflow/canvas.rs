use std::collections::HashSet;

use delog_flow::graph::{Graph, Node, NodeId, NodeKind, PortType};
use egui_graph::{SocketKind, node::EdgeEvent};

use super::canvas_state::{
    EdgeStart, GraphCanvasState, complete_connection, domain_node_id, ui_node_id,
};

pub type CanvasState = GraphCanvasState;

const MIN_CANVAS_ZOOM: f32 = 0.25;
const MAX_CANVAS_ZOOM: f32 = 2.5;
const FIT_PADDING_POINTS: f32 = 32.0;
const MAX_FIT_ZOOM: f32 = 1.0;

#[derive(Debug)]
pub enum CanvasEvent {
    Select(Option<NodeId>),
    Moved {
        id: NodeId,
        from: [f32; 2],
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
    DisconnectMany {
        endpoints: Vec<(NodeId, u32)>,
    },
    Delete(NodeId),
    OpenAddMenu {
        canvas_pos: [f32; 2],
        screen_pos: egui::Pos2,
    },
    EditKind {
        id: NodeId,
        kind: NodeKind,
    },
}

pub fn show_canvas(
    ui: &mut egui::Ui,
    graph: &Graph,
    selection: Option<NodeId>,
    state: &mut CanvasState,
) -> Vec<CanvasEvent> {
    let canvas_size = ui.available_size();
    state.prepare(graph, canvas_size);
    if state.fit_requested() {
        let node_bounds = if graph.nodes.is_empty() {
            None
        } else {
            measured_node_bounds(ui.ctx(), graph, &state.view.layout)
        };
        let fitted = node_bounds.and_then(|bounds| fitted_scene_rect(bounds, canvas_size));
        state.apply_fit_request(graph, canvas_size, fitted);
    }
    state
        .selected_edges
        .retain(|key| graph.edges.iter().any(|edge| edge_key(edge) == *key));

    let selected_nodes: HashSet<_> = selection.into_iter().map(ui_node_id).collect();
    let mut events = Vec::new();
    let edge_start = &mut state.edge_start;
    let selected_edges = &mut state.selected_edges;
    let mut node_contains_pointer = false;
    let mut socket_contains_pointer = false;
    let mut edge_contains_pointer = false;
    let mut socket_secondary_clicked = false;
    let secondary_clicked =
        ui.input(|input| input.pointer.button_clicked(egui::PointerButton::Secondary));
    let response = egui_graph::Graph::new("dataflow-canvas")
        .dot_grid(true)
        .zoom_range(MIN_CANVAS_ZOOM..=MAX_CANVAS_ZOOM)
        .snap(None)
        .align(true)
        .resize_behavior(egui_graph::ResizeBehavior::MaintainView)
        .selected_nodes(selected_nodes)
        .show(&mut state.view, ui, |ui, show| {
            show.nodes(ui, |node_context, ui| {
                for node in &graph.nodes {
                    let inputs = node.kind.inputs();
                    let outputs = node.kind.outputs();
                    let mut edited = node.kind.clone();
                    let node_response = egui_graph::node::Node::from_id(ui_node_id(node.id))
                        .inputs(inputs.len())
                        .outputs(outputs.len())
                        .flow(egui::Direction::LeftToRight)
                        .socket_radius(5.0)
                        .socket_color(socket_color(node))
                        .max_width(220.0)
                        .animation_time(0.0)
                        .show(node_context, ui, |context| {
                            context.framed(|ui, sockets| {
                                show_node_contents(
                                    ui,
                                    sockets,
                                    node,
                                    &inputs,
                                    &outputs,
                                    &mut edited,
                                )
                            })
                        });

                    let content = node_response.inner();
                    node_contains_pointer |= node_response.contains_pointer();
                    let mut hit_socket = None;
                    for (index, response) in node_response.sockets().inputs() {
                        if response.contains_pointer() {
                            socket_contains_pointer = true;
                            hit_socket.get_or_insert((SocketKind::Input, index));
                        }
                    }
                    for (index, response) in node_response.sockets().outputs() {
                        if response.contains_pointer() {
                            socket_contains_pointer = true;
                            hit_socket.get_or_insert((SocketKind::Output, index));
                        }
                    }
                    if secondary_clicked
                        && !socket_secondary_clicked
                        && let Some((kind, index)) = hit_socket
                    {
                        socket_secondary_clicked = true;
                        let endpoints =
                            disconnect_endpoints_for_socket(graph, node.id, kind, index);
                        if !endpoints.is_empty() {
                            events.push(CanvasEvent::DisconnectMany { endpoints });
                        }
                    }
                    if content.changed {
                        events.push(CanvasEvent::EditKind {
                            id: node.id,
                            kind: edited,
                        });
                    }
                    if content.delete || node_response.removed() {
                        events.push(CanvasEvent::Delete(node.id));
                    }
                    if let Some(edge_event) = node_response.edge_event() {
                        handle_edge_event(node.id, edge_event, edge_start, &mut events);
                    }
                }
            })
            .edges(ui, |edge_context, ui| {
                for edge in &graph.edges {
                    let key = edge_key(edge);
                    let mut selected = selected_edges.contains(&key);
                    let edge_response = egui_graph::edge::Edge::new(
                        (ui_node_id(edge.from), edge.from_port as usize),
                        (ui_node_id(edge.to), edge.to_port as usize),
                        &mut selected,
                    )
                    .show(edge_context, ui);
                    edge_contains_pointer |= edge_response.contains_pointer();

                    if edge_response.deleted() {
                        selected_edges.remove(&key);
                        events.push(CanvasEvent::Disconnect {
                            to: edge.to,
                            to_port: edge.to_port,
                        });
                    } else if selected {
                        selected_edges.insert(key);
                    } else {
                        selected_edges.remove(&key);
                    }
                }

                if let Some(edge) = edge_context.in_progress(ui) {
                    edge.show(ui, 0.5);
                }
            });
        });

    if let Some(selected) = response.selection_changed {
        let selected = selected.into_iter().min().map(domain_node_id);
        events.push(CanvasEvent::Select(selected));
    }
    let occupied = node_contains_pointer || socket_contains_pointer || edge_contains_pointer;
    if empty_canvas_double_clicked(response.response.double_clicked(), occupied) {
        state.request_fit();
    }
    if should_open_add_menu(
        response.response.secondary_clicked(),
        socket_secondary_clicked,
    ) && let Some(canvas_pos) = response.response.interact_pointer_pos()
        && let Some(global_pointer_pos) = ui.input(|input| input.pointer.interact_pos())
    {
        let (canvas_pos, screen_pos) = add_menu_positions(canvas_pos, global_pointer_pos);
        events.push(CanvasEvent::OpenAddMenu {
            canvas_pos,
            screen_pos,
        });
    }
    if needs_node_wheel_fallback(node_contains_pointer, response.response.contains_pointer()) {
        let (pointer, zoom_delta, pan_delta) = ui.input(|input| {
            (
                input.pointer.latest_pos(),
                input.zoom_delta(),
                input.smooth_scroll_delta(),
            )
        });
        if let Some(pointer) = pointer
            && (zoom_delta != 1.0 || pan_delta != egui::Vec2::ZERO)
        {
            apply_node_wheel(
                &mut state.view,
                response.response.rect,
                pointer,
                zoom_delta,
                pan_delta,
            );
        }
    }

    let primary_down = ui.input(|input| input.pointer.primary_down());
    events.extend(
        state
            .finish(graph, canvas_size, primary_down)
            .into_iter()
            .map(|moved| CanvasEvent::Moved {
                id: moved.id,
                from: moved.from,
                to: moved.to,
            }),
    );
    events
}

#[derive(Default)]
struct NodeContentResponse {
    changed: bool,
    delete: bool,
}

fn node_title(kind: &NodeKind) -> String {
    match kind {
        NodeKind::DataField(selector) => match selector.instance {
            Some(instance) => format!("{}[{instance}].{}", selector.topic, selector.field),
            None => format!("{}.{}", selector.topic, selector.field),
        },
        NodeKind::Output(spec) if !spec.topic.trim().is_empty() => spec.topic.clone(),
        NodeKind::Output(_) => "Output".to_owned(),
        NodeKind::Unknown(_) => "Unknown node".to_owned(),
        _ => kind.label(),
    }
}

fn port_type_label(port_type: PortType) -> &'static str {
    match port_type {
        PortType::Signal => "Signal",
        PortType::Scalar => "Scalar",
    }
}

fn show_node_contents(
    ui: &mut egui::Ui,
    sockets: &mut egui_graph::SocketLayout,
    node: &Node,
    inputs: &[delog_flow::graph::PortSpec],
    outputs: &[delog_flow::graph::PortSpec],
    edited: &mut NodeKind,
) -> NodeContentResponse {
    let mut result = NodeContentResponse::default();
    ui.horizontal(|ui| {
        ui.strong(node_title(&node.kind));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let icon_size = egui::Vec2::splat(ui.spacing().icon_width);
            let close = egui::Image::new(crate::icons::close())
                .fit_to_exact_size(icon_size)
                .tint(ui.visuals().text_color());
            result.delete = ui
                .add(egui::Button::image(close).frame(false))
                .on_hover_text("Remove node")
                .clicked();
        });
    });
    ui.separator();

    for index in 0..inputs.len().max(outputs.len()) {
        let input = inputs.get(index);
        let output = outputs.get(index);
        sockets.row(ui, input.map(|_| index), output.map(|_| index), |ui| {
            ui.horizontal(|ui| {
                if let Some(input) = input {
                    ui.label(&input.name);
                }
                if let Some(output) = output {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(port_type) = output.accepts.first().copied() {
                            ui.label(port_type_label(port_type));
                        }
                        if !output.name.is_empty() {
                            ui.label(&output.name);
                        }
                    });
                }
            });
        });
    }

    result.changed = match edited {
        NodeKind::Constant { value } => ui.add(egui::DragValue::new(value)).changed(),
        NodeKind::ScaleOffset { multiplier, offset } => {
            let multiplier_changed = ui
                .add(egui::DragValue::new(multiplier).prefix("x "))
                .changed();
            let offset_changed = ui.add(egui::DragValue::new(offset).prefix("+ ")).changed();
            multiplier_changed || offset_changed
        }
        #[cfg(feature = "scripting")]
        NodeKind::Script(spec) => {
            ui.weak(code_summary_line(&spec.code));
            false
        }
        _ => false,
    };
    result
}

/// A short, single-line summary of a script's code shown in the node body;
/// full editing happens in the inspector.
#[cfg(feature = "scripting")]
fn code_summary_line(code: &str) -> String {
    match code.lines().find(|line| !line.trim().is_empty()) {
        Some(line) => {
            let line = line.trim();
            if line.chars().count() > 40 {
                format!("{}\u{2026}", line.chars().take(40).collect::<String>())
            } else {
                line.to_owned()
            }
        }
        None => "(empty script)".to_owned(),
    }
}

fn handle_edge_event(
    node: NodeId,
    event: EdgeEvent,
    edge_start: &mut Option<EdgeStart>,
    events: &mut Vec<CanvasEvent>,
) {
    match event {
        EdgeEvent::Started { kind, index } => {
            *edge_start = Some(EdgeStart {
                node,
                kind,
                port: index,
            });
        }
        EdgeEvent::Ended { kind, index } => {
            let end = EdgeStart {
                node,
                kind,
                port: index,
            };
            if let Some(start) = edge_start.take()
                && let Some(event) = connection_event(start, end)
            {
                events.push(event);
            }
        }
        EdgeEvent::Cancelled => *edge_start = None,
    }
}

fn connection_event(first: EdgeStart, second: EdgeStart) -> Option<CanvasEvent> {
    let (from, from_port, to, to_port) = complete_connection(first, second)?;
    Some(CanvasEvent::Connect {
        from,
        from_port,
        to,
        to_port,
    })
}

fn socket_color(node: &Node) -> egui::Color32 {
    match node
        .kind
        .outputs()
        .first()
        .and_then(|output| output.accepts.first().copied())
        .or_else(|| node.kind.inputs().first()?.accepts.first().copied())
    {
        Some(PortType::Signal) => egui::Color32::from_rgb(90, 180, 235),
        Some(PortType::Scalar) => egui::Color32::from_rgb(235, 180, 90),
        None => egui::Color32::GRAY,
    }
}

fn empty_canvas_double_clicked(double_clicked: bool, occupied: bool) -> bool {
    double_clicked && !occupied
}

fn should_open_add_menu(canvas_secondary_clicked: bool, socket_secondary_clicked: bool) -> bool {
    canvas_secondary_clicked && !socket_secondary_clicked
}

fn disconnect_endpoints_for_socket(
    graph: &Graph,
    node: NodeId,
    kind: SocketKind,
    index: usize,
) -> Vec<(NodeId, u32)> {
    match kind {
        SocketKind::Input => u32::try_from(index)
            .ok()
            .filter(|&port| graph.incoming(node, port).is_some())
            .map(|port| vec![(node, port)])
            .unwrap_or_default(),
        SocketKind::Output => u32::try_from(index)
            .ok()
            .map(|port| {
                graph
                    .edges
                    .iter()
                    .filter(|edge| edge.from == node && edge.from_port == port)
                    .map(|edge| (edge.to, edge.to_port))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn edge_key(edge: &delog_flow::graph::Edge) -> (NodeId, NodeId, u32) {
    (edge.from, edge.to, edge.to_port)
}

fn add_menu_positions(
    canvas_pos: egui::Pos2,
    global_pointer_pos: egui::Pos2,
) -> ([f32; 2], egui::Pos2) {
    (
        [canvas_pos.x, canvas_pos.y],
        global_pointer_pos + egui::vec2(8.0, 8.0),
    )
}

fn needs_node_wheel_fallback(node_contains_pointer: bool, scene_contains_pointer: bool) -> bool {
    node_contains_pointer && !scene_contains_pointer
}

fn valid_navigation_rect(rect: egui::Rect) -> bool {
    let size = rect.size();
    rect.is_finite() && size.is_finite() && size.x > 0.0 && size.y > 0.0
}

fn fitted_scene_rect(node_bounds: egui::Rect, canvas_size: egui::Vec2) -> Option<egui::Rect> {
    if !valid_navigation_rect(node_bounds)
        || !canvas_size.is_finite()
        || canvas_size.x <= 2.0 * FIT_PADDING_POINTS
        || canvas_size.y <= 2.0 * FIT_PADDING_POINTS
    {
        return None;
    }
    let usable = canvas_size - egui::Vec2::splat(2.0 * FIT_PADDING_POINTS);
    let zoom = (usable / node_bounds.size())
        .min_elem()
        .clamp(MIN_CANVAS_ZOOM, MAX_FIT_ZOOM);
    let scene_rect = egui::Rect::from_center_size(node_bounds.center(), canvas_size / zoom);
    valid_navigation_rect(scene_rect).then_some(scene_rect)
}

fn measured_node_bounds(
    ctx: &egui::Context,
    graph: &Graph,
    layout: &egui_graph::Layout,
) -> Option<egui::Rect> {
    egui_graph::with_graph_memory(ctx, egui_graph::id("dataflow-canvas"), |memory| {
        graph
            .nodes
            .iter()
            .try_fold(egui::Rect::NOTHING, |bounds, node| {
                let id = ui_node_id(node.id);
                let position = *layout.get(&id)?;
                let size = *memory.node_sizes().get(&id)?;
                let rect = egui::Rect::from_min_size(position, size);
                valid_navigation_rect(rect).then_some(bounds.union(rect))
            })
    })
}

fn scene_point_at_pointer(
    scene_rect: egui::Rect,
    canvas_rect: egui::Rect,
    pointer: egui::Pos2,
) -> egui::Pos2 {
    let zoom = (canvas_rect.size() / scene_rect.size())
        .min_elem()
        .clamp(MIN_CANVAS_ZOOM, MAX_CANVAS_ZOOM);
    scene_rect.center() + (pointer - canvas_rect.center()) / zoom
}

fn apply_node_wheel(
    view: &mut egui_graph::View,
    canvas_rect: egui::Rect,
    pointer: egui::Pos2,
    zoom_delta: f32,
    pan_delta: egui::Vec2,
) {
    if !valid_navigation_rect(canvas_rect)
        || !valid_navigation_rect(view.scene_rect)
        || !pointer.is_finite()
        || !zoom_delta.is_finite()
        || zoom_delta <= 0.0
        || !pan_delta.is_finite()
    {
        return;
    }
    let current_zoom = (canvas_rect.size() / view.scene_rect.size())
        .min_elem()
        .clamp(MIN_CANVAS_ZOOM, MAX_CANVAS_ZOOM);
    let target_zoom = (current_zoom * zoom_delta).clamp(MIN_CANVAS_ZOOM, MAX_CANVAS_ZOOM);
    let anchor = scene_point_at_pointer(view.scene_rect, canvas_rect, pointer);
    let new_size = canvas_rect.size() / target_zoom;
    let mut new_min = anchor + (canvas_rect.min - pointer) / target_zoom;
    new_min -= pan_delta / target_zoom;
    let new_scene_rect = egui::Rect::from_min_size(new_min, new_size);
    if valid_navigation_rect(new_scene_rect) {
        view.scene_rect = new_scene_rect;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataflow::canvas_state::EdgeStart;
    use egui_graph::SocketKind;

    fn render_canvas_frame(
        ctx: &egui::Context,
        graph: &Graph,
        state: &mut CanvasState,
        size: egui::Vec2,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            let _ = show_canvas(ui, graph, None, state);
        });
    }

    fn rendered_node_bounds(ctx: &egui::Context, graph: &Graph, state: &CanvasState) -> egui::Rect {
        egui_graph::with_graph_memory(ctx, egui_graph::id("dataflow-canvas"), |memory| {
            graph
                .nodes
                .iter()
                .fold(egui::Rect::NOTHING, |bounds, node| {
                    let id = ui_node_id(node.id);
                    bounds.union(egui::Rect::from_min_size(
                        state.view.layout[&id],
                        memory.node_sizes()[&id],
                    ))
                })
        })
    }

    fn fan_out_test_graph() -> Graph {
        let mut graph = Graph::new("fan-out");
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [0.0, 0.0],
            kind: NodeKind::DataField(delog_flow::graph::FieldSelector {
                source: None,
                topic: "signal".to_owned(),
                instance: None,
                field: "value".to_owned(),
            }),
        });
        for (id, y) in [(NodeId(2), 0.0), (NodeId(3), 100.0)] {
            graph.insert_node(Node {
                id,
                pos: [200.0, y],
                kind: NodeKind::ScaleOffset {
                    multiplier: 1.0,
                    offset: 0.0,
                },
            });
            graph.connect(NodeId(1), 0, id, 0).unwrap();
        }
        graph
    }

    #[test]
    fn double_click_fits_only_an_empty_canvas() {
        assert!(empty_canvas_double_clicked(true, false));
        assert!(!empty_canvas_double_clicked(true, true));
        assert!(!empty_canvas_double_clicked(false, false));
    }

    #[test]
    fn add_menu_opens_only_for_an_unconsumed_canvas_secondary_click() {
        assert!(should_open_add_menu(true, false));
        assert!(!should_open_add_menu(true, true));
        assert!(!should_open_add_menu(false, false));
    }

    #[test]
    fn input_socket_resolves_its_single_incoming_endpoint() {
        let graph = fan_out_test_graph();
        assert_eq!(
            disconnect_endpoints_for_socket(&graph, NodeId(2), SocketKind::Input, 0),
            vec![(NodeId(2), 0)],
        );
    }

    #[test]
    fn output_socket_resolves_every_outgoing_endpoint() {
        let graph = fan_out_test_graph();
        assert_eq!(
            disconnect_endpoints_for_socket(&graph, NodeId(1), SocketKind::Output, 0),
            vec![(NodeId(2), 0), (NodeId(3), 0)],
        );
    }

    #[test]
    fn unconnected_and_unsupported_sockets_resolve_no_endpoints() {
        let graph = fan_out_test_graph();
        assert!(
            disconnect_endpoints_for_socket(&graph, NodeId(3), SocketKind::Output, 0).is_empty()
        );
        assert!(
            disconnect_endpoints_for_socket(&graph, NodeId(1), SocketKind::Output, 1).is_empty()
        );
    }

    #[test]
    fn repeated_fit_is_deterministic_padded_and_does_not_move_nodes() {
        let ctx = egui::Context::default();
        let mut graph = Graph::new("fit");
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [-400.0, -200.0],
            kind: NodeKind::Constant { value: 1.0 },
        });
        graph.insert_node(Node {
            id: NodeId(2),
            pos: [500.0, 300.0],
            kind: NodeKind::Add,
        });
        let mut state = CanvasState::default();
        let size = egui::vec2(800.0, 600.0);

        render_canvas_frame(&ctx, &graph, &mut state, size);
        let layout_before_fit = state.view.layout.clone();
        state.request_fit();
        render_canvas_frame(&ctx, &graph, &mut state, size);
        let first_fit = state.view.scene_rect;
        state.request_fit();
        render_canvas_frame(&ctx, &graph, &mut state, size);
        let second_fit = state.view.scene_rect;
        let rendered_bounds = rendered_node_bounds(&ctx, &graph, &state);

        assert_eq!(second_fit, first_fit);
        assert_eq!(state.view.layout, layout_before_fit);
        assert_eq!(state.viewport.offset, [second_fit.min.x, second_fit.min.y]);
        assert!((MIN_CANVAS_ZOOM..=1.0).contains(&state.viewport.zoom));

        let zoom = state.viewport.zoom;
        assert!((rendered_bounds.left() - second_fit.left()) * zoom >= 31.5);
        assert!((second_fit.right() - rendered_bounds.right()) * zoom >= 31.5);
        assert!((rendered_bounds.top() - second_fit.top()) * zoom >= 31.5);
        assert!((second_fit.bottom() - rendered_bounds.bottom()) * zoom >= 31.5);
    }

    #[test]
    fn fit_does_not_zoom_a_small_graph_above_default() {
        let ctx = egui::Context::default();
        let mut graph = Graph::new("small");
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [20.0, 30.0],
            kind: NodeKind::Constant { value: 1.0 },
        });
        let mut state = CanvasState::default();
        let size = egui::vec2(800.0, 600.0);

        render_canvas_frame(&ctx, &graph, &mut state, size);
        state.request_fit();
        render_canvas_frame(&ctx, &graph, &mut state, size);

        assert_eq!(state.viewport.zoom, 1.0);
        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(1))],
            egui::pos2(20.0, 30.0)
        );
    }

    #[test]
    fn resize_and_repaint_preserve_app_owned_viewport_and_positions() {
        let ctx = egui::Context::default();
        let mut graph = Graph::new("resize");
        graph.viewport = delog_flow::graph::Viewport {
            offset: [-100.0, -50.0],
            zoom: 0.8,
        };
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [20.0, 30.0],
            kind: NodeKind::Constant { value: 1.0 },
        });
        let mut state = CanvasState::default();

        render_canvas_frame(&ctx, &graph, &mut state, egui::vec2(800.0, 600.0));
        render_canvas_frame(&ctx, &graph, &mut state, egui::vec2(960.0, 720.0));
        let after_resize = state.viewport;
        let layout_after_resize = state.view.layout.clone();
        render_canvas_frame(&ctx, &graph, &mut state, egui::vec2(960.0, 720.0));

        assert_eq!(after_resize, graph.viewport);
        assert_eq!(state.viewport, after_resize);
        assert_eq!(state.view.layout, layout_after_resize);
        assert_eq!(
            state.view.layout[&ui_node_id(NodeId(1))],
            egui::pos2(20.0, 30.0)
        );
    }

    #[test]
    fn wheel_fallback_runs_only_when_a_node_owns_the_pointer() {
        assert!(needs_node_wheel_fallback(true, false));
        assert!(!needs_node_wheel_fallback(false, true));
        assert!(!needs_node_wheel_fallback(true, true));
        assert!(!needs_node_wheel_fallback(false, false));
    }

    #[test]
    fn node_wheel_pans_and_keeps_zoom_anchored_to_pointer() {
        let canvas = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 600.0));
        let pointer = egui::pos2(300.0, 200.0);
        let mut view = egui_graph::View {
            scene_rect: egui::Rect::from_min_size(
                egui::pos2(-200.0, -100.0),
                egui::vec2(800.0, 600.0),
            ),
            ..Default::default()
        };
        let before = scene_point_at_pointer(view.scene_rect, canvas, pointer);

        apply_node_wheel(&mut view, canvas, pointer, 2.0, egui::Vec2::ZERO);

        let after = scene_point_at_pointer(view.scene_rect, canvas, pointer);
        assert!((before - after).length() < 0.001);
        assert_eq!(view.scene_rect.size(), egui::vec2(400.0, 300.0));

        apply_node_wheel(&mut view, canvas, pointer, 1.0, egui::vec2(20.0, -10.0));
        assert_eq!(view.scene_rect.min, egui::pos2(-110.0, -20.0));
    }

    #[test]
    fn node_wheel_clamps_zoom_to_canvas_range() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut view = egui_graph::View {
            scene_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
            ..Default::default()
        };

        apply_node_wheel(&mut view, canvas, canvas.center(), 100.0, egui::Vec2::ZERO);
        assert_eq!(view.scene_rect.size(), egui::vec2(320.0, 240.0));
        apply_node_wheel(&mut view, canvas, canvas.center(), 0.0001, egui::Vec2::ZERO);
        assert_eq!(view.scene_rect.size(), egui::vec2(3200.0, 2400.0));
    }

    #[test]
    fn node_wheel_keeps_pointer_anchored_with_mismatched_aspect_ratios() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let pointer = egui::pos2(100.0, 100.0);
        let mut view = egui_graph::View {
            scene_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 600.0)),
            ..Default::default()
        };
        let current_zoom = (canvas.size() / view.scene_rect.size()).min_elem();
        let expected_anchor = view.scene_rect.center() + (pointer - canvas.center()) / current_zoom;

        apply_node_wheel(&mut view, canvas, pointer, 2.0, egui::Vec2::ZERO);

        let after = scene_point_at_pointer(view.scene_rect, canvas, pointer);
        assert_eq!(expected_anchor, egui::pos2(-100.0, 100.0));
        assert!((expected_anchor - after).length() < 0.001);
    }

    #[test]
    fn node_wheel_uses_clamped_scene_scale_for_pointer_anchor() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let pointer = egui::pos2(600.0, 300.0);
        let mut view = egui_graph::View {
            scene_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(8000.0, 6000.0)),
            ..Default::default()
        };
        let expected_anchor =
            view.scene_rect.center() + (pointer - canvas.center()) / MIN_CANVAS_ZOOM;

        apply_node_wheel(&mut view, canvas, pointer, 2.0, egui::Vec2::ZERO);

        let after = scene_point_at_pointer(view.scene_rect, canvas, pointer);
        assert_eq!(expected_anchor, egui::pos2(4800.0, 3000.0));
        assert!((expected_anchor - after).length() < 0.001);
    }

    #[test]
    fn node_wheel_rejects_non_finite_and_degenerate_input() {
        let valid_canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let valid_scene =
            egui::Rect::from_min_size(egui::pos2(-200.0, -100.0), egui::vec2(800.0, 600.0));
        let valid_pointer = egui::pos2(300.0, 200.0);
        let invalid_cases = [
            (
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(f32::NAN, 600.0)),
                valid_scene,
                valid_pointer,
                2.0,
                egui::Vec2::ZERO,
            ),
            (
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(0.0, 600.0)),
                valid_scene,
                valid_pointer,
                2.0,
                egui::Vec2::ZERO,
            ),
            (
                valid_canvas,
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(0.0, 600.0)),
                valid_pointer,
                2.0,
                egui::Vec2::ZERO,
            ),
            (
                valid_canvas,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(f32::INFINITY, 600.0)),
                valid_pointer,
                2.0,
                egui::Vec2::ZERO,
            ),
            (
                valid_canvas,
                valid_scene,
                egui::pos2(f32::NAN, 200.0),
                2.0,
                egui::Vec2::ZERO,
            ),
            (
                valid_canvas,
                valid_scene,
                valid_pointer,
                0.0,
                egui::Vec2::ZERO,
            ),
            (
                valid_canvas,
                valid_scene,
                valid_pointer,
                f32::NAN,
                egui::Vec2::ZERO,
            ),
            (
                valid_canvas,
                valid_scene,
                valid_pointer,
                2.0,
                egui::vec2(f32::INFINITY, 0.0),
            ),
        ];

        for (canvas, scene, pointer, zoom_delta, pan_delta) in invalid_cases {
            let mut view = egui_graph::View {
                scene_rect: scene,
                ..Default::default()
            };
            let before = view.scene_rect;

            apply_node_wheel(&mut view, canvas, pointer, zoom_delta, pan_delta);

            assert_eq!(view.scene_rect, before);
        }
    }

    #[test]
    fn data_titles_include_topic_instance_and_field_but_not_source() {
        let instanced = NodeKind::DataField(delog_flow::graph::FieldSelector {
            source: Some("flight-a".to_owned()),
            topic: "IMU".to_owned(),
            instance: Some(0),
            field: "GyrX".to_owned(),
        });
        let uninstanced = NodeKind::DataField(delog_flow::graph::FieldSelector {
            source: Some("flight-b".to_owned()),
            topic: "GPS".to_owned(),
            instance: None,
            field: "Lat".to_owned(),
        });

        assert_eq!(node_title(&instanced), "IMU[0].GyrX");
        assert_eq!(node_title(&uninstanced), "GPS.Lat");
    }

    #[test]
    fn operation_output_and_unknown_titles_follow_canvas_policy() {
        assert_eq!(node_title(&NodeKind::Add), "Add");
        assert_eq!(
            node_title(&NodeKind::Align {
                mode: delog_core::align::AlignMode::Nearest,
            }),
            "Align to Timeline"
        );
        assert_eq!(
            node_title(&NodeKind::Output(delog_flow::graph::OutputSpec {
                topic: "derived_attitude".to_owned(),
                fields: Vec::new(),
            })),
            "derived_attitude"
        );
        assert_eq!(
            node_title(&NodeKind::Output(delog_flow::graph::OutputSpec {
                topic: "   ".to_owned(),
                fields: Vec::new(),
            })),
            "Output"
        );
        assert_eq!(
            node_title(&NodeKind::Unknown(serde_json::json!({}))),
            "Unknown node"
        );
    }

    #[test]
    fn output_labels_name_the_port_type() {
        assert_eq!(port_type_label(PortType::Signal), "Signal");
        assert_eq!(port_type_label(PortType::Scalar), "Scalar");
    }

    #[test]
    fn dual_ended_port_row_keeps_sockets_on_one_compact_line() {
        let ctx = egui::Context::default();
        let mut graph = Graph::new("test");
        let node_id = NodeId(1);
        graph.insert_node(Node {
            id: node_id,
            pos: [0.0, 0.0],
            kind: NodeKind::Multiply,
        });
        let mut state = CanvasState::default();
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                show_canvas(ui, &graph, None, &mut state);
            },
        );

        let (first_input, second_input, output) =
            egui_graph::with_graph_memory(&ctx, egui_graph::id("dataflow-canvas"), |memory| {
                let sockets = &memory.node_sockets()[&ui_node_id(node_id)];
                (
                    sockets.input(0).unwrap().0,
                    sockets.input(1).unwrap().0,
                    sockets.output(0).unwrap().0,
                )
            });

        assert_eq!(first_input.y, output.y);
        let single_line_stride =
            ctx.global_style().spacing.interact_size.y + ctx.global_style().spacing.item_spacing.y;
        assert!(
            second_input.y - first_input.y <= single_line_stride,
            "dual-ended row used more than one line: first={first_input:?}, second={second_input:?}, output={output:?}"
        );
    }

    #[test]
    fn add_menu_screen_position_is_offset_from_global_pointer() {
        let (canvas_pos, screen_pos) =
            add_menu_positions(egui::pos2(40.0, -10.0), egui::pos2(700.0, 300.0));

        assert_eq!(canvas_pos, [40.0, -10.0]);
        assert_eq!(screen_pos, egui::pos2(708.0, 308.0));
    }

    #[test]
    fn add_menu_positions_keep_scene_and_screen_coordinates_independent() {
        let (canvas_pos, screen_pos) =
            add_menu_positions(egui::pos2(-250.0, 900.0), egui::pos2(120.0, 80.0));

        assert_eq!(canvas_pos, [-250.0, 900.0]);
        assert_eq!(screen_pos, egui::pos2(128.0, 88.0));
    }

    #[test]
    fn rejected_connection_does_not_emit_event() {
        let first = EdgeStart {
            node: NodeId(1),
            kind: SocketKind::Input,
            port: 0,
        };
        let second = EdgeStart {
            node: NodeId(2),
            kind: SocketKind::Input,
            port: 0,
        };

        assert!(connection_event(first, second).is_none());
    }

    #[test]
    fn accepted_connection_uses_domain_port_indices() {
        let mut graph = Graph::new("test");
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [0.0, 0.0],
            kind: NodeKind::Constant { value: 2.0 },
        });
        graph.insert_node(Node {
            id: NodeId(2),
            pos: [100.0, 0.0],
            kind: NodeKind::Multiply,
        });
        let output = EdgeStart {
            node: NodeId(1),
            kind: SocketKind::Output,
            port: 0,
        };
        let input = EdgeStart {
            node: NodeId(2),
            kind: SocketKind::Input,
            port: 1,
        };

        assert_eq!(graph.check_connect(NodeId(1), 0, NodeId(2), 1), Ok(()));

        assert!(matches!(
            connection_event(output, input),
            Some(CanvasEvent::Connect {
                from: NodeId(1),
                from_port: 0,
                to: NodeId(2),
                to_port: 1,
            })
        ));
    }

    #[test]
    fn invalid_domain_connection_is_forwarded_for_logging() {
        let mut graph = Graph::new("test");
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [0.0, 0.0],
            kind: NodeKind::Constant { value: 2.0 },
        });
        graph.insert_node(Node {
            id: NodeId(2),
            pos: [100.0, 0.0],
            kind: NodeKind::Add,
        });
        let output = EdgeStart {
            node: NodeId(1),
            kind: SocketKind::Output,
            port: 0,
        };
        let input = EdgeStart {
            node: NodeId(2),
            kind: SocketKind::Input,
            port: 0,
        };

        assert_eq!(
            graph.check_connect(NodeId(1), 0, NodeId(2), 0),
            Err(delog_flow::graph::ConnectError::TypeMismatch)
        );

        assert!(matches!(
            connection_event(output, input),
            Some(CanvasEvent::Connect {
                from: NodeId(1),
                from_port: 0,
                to: NodeId(2),
                to_port: 0,
            })
        ));
    }
}

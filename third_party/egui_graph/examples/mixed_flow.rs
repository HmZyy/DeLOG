//! Demonstrates mixed-directional flow layout.
//!
//! Run with `cargo run --example mixed_flow` (needs the `layout` feature,
//! which is on by default).
//!
//! A graph rarely flows in a single direction. Here a horizontal "signal
//! pipeline" (`Input -> EQ -> Reverb -> Output`) is modulated by two vertical
//! "control" chains (`LFO -> Depth` and `Env -> Amount`). Each node carries
//! its own flow via [`LayoutNode::flow`], so the auto-layout:
//!
//! - groups nodes that share a flow into clusters and lays each cluster out in
//!   its own direction (the pipeline runs left-to-right, the controls top-down);
//! - treats the edges crossing between flows as "cut" edges and arranges the
//!   clusters along the outer direction ([`LayoutParams::flow`]).
//!
//! Use the controls to change the modulation flow and the outer direction.
//! Set the modulation flow to match the pipeline (Right) and the cross-flow
//! edges vanish - the whole graph collapses into one left-to-right cluster.

use eframe::egui;
use egui_graph::{EdgeRoutes, LayoutNode, LayoutParams, NodeId, View};
use std::collections::HashMap;

fn main() -> Result<(), eframe::Error> {
    env_logger::init();
    let options = eframe::NativeOptions::default();
    let name = "egui_graph - mixed-flow layout";
    eframe::run_native(name, options, Box::new(|_cc| Ok(Box::new(App::new()))))
}

struct App {
    view: View,
    state: State,
}

struct State {
    nodes: Vec<GraphNode>,
    edges: Vec<Edge>,
    routes: EdgeRoutes,
    /// The flow direction of the modulation (control) nodes.
    mod_flow: egui::Direction,
    /// The outer direction along which the clusters are arranged.
    outer_flow: egui::Direction,
    center_view: bool,
}

/// One node of the example graph.
struct GraphNode {
    label: &'static str,
    role: Role,
    inputs: usize,
    outputs: usize,
}

/// A node's role decides its flow: the signal pipeline always flows
/// left-to-right, while the modulation sources follow [`State::mod_flow`].
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Pipeline,
    Modulation,
}

/// An edge as `((src node, output socket), (dst node, input socket))`.
type Edge = ((usize, usize), (usize, usize));

impl App {
    fn new() -> Self {
        let (nodes, edges) = example_graph();
        let state = State {
            nodes,
            edges,
            routes: EdgeRoutes::default(),
            mod_flow: egui::Direction::TopDown,
            outer_flow: egui::Direction::LeftToRight,
            center_view: true,
        };
        App {
            view: View::default(),
            state,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        relayout(&mut self.view, &mut self.state, ui.ctx());
        controls(ui, &mut self.state);
        graph(ui, &mut self.view, &self.state);
    }
}

/// The signal pipeline modulated by two control chains.
fn example_graph() -> (Vec<GraphNode>, Vec<Edge>) {
    use Role::{Modulation, Pipeline};
    let node = |label, role, inputs, outputs| GraphNode {
        label,
        role,
        inputs,
        outputs,
    };
    let nodes = vec![
        // 0..4: the left-to-right signal pipeline.
        node("Input", Pipeline, 0, 1),
        node("EQ", Pipeline, 2, 1),
        node("Reverb", Pipeline, 2, 1),
        node("Output", Pipeline, 1, 0),
        // 4..6: a top-down control chain feeding the EQ.
        node("LFO", Modulation, 0, 1),
        node("Depth", Modulation, 1, 1),
        // 6..8: a second top-down control chain feeding the Reverb.
        node("Env", Modulation, 0, 1),
        node("Amount", Modulation, 1, 1),
    ];
    let edges = vec![
        // The pipeline chain.
        ((0, 0), (1, 0)),
        ((1, 0), (2, 0)),
        ((2, 0), (3, 0)),
        // The two control chains.
        ((4, 0), (5, 0)),
        ((6, 0), (7, 0)),
        // Cross-flow cut edges from the controls into the pipeline.
        ((5, 0), (1, 1)),
        ((7, 0), (2, 1)),
    ];
    (nodes, edges)
}

/// A node's effective flow: pipeline nodes are fixed, controls follow
/// `mod_flow`.
fn flow_of(node: &GraphNode, mod_flow: egui::Direction) -> egui::Direction {
    match node.role {
        Role::Pipeline => egui::Direction::LeftToRight,
        Role::Modulation => mod_flow,
    }
}

fn graph_id() -> egui::Id {
    egui_graph::id("Mixed Flow Graph")
}

fn node_id(ix: usize) -> NodeId {
    NodeId::from_u64(ix as u64)
}

/// Recompute the layout and edge routes for the current flow settings.
fn relayout(view: &mut View, state: &mut State, ctx: &egui::Context) {
    let socket_padding = egui_graph::socket_padding(&ctx.global_style());
    // Node sizes come from the previous frame's rendered frames.
    let layout_nodes: Vec<(NodeId, LayoutNode)> =
        egui_graph::with_graph_memory(ctx, graph_id(), |gmem| {
            let sizes = gmem.node_sizes();
            state
                .nodes
                .iter()
                .enumerate()
                .map(|(ix, n)| {
                    let id = node_id(ix);
                    let size = sizes
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| [120.0, 48.0].into());
                    let node = LayoutNode::new(size)
                        .socket_padding(socket_padding)
                        .inputs(n.inputs)
                        .outputs(n.outputs)
                        .flow(flow_of(n, state.mod_flow));
                    (id, node)
                })
                .collect()
        });
    let edges = state
        .edges
        .iter()
        .map(|&((s, so), (d, di))| ((node_id(s), so), (node_id(d), di)));
    // `outer_flow` is both the default node flow and the direction the
    // clusters are arranged along.
    let params = LayoutParams::new(state.outer_flow);
    let (layout, routes) = egui_graph::layout_routed(layout_nodes, edges, params);
    view.layout = layout;
    state.routes = routes;
}

fn graph(ui: &mut egui::Ui, view: &mut View, state: &State) {
    egui_graph::Graph::from_id(graph_id())
        .center_view(state.center_view)
        .dot_grid(true)
        .immutable(true)
        .show(view, ui, |ui, show| {
            show.nodes(ui, |nctx, ui| nodes(nctx, ui, state))
                .edges(ui, |ectx, ui| edges(ectx, ui, state));
        });
}

fn nodes(nctx: &mut egui_graph::NodesCtx, ui: &mut egui::Ui, state: &State) {
    for (ix, n) in state.nodes.iter().enumerate() {
        let flow = flow_of(n, state.mod_flow);
        egui_graph::node::Node::from_id(node_id(ix))
            .inputs(n.inputs)
            .outputs(n.outputs)
            .flow(flow)
            .show(nctx, ui, |node_ctx| {
                node_ctx.framed(|ui, _sockets| {
                    ui.label(n.label);
                })
            });
    }
}

fn edges(ectx: &mut egui_graph::EdgesCtx, ui: &mut egui::Ui, state: &State) {
    // Multiple edges between the same socket pair each get their own route.
    let mut occurrences: HashMap<((NodeId, usize), (NodeId, usize)), usize> = HashMap::new();
    for &((s, so), (d, di)) in &state.edges {
        let (a, b) = ((node_id(s), so), (node_id(d), di));
        let occurrence = occurrences.entry((a, b)).or_default();
        let waypoints = state.routes.route(a, b, *occurrence).unwrap_or(&[]);
        *occurrence += 1;
        let mut selected = false;
        egui_graph::edge::Edge::new(a, b, &mut selected)
            .waypoints(waypoints)
            .show(ectx, ui);
    }
}

fn controls(ui: &mut egui::Ui, state: &mut State) {
    let mut frame = egui::Frame::window(ui.style());
    frame.shadow.spread = 0;
    frame.shadow.offset = [0, 0];
    egui::Window::new("Mixed Flow")
        .frame(frame)
        .anchor(
            egui::Align2::LEFT_TOP,
            ui.spacing().window_margin.left_top(),
        )
        .collapsible(false)
        .title_bar(false)
        .auto_sized()
        .show(ui.ctx(), |ui| {
            ui.label("MIXED-FLOW LAYOUT");
            ui.label(
                "The signal pipeline flows left-to-right; the control chains\n\
                 flow independently. Cross-flow edges split the graph into\n\
                 clusters arranged along the outer direction.",
            );
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Control flow:");
                ui.radio_value(&mut state.mod_flow, egui::Direction::TopDown, "Down");
                ui.radio_value(&mut state.mod_flow, egui::Direction::LeftToRight, "Right");
            });
            ui.horizontal(|ui| {
                ui.label("Outer flow:");
                ui.radio_value(&mut state.outer_flow, egui::Direction::LeftToRight, "Right");
                ui.radio_value(&mut state.outer_flow, egui::Direction::TopDown, "Down");
            });
            ui.checkbox(&mut state.center_view, "Center View");
            if state.mod_flow == egui::Direction::LeftToRight {
                ui.label("Controls now share the pipeline's flow: one cluster.");
            }
        });
}

# egui_graph

A general-purpose node graph widget for [egui](https://github.com/emilk/egui).

Build interactive node-based editors with nodes connected by edges for visual
programming interfaces, shader editors, DSP graphs, or any graph-based UI.

**Note:** This library is the basis for
[nannou-org/gantz](https://github.com/nannou-org/gantz). For a more
sophisticated example of what can be built with egui_graph, check out
[gantz](https://nannou-org.github.io/gantz).

## Key Design Philosophy

One of the core design decisions of `egui_graph` is to avoid requiring that
users model their graph with any particular data structure. The library provides
immediate-mode widgets for rendering and interacting with graphs, but leaves
the underlying data model up to you. Store your graph however makes sense for
your application - whether that's an adjacency list, entity-component system,
or any other representation.

## Features

- **Interactive Nodes**: Drag, select, and delete nodes with intuitive controls
- **Edge Creation**: Connect nodes via input/output sockets with bezier curve edges
- **Multi-Selection**: Rectangle selection and Ctrl+click for selecting multiple nodes
- **Automatic Layout**: Optional socket-aware layered graph layout
- **Customizable**: Configure node flow direction, socket/frame appearance, and more
- **Zoom & Pan**: Navigate large graphs with mouse controls
- **Model-Agnostic**: No prescribed graph data structure - use whatever fits your needs

## Quick Start

```rust
use egui_graph::{Graph, View, Node, NodeId, Edge};

// Create a view to store node and "camera" positions.
let mut view = View::default();

// Show the graph widget
Graph::new("my_graph")
    .show(&mut view, ui, |ui, mut show| {
        // Add nodes to the graph
        show.nodes(|nctx, ui| {
            Node::new("node_1")
                .inputs(2)
                .outputs(1)
                .show(nctx, ui, |node_ctx| {
                    node_ctx.framed(|ui| {
                        ui.label("My Node");
                    })
                })
        });

        // Add edges between nodes
        show.edges(|ectx, ui| {
            let selected = false;
            Edge::new(
                (NodeId::new("node_1"), 0),  // From output 0
                (NodeId::new("node_2"), 1),  // To input 1
                &mut selected
            ).show(ectx, ui);
        });
    });
```

Visit the demo.rs example for a more thorough, up-to-date example.

## Core Components

### Graph Widget

The main widget that contains all nodes and edges:

```rust
Graph::new(id_source)
    .background(true)           // Enable background
    .dot_grid(true)             // Show dot grid
    .zoom_range(0.1..=2.0)      // Set zoom limits
    .center_view(true)          // Center the camera
    .show(&mut view, ui, |ui, show| { /* ... */ })
```

### Nodes

Nodes are containers with input/output sockets:

```rust
Node::new(id_source)
    .inputs(3)                  // Number of input sockets
    .outputs(2)                 // Number of output sockets
    .flow(Direction::LeftToRight) // Socket arrangement
    .socket_color(Color32::BLUE)
    .socket_radius(5.0)
    .show(ctx, ui, |node_ctx| {
        // Node content goes here
        node_ctx.framed(|ui| {
            ui.label("Node Content");
        })
    })
```

### Edges

Connect nodes with bezier curve edges:

```rust
Edge::new(
    (source_node_id, output_index),
    (target_node_id, input_index),
    &mut selected
)
.distance_per_point(1.0)  // Curve sampling distance
.show(ctx, ui)
```

### Automatic Layout

With the `layout` feature enabled, a built-in socket-aware layered layout
orders and positions nodes to minimise edge crossings and keep edges straight,
taking the socket each edge connects to into account:

```rust
use egui_graph::{layout, LayoutNode, LayoutParams};

let positions = layout(
    nodes.iter().map(|(id, size, inputs, outputs)| {
        let node = LayoutNode::new(*size)
            .socket_padding(egui_graph::socket_padding(&style))
            .inputs(*inputs)
            .outputs(*outputs);
        (*id, node)
    }),
    // Edge endpoints are `(node, socket index)`, as in `Edge::new`.
    edges.iter().map(|(a, out_ix, b, in_ix)| ((*a, *out_ix), (*b, *in_ix))),
    LayoutParams::new(Direction::LeftToRight),
);
view.layout = positions;
```

For graphs without socket information, `layout_from_sizes` accepts plain
`(NodeId, size)` nodes and `(NodeId, NodeId)` edges.

To keep long edges from passing over unrelated nodes, use `layout_routed`,
which additionally returns corridor waypoints for the edges that need them,
and thread each edge through its route when drawing:

```rust
let (positions, routes) = layout_routed(nodes, edges, params);
view.layout = positions;
// ... when drawing each edge:
let waypoints = routes.route((src, out_ix), (dst, in_ix), 0).unwrap_or(&[]);
Edge::new((src, out_ix), (dst, in_ix), &mut selected)
    .waypoints(waypoints)
    .show(ctx, ui);
```

Nodes may flow in different directions within one graph. Give a node its own
flow with `LayoutNode::flow`; nodes joined only to others of the same flow are
laid out together in that flow, while edges crossing between flows split the
graph into clusters that are arranged along the outer direction
(`LayoutParams::flow`):

```rust
let node = LayoutNode::new(size).inputs(1).outputs(1).flow(Direction::TopDown);
```

## Controls

### Mouse Controls
- **Left Click**: Select node/edge
- **Ctrl + Left Click**: Toggle selection
- **Shift + Left Click**: Clear selection
- **Left Drag on Background**: Rectangle selection
- **Left Drag on Node**: Move selected nodes
- **Middle Mouse Drag**: Pan view
- **Scroll Wheel**: Zoom in/out

### Keyboard Controls
- **Delete/Backspace**: Remove selected nodes/edges

### Socket Interaction
- **Click Output Socket**: Start edge creation
- **Drag to Input Socket**: Preview connection
- **Release on Input**: Create edge
- **ESC**: Cancel edge creation

## Examples

Run the included demo:

```bash
cargo run --release --example demo
```

The demo showcases:
- Multiple node types (labels, buttons, sliders)
- Dynamic node creation and deletion
- Edge creation between nodes
- Automatic layout
- Configuration options

For a focused look at laying out graphs whose nodes flow in different
directions, run the mixed-flow example:

```bash
cargo run --release --example mixed_flow
```

## Architecture

The library follows egui's immediate-mode paradigm while maintaining necessary
state for graph interactions. Internal state includes:

- Node sizes
- Selection state for nodes and edges
- Active edge creation
- Socket positions for edge rendering

State is stored in egui's data store and accessed through the widget APIs.

use super::*;
use crate::dataflow::add_menu::{AddMenuMode, AddMenuState};
use delog_core::align::AlignMode;
use delog_core::ingest::ingest_channel;
use delog_flow::command::GraphCommand;
use delog_flow::graph::{FieldSelector, Node, NodeId, NodeKind, OutputFieldSpec, OutputSpec};
#[cfg(feature = "scripting")]
use delog_flow::script::{ScriptInputSpec, ScriptOutputSpec};
use std::collections::HashSet;

#[test]
fn opening_and_viewing_a_saved_flow_closes_without_save_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    let mut graph = Graph::new("saved");
    graph.insert_node(Node {
        id: NodeId(1),
        pos: [24.0, 48.0],
        kind: NodeKind::Constant { value: 42.0 },
    });
    flow.store.save(&graph).unwrap();
    flow.edit_named("saved", &mut Vec::new());
    let id = flow.active;
    flow.open = true;
    let ctx = egui::Context::default();
    let snapshot = Arc::new(StoreSnapshot::empty());
    let (sender, _receiver) = ingest_channel();
    for _ in 0..4 {
        render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
        assert!(!flow.active_editor().has_unsaved_changes());
        assert_eq!(flow.active_editor().title(), "saved");
    }
    assert_ne!(flow.active_editor().canvas.viewport, graph.viewport);
    flow.request_close(id);
    assert_eq!(flow.pending_close, None);
    assert!(!flow.editors.contains_key(&id));
    assert_eq!(flow.active_editor().title(), "Untitled");
}

#[test]
fn opening_another_flow_preserves_unsaved_edits_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    flow.store.save(&Graph::new("first")).unwrap();
    flow.store.save(&Graph::new("second")).unwrap();
    let mut logs = Vec::new();
    flow.edit_named("first", &mut logs);
    flow.active_editor_mut()
        .controller
        .apply(GraphCommand::AddNode {
            node: Node {
                id: NodeId(1),
                pos: [24.0, 48.0],
                kind: NodeKind::Constant { value: 42.0 },
            },
        })
        .unwrap();
    flow.active_editor_mut().controller.selection = HashSet::from([NodeId(1)]);
    flow.edit_named("second", &mut logs);
    flow.edit_named("first", &mut logs);

    assert_eq!(flow.active_editor_mut().controller.graph.nodes.len(), 1);
    assert!(flow.active_editor_mut().controller.dirty);
    assert_eq!(
        flow.active_editor_mut().controller.sole_selection(),
        Some(NodeId(1))
    );
    assert!(flow.active_editor_mut().controller.can_undo());
    flow.active_editor_mut().controller.undo();
    assert!(flow.active_editor_mut().controller.graph.nodes.is_empty());
}

#[test]
fn dataflow_window_exposes_three_docked_tab_headers() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut flow = DataFlowUi::new();
    flow.open = true;
    let snapshot = Arc::new(StoreSnapshot::empty());
    let (sender, _receiver) = ingest_channel();
    let mut labels = Vec::new();
    for _ in 0..3 {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| {
                flow.show(ui.ctx(), &snapshot, &sender, false);
            },
        );
        labels = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Tab)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
    }
    for title in ["Dataflows", "Untitled", "Inspector"] {
        assert!(
            labels.iter().any(|label| label == title),
            "missing {title}: {labels:?}"
        );
    }
}

fn node_kinds_that_must_not_expand_the_window() -> Vec<NodeKind> {
    let mut kinds = vec![
        NodeKind::DataField(FieldSelector {
            source: Some("source-with-a-deliberately-long-display-name".to_owned()),
            topic: "topic_with_a_deliberately_long_name".to_owned(),
            instance: Some(0),
            field: "field_with_a_deliberately_long_name".to_owned(),
        }),
        NodeKind::Constant { value: 1.0 },
        NodeKind::Add,
        NodeKind::Subtract,
        NodeKind::Multiply,
        NodeKind::Divide,
        NodeKind::ScaleOffset {
            multiplier: 1.0,
            offset: 0.0,
        },
        NodeKind::Align {
            mode: AlignMode::Prev,
        },
        NodeKind::Output(OutputSpec {
            topic: "output_with_a_deliberately_long_topic_name".to_owned(),
            fields: vec![OutputFieldSpec {
                name: "field_with_a_deliberately_long_name".to_owned(),
                unit: Some("unit_with_a_deliberately_long_name".to_owned()),
            }],
        }),
        NodeKind::Unknown(serde_json::json!({"type": "future_node"})),
    ];
    kinds.extend(
        delog_flow::filter::FilterKind::ALL
            .into_iter()
            .map(|kind| NodeKind::Filter(delog_flow::filter::FilterSpec::new(kind))),
    );
    #[cfg(feature = "scripting")]
        kinds.push(NodeKind::Script(delog_flow::script::ScriptSpec {
            name: "script_with_a_deliberately_long_display_name".to_owned(),
            inputs: vec![ScriptInputSpec {
                name: "input_with_a_deliberately_long_name".to_owned(),
            }],
            outputs: vec![ScriptOutputSpec {
                name: "output_with_a_deliberately_long_name".to_owned(),
                unit: Some("unit_with_a_deliberately_long_name".to_owned()),
            }],
            code: "def flow(inputs):\n    return {\"output_with_a_deliberately_long_name\": inputs.input_with_a_deliberately_long_name.v}\n".to_owned(),
        }));
    kinds
}

fn render_data_flow_frame(
    ctx: &egui::Context,
    flow: &mut DataFlowUi,
    snapshot: &Arc<StoreSnapshot>,
    sender: &IngestSender,
    events: Vec<egui::Event>,
) -> egui::Rect {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_600.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui| {
        let _ = flow.show(ui.ctx(), snapshot, sender, false);
        let _ = flow.drive(
            ui.ctx(),
            snapshot,
            sender,
            false,
            crate::config::settings::DataFlowSettings::default(),
        );
    });
    ctx.memory(|memory| memory.area_rect(egui::Id::new("Data Flow")).unwrap())
}

#[test]
fn active_data_flow_window_is_the_top_layer_for_title_highlight() {
    let ctx = egui::Context::default();
    let snapshot = Arc::new(StoreSnapshot::empty());
    let (sender, _receiver) = ingest_channel();
    let mut flow = DataFlowUi::new();
    flow.open = true;
    let id = flow.active_editor_mut().controller.graph.alloc_id();
    flow.active_editor_mut().controller.graph.insert_node(Node {
        id,
        pos: [0.0, 0.0],
        kind: NodeKind::Add,
    });
    flow.active_editor_mut().controller.selection = HashSet::from([id]);
    for _ in 0..4 {
        let _ = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
    }
    let window_layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("Data Flow"));
    assert!(
        !flow.canvas_layers.is_empty(),
        "canvas should paint sublayers that would otherwise shadow the window"
    );
    let mut observed = None;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_600.0, 900.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui| {
        let _ = flow.reassert_canvas_sublayers(ui.ctx());
        observed = ui.ctx().top_layer_id();
    });
    assert_eq!(
        observed,
        Some(window_layer),
        "re-parenting canvas sublayers must leave the window as the top layer so egui keeps the active title highlight"
    );
}

#[test]
fn adding_any_node_kind_does_not_expand_the_data_flow_window() {
    for kind in node_kinds_that_must_not_expand_the_window() {
        let ctx = egui::Context::default();
        let snapshot = Arc::new(StoreSnapshot::empty());
        let (sender, _receiver) = ingest_channel();
        let mut flow = DataFlowUi::new();
        flow.open = true;

        let _ = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
        let initial = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);

        let id = flow.active_editor_mut().controller.graph.alloc_id();
        flow.active_editor_mut().controller.graph.insert_node(Node {
            id,
            pos: [0.0, 0.0],
            kind,
        });
        flow.active_editor_mut().controller.selection = HashSet::from([id]);

        let node_frame = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
        let following_frame = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);

        assert_eq!(
            node_frame.width(),
            initial.width(),
            "node frame regrew for {:?}",
            flow.active_editor_mut()
                .controller
                .graph
                .node(id)
                .unwrap()
                .kind
        );
        assert_eq!(
            following_frame.width(),
            initial.width(),
            "following frame regrew for {:?}",
            flow.active_editor_mut()
                .controller
                .graph
                .node(id)
                .unwrap()
                .kind
        );
    }
}

#[test]
fn bounded_window_body_keeps_a_window_stable_after_content_grows() {
    let ctx = egui::Context::default();
    let window_id = egui::Id::new("bounded-window-regression");
    let render = |large: bool| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_200.0, 800.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            egui::Window::new("bounded-window-regression")
                .default_size([520.0, 380.0])
                .show(ui.ctx(), |ui| {
                    bounded_window_body(ui, |ui| {
                        if large {
                            ui.allocate_space(egui::vec2(4_000.0, 3_000.0));
                        }
                    });
                });
        });
        ctx.memory(|memory| memory.area_rect(window_id).unwrap())
    };

    let reduced = render(false);
    let after_node = render(true);
    let following_frame = render(true);

    assert_eq!(after_node.size(), reduced.size());
    assert_eq!(following_frame.size(), reduced.size());
}

#[test]
fn choosing_add_data_keeps_the_typed_filter() {
    let ctx = egui::Context::default();
    let snapshot = Arc::new(StoreSnapshot::empty());
    let (sender, _receiver) = ingest_channel();
    let mut flow = DataFlowUi::new();
    flow.open = true;
    let mut menu = AddMenuState::new(egui::pos2(20.0, 30.0), [1.0, 2.0]);
    menu.query = "altitude".to_owned();
    menu.dismiss_armed = true;
    flow.active_editor_mut().add_menu = Some(menu);

    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let _ = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![enter]);

    let menu = flow
        .active_editor_mut()
        .add_menu
        .take()
        .expect("add menu stays open after choosing Add Data");
    assert_eq!(menu.mode, AddMenuMode::Data);
    assert_eq!(menu.query, "altitude");
}

#[test]
fn closing_data_flow_clears_open_add_menu() {
    let mut ui = DataFlowUi::new();
    ui.open = true;
    ui.active_editor_mut().add_menu = Some(AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]));

    ui.update_open(false);

    assert!(!ui.open);
    assert!(ui.active_editor_mut().add_menu.is_none());
}

#[test]
fn opening_an_open_flow_focuses_its_existing_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    flow.store.save(&Graph::new("first")).unwrap();
    flow.store.save(&Graph::new("second")).unwrap();
    let mut logs = Vec::new();
    flow.edit_named("first", &mut logs);
    let first = flow.active;
    flow.edit_named("second", &mut logs);
    flow.edit_named("first", &mut logs);
    assert_eq!(flow.active, first);
    assert_eq!(flow.editors.len(), 2);
    assert!(logs.is_empty());
}

#[test]
fn closing_the_last_editor_replaces_it_with_a_clean_untitled_canvas() {
    let mut flow = DataFlowUi::new();
    let previous = flow.active;
    flow.request_close(previous);
    assert_eq!(flow.editors.len(), 1);
    assert_ne!(flow.active, previous);
    assert_eq!(flow.active_editor().title(), "Untitled");
    assert!(flow.active_editor().controller.graph.nodes.is_empty());
    assert!(flow.active_editor().controller.graph.edges.is_empty());
    assert!(!flow.active_editor().controller.can_undo());
    assert_eq!(flow.dock.main_surface().num_tabs(), 3);
    assert!(flow.dock.find_tab(&DataFlowTab::Dataflows).is_some());
    assert!(flow.dock.find_tab(&DataFlowTab::Inspector).is_some());
}

#[test]
fn closing_an_inactive_tab_keeps_the_active_graph() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    let first = flow.active;
    flow.new_graph();
    let second = flow.active;
    flow.request_close(first);
    assert_eq!(flow.active, second);
    assert_eq!(flow.editors.len(), 1);
    assert_eq!(flow.active_editor().name_edit, "untitled 2");
}

#[test]
fn workspace_tab_close_policy_protects_the_library_and_inspector() {
    let mut flow = DataFlowUi::new();
    let snapshot = Arc::new(StoreSnapshot::empty());
    let mut logs = Vec::new();
    let mut actions = Vec::new();
    let id = flow.active;
    let mut viewer = WorkspaceViewer {
        workspace: &mut flow,
        snapshot: &snapshot,
        live_connected: false,
        logs: &mut logs,
        actions: &mut actions,
    };
    for mut tab in [DataFlowTab::Dataflows, DataFlowTab::Inspector] {
        assert!(!viewer.is_closeable(&tab));
        assert_eq!(viewer.on_close(&mut tab), OnCloseResponse::Ignore);
    }
    assert!(viewer.is_closeable(&DataFlowTab::Editor(id)));
    assert!(actions.is_empty());
}

fn close_dialog_click(flow: &mut DataFlowUi, label: &str) -> Vec<(LogLevel, String)> {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut logs = Vec::new();
    let mut bounds = None;
    for _ in 0..3 {
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            flow.close_confirm(ui.ctx(), &mut logs);
        });
        bounds = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.label() == Some(label)
            })
            .and_then(|(_, node)| node.bounds());
    }
    let bounds = bounds.expect("close dialog button is visible");
    let pos = egui::pos2(
        ((bounds.x0 + bounds.x1) * 0.5) as f32,
        ((bounds.y0 + bounds.y1) * 0.5) as f32,
    );
    for pressed in [true, false] {
        let _ = ctx.run_ui(
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| {
                flow.close_confirm(ui.ctx(), &mut logs);
            },
        );
    }
    logs
}

#[test]
fn cancelling_close_preserves_the_unsaved_graph() {
    let mut flow = DataFlowUi::new();
    let id = flow.active;
    flow.active_editor_mut().name_edit = "draft".to_owned();
    flow.request_close(id);
    assert_eq!(flow.pending_close, Some(id));
    close_dialog_click(&mut flow, "Cancel");
    assert_eq!(flow.pending_close, None);
    assert_eq!(flow.active, id);
    assert_eq!(flow.active_editor().name_edit, "draft");
}

#[test]
fn discarding_the_last_tab_opens_untitled_without_saving() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    flow.active_editor_mut().name_edit = "draft".to_owned();
    flow.request_close(flow.active);
    close_dialog_click(&mut flow, "Discard");
    assert_eq!(flow.pending_close, None);
    assert_eq!(flow.active_editor().title(), "Untitled");
    assert!(flow.store.list().is_empty());
}

#[test]
fn save_on_close_persists_the_document_and_replaces_the_last_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    flow.active_editor_mut().name_edit = "draft".to_owned();
    flow.request_close(flow.active);
    close_dialog_click(&mut flow, "Save");
    assert_eq!(flow.pending_close, None);
    assert_eq!(flow.active_editor().title(), "Untitled");
    assert_eq!(flow.store.load("draft").unwrap().name, "draft");
}

#[test]
fn a_failed_save_on_close_keeps_the_document_and_confirmation_open() {
    let mut flow = DataFlowUi::new();
    let id = flow.active;
    flow.active_editor_mut().name_edit = "invalid/name".to_owned();
    flow.request_close(id);
    let logs = close_dialog_click(&mut flow, "Save");
    assert_eq!(flow.pending_close, Some(id));
    assert_eq!(flow.active, id);
    assert_eq!(logs.len(), 1);
    assert!(logs[0].1.contains("invalid graph name"));
}

#[test]
fn save_on_close_cannot_overwrite_another_open_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    flow.store.save(&Graph::new("first")).unwrap();
    let mut second = Graph::new("second");
    second.insert_node(Node {
        id: NodeId(7),
        pos: [12.0, 24.0],
        kind: NodeKind::Add,
    });
    flow.store.save(&second).unwrap();
    let mut logs = Vec::new();
    flow.edit_named("second", &mut logs);
    flow.edit_named("first", &mut logs);
    let first = flow.active;
    flow.active_editor_mut().name_edit = "second".into();
    flow.request_close(first);
    close_dialog_click(&mut flow, "Save");
    assert_eq!(flow.store.load("second").unwrap().nodes.len(), 1);
    assert_eq!(flow.pending_close, Some(first));
    assert_eq!(flow.editors[&first].loaded_name.as_deref(), Some("first"));
}

#[test]
fn reopening_and_running_a_flow_replaces_its_previous_published_source() {
    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{IdentityRegistry, SourceId};
    use delog_core::ingest::IngestMsg;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "GPS").unwrap();
    identity.add_field(topic, "Alt").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "GPS",
            [FieldSchema::new("Alt", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![100, 200]),
            vec![Arc::new(Float64Array::from(vec![1.0, 2.0])) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot = Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 1).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    let mut graph = Graph::new("publish");
    graph.insert_node(Node {
        id: NodeId(1),
        pos: [0.0, 0.0],
        kind: NodeKind::DataField(FieldSelector {
            source: None,
            topic: "GPS".into(),
            instance: None,
            field: "Alt".into(),
        }),
    });
    graph.insert_node(Node {
        id: NodeId(2),
        pos: [200.0, 0.0],
        kind: NodeKind::Output(OutputSpec {
            topic: "derived".into(),
            fields: vec![OutputFieldSpec {
                name: "alt".into(),
                unit: None,
            }],
        }),
    });
    graph.connect(NodeId(1), 0, NodeId(2), 0).unwrap();
    flow.store.save(&graph).unwrap();
    let mut logs = Vec::new();
    flow.edit_named("publish", &mut logs);
    let (sender, receiver) = ingest_channel();
    let (removed_tx, removed_rx) = mpsc::channel();
    let ingest = std::thread::spawn(move || {
        let mut next = 40;
        while let Some(message) = receiver.recv() {
            match message {
                IngestMsg::OpenSource { reply, .. } => {
                    reply.send(SourceId(next)).unwrap();
                    next += 1;
                }
                IngestMsg::RemoveSource { source } => {
                    removed_tx.send(source).unwrap();
                }
                _ => {}
            }
        }
    });
    for iteration in 0..2 {
        if iteration == 1 {
            flow.request_close(flow.active);
            flow.edit_named("publish", &mut logs);
        }
        let controller = &mut flow.active_editor_mut().controller;
        controller.request_publish(Arc::clone(&snapshot));
        let deadline = Instant::now() + Duration::from_secs(3);
        while controller.is_evaluating() && Instant::now() < deadline {
            logs.extend(controller.poll(&sender));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!controller.is_evaluating(), "publication timed out");
    }
    drop(sender);
    ingest.join().unwrap();
    assert_eq!(
        removed_rx.try_iter().collect::<Vec<_>>(),
        vec![SourceId(40)]
    );
    assert!(!logs.iter().any(|(level, _)| *level == LogLevel::Error));
}

#[test]
fn docked_columns_leave_the_largest_area_for_the_editor() {
    let ctx = egui::Context::default();
    let mut flow = DataFlowUi::new();
    flow.open = true;
    let snapshot = Arc::new(StoreSnapshot::empty());
    let (sender, _receiver) = ingest_channel();
    for _ in 0..3 {
        render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
    }
    let pane = |tab| {
        let path = flow.dock.find_tab(&tab).unwrap();
        flow.dock.main_surface()[path.node].rect().unwrap()
    };
    let library = pane(DataFlowTab::Dataflows);
    let editor = pane(DataFlowTab::Editor(flow.active));
    let inspector = pane(DataFlowTab::Inspector);
    assert!(
        editor.width() > library.width(),
        "{library:?}, {editor:?}, {inspector:?}"
    );
    assert!(editor.width() > inspector.width());
    assert!(library.width() >= 150.0);
    assert!(inspector.width() >= 180.0);
    assert!(library.right() <= editor.left());
    assert!(editor.right() <= inspector.left());
    assert_eq!(library.top(), editor.top());
    assert_eq!(editor.top(), inspector.top());
}

#[test]
fn clicking_editor_tabs_restores_the_canvas_and_inspector_selection() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut flow = DataFlowUi::new();
    flow.open = true;
    let first = flow.active;
    let editor = flow.active_editor_mut();
    editor.name_edit = "first".into();
    editor.controller.graph.name = "first".into();
    editor.controller.graph.insert_node(Node {
        id: NodeId(1),
        pos: [10.0, 20.0],
        kind: NodeKind::DataField(FieldSelector {
            source: None,
            topic: "FIRST_TOPIC".into(),
            instance: None,
            field: "value".into(),
        }),
    });
    editor.controller.selection = HashSet::from([NodeId(1)]);
    editor.canvas.viewport = delog_flow::graph::Viewport {
        offset: [100.0, -25.0],
        zoom: 0.8,
    };
    let snapshot = Arc::new(StoreSnapshot::empty());
    let (sender, _receiver) = ingest_channel();
    let render = |flow: &mut DataFlowUi, events| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                flow.show(ui.ctx(), &snapshot, &sender, false);
            },
        )
    };
    for _ in 0..3 {
        render(&mut flow, vec![]);
    }
    let viewport = flow.active_editor().canvas.viewport;
    let mut second = Graph::new("second");
    second.insert_node(Node {
        id: NodeId(1),
        pos: [250.0, 90.0],
        kind: NodeKind::Add,
    });
    flow.add_editor(second, None);
    for _ in 0..3 {
        render(&mut flow, vec![]);
    }
    let output = render(&mut flow, vec![]);
    let bounds = output
        .platform_output
        .accesskit_update
        .unwrap()
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::Tab
                && node.label().is_some_and(|label| label.starts_with("first"))
        })
        .and_then(|(_, node)| node.bounds())
        .unwrap();
    let pos = egui::pos2(
        (bounds.x0 + 12.0) as f32,
        ((bounds.y0 + bounds.y1) / 2.0) as f32,
    );
    for pressed in [true, false] {
        render(
            &mut flow,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let output = render(&mut flow, vec![]);
    assert_eq!(flow.active, first);
    assert_eq!(
        flow.active_editor().controller.sole_selection(),
        Some(NodeId(1))
    );
    let restored = flow.active_editor().canvas.viewport;
    assert!((restored.zoom - viewport.zoom).abs() < 0.0001);
    for i in 0..2 {
        assert!((restored.offset[i] - viewport.offset[i]).abs() < 0.001);
    }
    assert!(
        output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .any(|(_, node)| node.value() == Some("FIRST_TOPIC"))
    );
}

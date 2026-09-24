use super::*;
use delog_flow::graph::{Node, NodeId, NodeKind};

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
fn renaming_a_saved_flow_moves_its_file_instead_of_leaving_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    let mut graph = Graph::new("first");
    graph.insert_node(Node {
        id: NodeId(7),
        pos: [12.0, 24.0],
        kind: NodeKind::Add,
    });
    flow.store.save(&graph).unwrap();
    let mut logs = Vec::new();
    flow.edit_named("first", &mut logs);
    let id = flow.active;
    flow.editors.get_mut(&id).unwrap().name_edit = "renamed".to_owned();

    assert!(flow.save_editor(id, &mut logs));

    assert_eq!(flow.store.list(), vec!["renamed".to_string()]);
    assert_eq!(flow.store.load("renamed").unwrap().nodes.len(), 1);
    assert_eq!(flow.editors[&id].loaded_name.as_deref(), Some("renamed"));
}

#[test]
fn renaming_onto_an_existing_saved_flow_is_refused() {
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
    flow.edit_named("first", &mut logs);
    let id = flow.active;
    flow.editors.get_mut(&id).unwrap().name_edit = "second".to_owned();
    logs.clear();

    assert!(!flow.save_editor(id, &mut logs));

    assert_eq!(
        flow.store.list(),
        vec!["first".to_string(), "second".into()]
    );
    assert_eq!(flow.store.load("second").unwrap().nodes.len(), 1);
    assert_eq!(flow.editors[&id].loaded_name.as_deref(), Some("first"));
    assert!(logs.iter().any(|(_, text)| text.contains("already exists")));
}

#[test]
fn opening_a_named_flow_reveals_the_window_with_that_graph_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());
    let mut saved = Graph::new("fusion");
    saved.insert_node(Node {
        id: NodeId(3),
        pos: [4.0, 5.0],
        kind: NodeKind::Add,
    });
    flow.store.save(&saved).unwrap();
    assert!(!flow.open);

    let logs = flow.open_named("fusion");

    assert!(
        logs.is_empty(),
        "opening a saved graph should not log: {logs:?}"
    );
    assert!(flow.open, "a failed headless run should reveal the window");
    assert_eq!(
        flow.active_editor().loaded_name.as_deref(),
        Some("fusion"),
        "the named graph should be the focused editor"
    );
    assert_eq!(flow.active_editor().controller.graph.nodes.len(), 1);

    let reopened = flow.open_named("fusion");
    assert!(reopened.is_empty());
    assert_eq!(
        flow.editors.len(),
        1,
        "reopening the same graph should focus its tab, not duplicate it"
    );
}

#[test]
fn opening_a_missing_flow_reports_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut flow = DataFlowUi::new();
    flow.store = GraphStore::new(dir.path().to_path_buf());

    let logs = flow.open_named("absent");

    assert!(flow.open);
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].0, LogLevel::Error);
}

use super::add_menu::{AddAction, AddMenuMode, AddMenuState, show_add_menu};
use super::canvas::{CanvasEvent, CanvasState, show_canvas};
use super::controller::{Clipboard, DataFlowController, PublishedSources};
#[cfg(feature = "scripting")]
use super::inspector::ScriptEditorState;
use super::registry::{ADD_DATA_INDEX, templates};
use super::store::GraphStore;
use crate::ui::logging::LogLevel;
use delog_core::ingest::IngestSender;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::GraphCommand;
use delog_flow::graph::{Graph, Node, NodeId, NodeKind};
use std::collections::HashSet;
use std::sync::Arc;

const CLIPBOARD_SENTINEL: &str = "delog-dataflow-clipboard";

pub(super) enum EditorAction {
    Save,
    Run,
}

pub(super) struct DataFlowEditor {
    pub id: u64,
    pub controller: DataFlowController,
    pub canvas: CanvasState,
    pub add_menu: Option<AddMenuState>,
    pub name_edit: String,
    pub loaded_name: Option<String>,
    last_live_request_s: f64,
    last_live_epoch: u64,
    pending_live_publish: bool,
    clipboard: Clipboard,
    #[cfg(feature = "scripting")]
    pub script_editor: Option<ScriptEditorState>,
}

impl DataFlowEditor {
    pub fn new(
        id: u64,
        graph: Graph,
        loaded_name: Option<String>,
        published: PublishedSources,
    ) -> Self {
        let mut canvas = CanvasState::default();
        canvas.graph_id = Some(egui::Id::new(("dataflow-canvas", id)));
        canvas.reset(&graph);
        if !graph.nodes.is_empty() {
            canvas.request_fit();
        }
        Self {
            id,
            name_edit: graph.name.clone(),
            loaded_name,
            controller: DataFlowController::new(graph).with_shared_publications(published),
            canvas,
            add_menu: None,
            last_live_request_s: 0.0,
            last_live_epoch: 0,
            pending_live_publish: false,
            clipboard: Clipboard::default(),
            #[cfg(feature = "scripting")]
            script_editor: None,
        }
    }

    pub fn title(&self) -> String {
        let name = if self.loaded_name.is_none() && self.name_edit == "untitled" {
            "Untitled"
        } else {
            &self.name_edit
        };
        if self.has_unsaved_changes() {
            format!("{name} •")
        } else {
            name.to_owned()
        }
    }

    pub fn has_unsaved_changes(&self) -> bool {
        #[cfg(feature = "scripting")]
        if self
            .script_editor
            .as_ref()
            .is_some_and(|state| state.buffer != state.baseline)
        {
            return true;
        }
        self.controller.dirty || self.name_edit != self.controller.graph.name
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Arc<StoreSnapshot>,
        live_connected: bool,
        logs: &mut Vec<(LogLevel, String)>,
    ) -> Option<EditorAction> {
        let action = egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt(("dataflow-toolbar", self.id))
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.toolbar(ui, live_connected, logs))
                    .inner
            })
            .inner;
        let issue_nodes = self
            .controller
            .graph
            .nodes
            .iter()
            .map(|node| node.id)
            .filter(|&id| !self.controller.diagnostics_for(id).is_empty())
            .collect();
        let events = show_canvas(
            ui,
            &self.controller.graph,
            &self.controller.selection,
            &issue_nodes,
            &mut self.canvas,
        );
        self.handle_canvas_events(events, snapshot, logs);
        self.controller.graph.viewport = self.canvas.viewport;
        action
    }

    pub fn show_add_menu(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        if let Some(mut menu) = self.add_menu.take() {
            let action = show_add_menu(ctx, &mut menu, snapshot);
            match action {
                Some(AddAction::Template(ADD_DATA_INDEX)) => {
                    menu.mode = AddMenuMode::Data;
                    menu.highlighted = 0;
                    menu.focus_requested = true;
                    self.add_menu = Some(menu);
                }
                Some(AddAction::Template(index)) => {
                    let id = self.controller.graph.alloc_id();
                    let node = Node {
                        id,
                        pos: menu.canvas_pos,
                        kind: (templates()[index].make)(),
                    };
                    self.apply(GraphCommand::AddNode { node }, logs);
                    self.controller.selection = HashSet::from([id]);
                }
                Some(AddAction::Data(hit)) => {
                    let id = self.controller.graph.alloc_id();
                    let node = Node {
                        id,
                        pos: menu.canvas_pos,
                        kind: NodeKind::DataField(hit.selector),
                    };
                    self.apply(GraphCommand::AddNode { node }, logs);
                    self.controller.selection = HashSet::from([id]);
                }
                Some(AddAction::Close) => {}
                None => self.add_menu = Some(menu),
            }
        }
    }

    pub fn handle_shortcuts(
        &mut self,
        ctx: &egui::Context,
        window_active: bool,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        if !window_active || ctx.egui_wants_keyboard_input() {
            return;
        }
        let (copy, paste, delete) = ctx.input(|input| {
            let mut copy = false;
            let mut paste = false;
            let mut delete = false;
            for event in &input.events {
                match event {
                    egui::Event::Copy | egui::Event::Cut => copy = true,
                    egui::Event::Paste(_) => paste = true,
                    egui::Event::Key {
                        key: egui::Key::Delete | egui::Key::Backspace,
                        pressed: true,
                        ..
                    } => delete = true,
                    _ => {}
                }
            }
            (copy, paste, delete)
        });
        if copy {
            self.clipboard = self.controller.copy_selection();
            if !self.clipboard.is_empty() {
                ctx.copy_text(CLIPBOARD_SENTINEL.to_owned());
            }
        }
        if paste
            && !self.clipboard.is_empty()
            && let Err(error) = self.controller.paste(&self.clipboard, [30.0, 30.0])
        {
            logs.push((LogLevel::Error, format!("Paste failed: {error}")));
        }
        if delete
            && !self.controller.selection.is_empty()
            && let Err(error) = self.controller.delete_selection()
        {
            logs.push((LogLevel::Error, format!("Delete failed: {error}")));
        }
    }
    pub fn drive(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        sender: &IngestSender,
        live_connected: bool,
        settings: crate::config::settings::DataFlowSettings,
    ) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        if !live_connected && self.controller.is_live_published() {
            self.controller.reset_live(sender);
        }
        if !live_connected {
            self.pending_live_publish = false;
        }
        if self.controller.take_needs_live_reset() {
            self.controller.reset_live(sender);
        }

        if live_connected {
            let now_s = ctx.input(|i| i.time);
            let epoch = snapshot.epoch;
            if should_tick_live(
                now_s,
                self.last_live_request_s,
                settings.live_throttle_ms,
                epoch,
                self.last_live_epoch,
            ) {
                if self.pending_live_publish && !self.controller.is_live_published() {
                    self.controller.reset_live(sender);
                }
                let append = self.controller.is_live_published() || self.pending_live_publish;
                self.pending_live_publish = false;
                self.controller.request_live(
                    Arc::clone(snapshot),
                    settings.live_overlap_secs,
                    append,
                );
                self.last_live_request_s = now_s;
                self.last_live_epoch = epoch;
            }
            ctx.request_repaint();
        } else if self.controller.needs_eval() {
            self.controller.request_eval(Arc::clone(snapshot));
        }

        logs.extend(self.controller.poll(sender));
        if self.controller.is_evaluating() {
            ctx.request_repaint();
        }
        logs
    }
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        live_connected: bool,
        logs: &mut Vec<(LogLevel, String)>,
    ) -> Option<EditorAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            let name = ui.add(
                egui::TextEdit::singleline(&mut self.name_edit)
                    .desired_width(120.0)
                    .hint_text("graph name"),
            );
            if name.lost_focus() && self.name_edit != self.controller.graph.name {
                self.controller.graph.name.clone_from(&self.name_edit);
                self.controller.dirty = true;
            }
            if icon_btn_enabled(
                ui,
                !self.name_edit.is_empty(),
                crate::ui::icons::save(),
                "Save",
            )
            .clicked()
            {
                action = Some(EditorAction::Save);
            }
            ui.add_space(8.0);
            if icon_btn_enabled(
                ui,
                self.controller.can_undo(),
                crate::ui::icons::rotate_ccw(),
                "Undo",
            )
            .clicked()
            {
                self.controller.undo();
            }
            if icon_btn_enabled(
                ui,
                self.controller.can_redo(),
                crate::ui::icons::rotate_cw(),
                "Redo",
            )
            .clicked()
            {
                self.controller.redo();
            }
            ui.add_space(8.0);
            let has_selection = !self.controller.selection.is_empty();
            if icon_btn_enabled(
                ui,
                has_selection,
                crate::ui::icons::copy(),
                "Duplicate selected",
            )
            .clicked()
            {
                let clipboard = self.controller.copy_selection();
                if let Err(error) = self.controller.paste(&clipboard, [30.0, 30.0]) {
                    logs.push((LogLevel::Error, format!("Duplicate failed: {error}")));
                }
            }
            if icon_btn_enabled(
                ui,
                has_selection,
                crate::ui::icons::trash(),
                "Delete selected",
            )
            .clicked()
                && let Err(error) = self.controller.delete_selection()
            {
                logs.push((LogLevel::Error, format!("Delete failed: {error}")));
            }
            ui.add_space(8.0);
            if !live_connected && self.controller.is_evaluating() {
                ui.add(egui::Spinner::new().size(16.0))
                    .on_hover_text("Running");
            } else {
                let tooltip = if live_connected {
                    "Run (publish live output)"
                } else {
                    "Run"
                };
                if icon_btn_enabled(ui, true, crate::ui::icons::play(), tooltip).clicked() {
                    action = Some(EditorAction::Run);
                }
            }
        });
        action
    }

    pub fn run(&mut self, snapshot: &Arc<StoreSnapshot>, live_connected: bool) {
        self.controller.graph.name.clone_from(&self.name_edit);
        if live_connected {
            self.pending_live_publish = true;
        } else {
            self.controller.request_publish(Arc::clone(snapshot));
        }
    }

    pub fn save(&mut self, store: &GraphStore, logs: &mut Vec<(LogLevel, String)>) -> bool {
        #[cfg(feature = "scripting")]
        if let Some(state) = &self.script_editor
            && state.buffer != state.baseline
            && let Some(node) = self.controller.graph.node(state.node)
            && let NodeKind::Script(mut spec) = node.kind.clone()
        {
            spec.code.clone_from(&state.buffer);
            if let Err(error) = self.controller.apply(GraphCommand::SetKind {
                id: state.node,
                kind: NodeKind::Script(spec),
            }) {
                logs.push((LogLevel::Error, error));
                return false;
            }
            if let Some(state) = &mut self.script_editor {
                state.baseline.clone_from(&state.buffer);
            }
        }
        self.controller.graph.name.clone_from(&self.name_edit);
        match store.save(&self.controller.graph) {
            Ok(()) => {
                self.controller.dirty = false;
                self.loaded_name = Some(self.controller.graph.name.clone());
                logs.push((
                    LogLevel::Info,
                    format!("Saved data flow '{}'", self.controller.graph.name),
                ));
                true
            }
            Err(error) => {
                logs.push((LogLevel::Error, error));
                false
            }
        }
    }
    fn handle_canvas_events(
        &mut self,
        events: Vec<CanvasEvent>,
        snapshot: &Arc<StoreSnapshot>,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        for event in events {
            match event {
                CanvasEvent::Select(selection) => {
                    self.controller.selection = selection;
                    self.controller.request_eval(Arc::clone(snapshot));
                }
                CanvasEvent::Moved { moves } => {
                    if !moves.is_empty() {
                        let commands = moves
                            .into_iter()
                            .map(|(id, to)| GraphCommand::MoveNode { id, to })
                            .collect();
                        self.apply(GraphCommand::Batch(commands), logs);
                    }
                }
                CanvasEvent::Connect {
                    from,
                    from_port,
                    to,
                    to_port,
                } => match self
                    .controller
                    .graph
                    .check_connect(from, from_port, to, to_port)
                {
                    Ok(()) => self.apply(
                        GraphCommand::Connect {
                            from,
                            from_port,
                            to,
                            to_port,
                        },
                        logs,
                    ),
                    Err(error) => {
                        logs.push((LogLevel::Error, format!("Cannot connect nodes: {error:?}")))
                    }
                },
                CanvasEvent::Disconnect { to, to_port } => {
                    if self.controller.graph.incoming(to, to_port).is_some() {
                        self.apply(GraphCommand::Disconnect { to, to_port }, logs);
                    }
                }
                CanvasEvent::DisconnectMany { endpoints } => {
                    if !endpoints.is_empty() {
                        self.apply(disconnect_many_command(endpoints), logs);
                    }
                }
                CanvasEvent::Delete(id) => {
                    self.apply(GraphCommand::RemoveNode { id }, logs);
                }
                CanvasEvent::OpenAddMenu {
                    canvas_pos,
                    screen_pos,
                } => {
                    self.add_menu = Some(AddMenuState::new(screen_pos, canvas_pos));
                }
                CanvasEvent::EditKind { id, kind } => {
                    self.apply(GraphCommand::SetKind { id, kind }, logs);
                }
            }
        }
    }
    pub(super) fn apply(&mut self, command: GraphCommand, logs: &mut Vec<(LogLevel, String)>) {
        if let Err(error) = self.controller.apply(command) {
            logs.push((LogLevel::Error, format!("Data-flow edit failed: {error}")));
        }
    }
}

fn should_tick_live(
    now_s: f64,
    last_s: f64,
    throttle_ms: u32,
    epoch: u64,
    last_epoch: u64,
) -> bool {
    epoch != last_epoch && (now_s - last_s) * 1000.0 >= throttle_ms as f64
}

fn disconnect_many_command(endpoints: Vec<(NodeId, u32)>) -> GraphCommand {
    GraphCommand::Batch(
        endpoints
            .into_iter()
            .rev()
            .map(|(to, to_port)| GraphCommand::Disconnect { to, to_port })
            .collect(),
    )
}

fn icon_btn_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: egui::ImageSource<'static>,
    hover: &str,
) -> egui::Response {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color());
    ui.add_enabled(enabled, egui::Button::image(image))
        .on_hover_text(hover)
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_flow::graph::FieldSelector;

    #[test]
    fn live_cadence_fires_only_on_new_epoch_after_interval() {
        assert!(super::should_tick_live(1.000, 0.700, 200, 5, 4));
        assert!(!super::should_tick_live(0.800, 0.700, 200, 5, 4));
        assert!(!super::should_tick_live(2.000, 0.700, 200, 4, 4));
    }

    #[test]
    fn socket_fan_out_disconnect_is_one_undo_step() {
        let mut graph = Graph::new("fan-out");
        graph.insert_node(Node {
            id: NodeId(1),
            pos: [0.0, 0.0],
            kind: NodeKind::DataField(FieldSelector {
                source: None,
                topic: "signal".to_owned(),
                instance: None,
                field: "value".to_owned(),
            }),
        });
        for id in [NodeId(2), NodeId(3)] {
            graph.insert_node(Node {
                id,
                pos: [200.0, 0.0],
                kind: NodeKind::ScaleOffset {
                    multiplier: 1.0,
                    offset: 0.0,
                },
            });
            graph.connect(NodeId(1), 0, id, 0).unwrap();
        }
        let original_edges = graph.edges.clone();
        let mut controller = DataFlowController::new(graph);

        controller
            .apply(disconnect_many_command(vec![
                (NodeId(2), 0),
                (NodeId(3), 0),
            ]))
            .unwrap();
        assert!(controller.graph.edges.is_empty());
        controller.undo();
        assert_eq!(controller.graph.edges, original_edges);
        assert!(!controller.can_undo());
    }
}

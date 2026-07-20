use std::collections::HashSet;
use std::sync::Arc;

use delog_core::align::AlignMode;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSender, IngestSink, ParseSummary};
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::GraphCommand;
use delog_flow::graph::{FieldSelector, Graph, Node, NodeId, NodeKind, OutputFieldSpec};

use super::canvas::{CanvasEvent, CanvasState, show_canvas};
use super::controller::{Clipboard, DataFlowController};
use super::picker::{DataHit, search_fields};
use super::registry::{ADD_DATA_INDEX, search_templates, templates};
use super::store::GraphStore;
use crate::logging::LogLevel;

#[cfg(feature = "scripting")]
use delog_flow::script::{ScriptInputSpec, ScriptOutputSpec};
#[cfg(feature = "scripting")]
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddMenuMode {
    Templates,
    Data,
}

struct AddMenuState {
    screen_pos: egui::Pos2,
    canvas_pos: [f32; 2],
    query: String,
    mode: AddMenuMode,
    highlighted: usize,
    focus_requested: bool,
    dismiss_armed: bool,
}

impl AddMenuState {
    fn new(screen_pos: egui::Pos2, canvas_pos: [f32; 2]) -> Self {
        Self {
            screen_pos,
            canvas_pos,
            query: String::new(),
            mode: AddMenuMode::Templates,
            highlighted: 0,
            focus_requested: true,
            dismiss_armed: false,
        }
    }
}

enum AddAction {
    Template(usize),
    Data(DataHit),
    Close,
}

#[cfg(feature = "scripting")]
struct ScriptEditorState {
    node: NodeId,
    baseline: String,
    buffer: String,
}

pub struct DataFlowUi {
    pub open: bool,
    controller: DataFlowController,
    store: GraphStore,
    canvas: CanvasState,
    add_menu: Option<AddMenuState>,
    name_edit: String,
    loaded_name: Option<String>,
    pending_delete: Option<String>,
    library_collapsed: bool,
    canvas_layers: Vec<egui::LayerId>,
    last_live_request_s: f64,
    last_live_epoch: u64,
    pending_live_publish: bool,
    orphaned_live_sources: Vec<SourceId>,
    clipboard: Clipboard,
    #[cfg(feature = "scripting")]
    script_editor: Option<ScriptEditorState>,
}

fn bounded_window_body<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("bounded-data-flow-window-body")
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect);
    child.set_min_size(rect.size());
    child.set_max_size(rect.size());
    content(&mut child)
}

impl DataFlowUi {
    pub fn new() -> Self {
        let graph = Graph::new("untitled");
        Self {
            open: false,
            controller: DataFlowController::new(graph),
            store: GraphStore::new(
                GraphStore::default_dir()
                    .unwrap_or_else(|| std::env::temp_dir().join("delog-dataflows")),
            ),
            canvas: CanvasState::default(),
            add_menu: None,
            name_edit: "untitled".to_owned(),
            loaded_name: None,
            pending_delete: None,
            library_collapsed: true,
            canvas_layers: Vec::new(),
            last_live_request_s: 0.0,
            last_live_epoch: 0,
            pending_live_publish: false,
            orphaned_live_sources: Vec::new(),
            clipboard: Clipboard::default(),
            #[cfg(feature = "scripting")]
            script_editor: None,
        }
    }

    /// Whether the graph currently contains a script node — the app layer
    /// uses this to decide whether the Python engine needs to be running.
    #[cfg(feature = "scripting")]
    pub fn has_script_node(&self) -> bool {
        self.controller
            .graph
            .nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::Script(_)))
    }

    #[cfg(feature = "scripting")]
    pub fn set_script_host(&mut self, host: Option<delog_script::flow::EngineFlowHost>) {
        self.controller.set_script_host(host);
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        _sender: &IngestSender,
        live_connected: bool,
    ) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        let mut open = self.open;
        let window_layer = self.reassert_canvas_sublayers(ctx);
        let window_response = egui::Window::new("Data Flow")
            .open(&mut open)
            .default_size([980.0, 640.0])
            .min_size([720.0, 420.0])
            .show(ctx, |ui| {
                bounded_window_body(ui, |ui| {
                    if self.library_collapsed {
                        self.collapsed_library_drawer(ui);
                    } else {
                        egui::Panel::left("dataflow_library_drawer")
                            .resizable(true)
                            .default_size(180.0)
                            .size_range(140.0..=260.0)
                            .show_inside(ui, |ui| self.library_drawer(ui, &mut logs));
                    }
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        self.toolbar(ui, snapshot, live_connected, &mut logs);
                        ui.separator();

                        let issue_nodes: std::collections::HashSet<NodeId> = self
                            .controller
                            .graph
                            .nodes
                            .iter()
                            .map(|node| node.id)
                            .filter(|&id| !self.controller.diagnostics_for(id).is_empty())
                            .collect();
                        let height = ui.available_height();
                        ui.horizontal(|ui| {
                            let canvas_width = (ui.available_width() - 270.0).max(200.0);
                            let events = ui
                                .allocate_ui_with_layout(
                                    egui::vec2(canvas_width, height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        show_canvas(
                                            ui,
                                            &self.controller.graph,
                                            &self.controller.selection,
                                            &issue_nodes,
                                            &mut self.canvas,
                                        )
                                    },
                                )
                                .inner;
                            ui.separator();
                            ui.allocate_ui_with_layout(
                                egui::vec2(260.0, height),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| self.inspector(ui, snapshot, &mut logs),
                            );
                            self.handle_canvas_events(events, snapshot, &mut logs);
                        });
                    });
                });
            });
        self.canvas_layers = descendant_layers(ctx, window_layer);
        if self.controller.graph.viewport != self.canvas.viewport {
            self.controller.graph.viewport = self.canvas.viewport;
            self.controller.dirty = true;
        }
        self.update_open(open);
        self.delete_confirm(ctx, &mut logs);

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
                    self.apply(GraphCommand::AddNode { node }, &mut logs);
                    self.controller.selection = HashSet::from([id]);
                }
                Some(AddAction::Data(hit)) => {
                    let id = self.controller.graph.alloc_id();
                    let node = Node {
                        id,
                        pos: menu.canvas_pos,
                        kind: NodeKind::DataField(hit.selector),
                    };
                    self.apply(GraphCommand::AddNode { node }, &mut logs);
                    self.controller.selection = HashSet::from([id]);
                }
                Some(AddAction::Close) => {}
                None => self.add_menu = Some(menu),
            }
        }

        let window_active = window_response
            .as_ref()
            .is_some_and(|response| response.response.contains_pointer());
        self.handle_shortcuts(ctx, window_active, &mut logs);

        logs
    }

    /// Delete / copy / paste keyboard shortcuts, scoped to the Data Flow window
    /// and suppressed while a text field or code editor has keyboard focus.
    fn handle_shortcuts(
        &mut self,
        ctx: &egui::Context,
        window_active: bool,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        if !window_active || ctx.egui_wants_keyboard_input() {
            return;
        }
        let (delete, copy, paste) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace),
                input.modifiers.command && input.key_pressed(egui::Key::C),
                input.modifiers.command && input.key_pressed(egui::Key::V),
            )
        });
        if copy {
            self.clipboard = self.controller.copy_selection();
        }
        if paste && !self.clipboard.is_empty() {
            if let Err(error) = self.controller.paste(&self.clipboard, [30.0, 30.0]) {
                logs.push((LogLevel::Error, format!("Paste failed: {error}")));
            }
        }
        if delete && !self.controller.selection.is_empty() {
            if let Err(error) = self.controller.delete_selection() {
                logs.push((LogLevel::Error, format!("Delete failed: {error}")));
            }
        }
    }

    /// Runs every frame regardless of window visibility: handles live reset,
    /// the throttled live cadence, static preview evaluation, and `poll`.
    pub fn drive(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        sender: &IngestSender,
        live_connected: bool,
        settings: crate::settings::DataFlowSettings,
    ) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();

        // Close any live sources orphaned by a graph reload/new (see replace_graph),
        // which drops the controller before it can close them itself.
        for source in std::mem::take(&mut self.orphaned_live_sources) {
            let mut sink = sender.file_sink();
            sink.close_source(source, ParseSummary::default());
        }

        // Live disconnected -> freeze the current output (data stays), stop live.
        if !live_connected && self.controller.is_live_published() {
            self.controller.reset_live(sender);
        }
        // A Run intent must not survive a disconnect.
        if !live_connected {
            self.pending_live_publish = false;
        }
        // Graph edited while live -> re-seed on the next tick.
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
                // A fresh live publication (Run pressed, nothing published yet) must seed
                // from the full history, so clear the preview-advanced watermark before
                // seeding.
                if self.pending_live_publish && !self.controller.is_live_published() {
                    self.controller.reset_live(sender);
                }
                let append = self.controller.is_live_published() || self.pending_live_publish;
                self.pending_live_publish = false;
                self.controller
                    .request_live(Arc::clone(snapshot), settings.live_overlap_secs, append);
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
        snapshot: &Arc<StoreSnapshot>,
        live_connected: bool,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        ui.horizontal(|ui| {
            let name = ui.add(
                egui::TextEdit::singleline(&mut self.name_edit)
                    .desired_width(160.0)
                    .hint_text("graph name"),
            );
            if name.lost_focus() && self.name_edit != self.controller.graph.name {
                self.controller.graph.name.clone_from(&self.name_edit);
                self.controller.dirty = true;
            }
            if icon_btn_enabled(ui, !self.name_edit.is_empty(), crate::icons::save(), "Save")
                .clicked()
            {
                self.save(logs);
            }
            ui.separator();
            if icon_btn_enabled(ui, self.controller.can_undo(), crate::icons::rotate_ccw(), "Undo")
                .clicked()
            {
                self.controller.undo();
            }
            if icon_btn_enabled(ui, self.controller.can_redo(), crate::icons::rotate_cw(), "Redo")
                .clicked()
            {
                self.controller.redo();
            }
            ui.separator();
            if !live_connected && self.controller.is_evaluating() {
                ui.add(egui::Spinner::new().size(16.0))
                    .on_hover_text("Running");
            } else {
                let tooltip = if live_connected { "Run (publish live output)" } else { "Run" };
                if icon_btn_enabled(ui, true, crate::icons::play(), tooltip).clicked() {
                    if live_connected {
                        self.pending_live_publish = true;
                    } else {
                        self.controller.request_publish(Arc::clone(snapshot));
                    }
                }
            }
        });
    }

    // egui only highlights the title bar of the layer that is `ctx.top_layer_id()`.
    // egui_graph paints the canvas in sublayers that egui re-orders above the
    // window each frame, so the window is never seen as topmost. Re-parent the
    // previous frame's canvas layers under the window so egui excludes them from
    // the "top layer" search and keeps the active-window highlight.
    fn reassert_canvas_sublayers(&self, ctx: &egui::Context) -> egui::LayerId {
        let window_layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("Data Flow"));
        for child in &self.canvas_layers {
            ctx.set_sublayer(window_layer, *child);
        }
        window_layer
    }

    fn collapsed_library_drawer(&mut self, ui: &mut egui::Ui) {
        let button_size = crate::browser::panel_toggle_button_size(ui);
        let collapsed_left_margin = ui.spacing().item_spacing.x;
        let collapsed_right_margin = ui.spacing().item_spacing.x;
        let collapsed_width = collapsed_left_margin + button_size.x + collapsed_right_margin;
        let collapsed_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::ZERO);
        egui::Panel::left("dataflow_library_collapsed")
            .resizable(false)
            .show_separator_line(true)
            .frame(collapsed_frame)
            .exact_size(collapsed_width)
            .show_inside(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(collapsed_left_margin);
                    let icon_size = button_size - ui.spacing().button_padding * 2.0;
                    let icon = egui::Image::new(crate::icons::panel_left_open())
                        .fit_to_exact_size(icon_size)
                        .tint(ui.visuals().text_color());
                    if ui
                        .add_sized(button_size, egui::Button::image(icon))
                        .on_hover_text("Show data flows")
                        .clicked()
                    {
                        self.library_collapsed = false;
                    }
                });
            });
    }

    fn library_drawer(&mut self, ui: &mut egui::Ui, logs: &mut Vec<(LogLevel, String)>) {
        ui.horizontal(|ui| {
            ui.strong("Data Flows");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button_size = crate::browser::panel_toggle_button_size(ui);
                let icon_size = button_size - ui.spacing().button_padding * 2.0;
                let icon = egui::Image::new(crate::icons::panel_left_close())
                    .fit_to_exact_size(icon_size)
                    .tint(ui.visuals().text_color());
                if ui
                    .add_sized(button_size, egui::Button::image(icon))
                    .on_hover_text("Hide data flows")
                    .clicked()
                {
                    self.library_collapsed = true;
                }
                if ui.button("+ New").clicked() {
                    self.new_graph(logs);
                }
            });
        });
        ui.separator();
        let names = self.store.list();
        if names.is_empty() {
            ui.weak("No saved data flows.");
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for name in names {
                    ui.horizontal(|ui| {
                        let selected = self.loaded_name.as_deref() == Some(name.as_str());
                        if ui
                            .selectable_label(selected, name.as_str())
                            .on_hover_text("Load data flow")
                            .clicked()
                        {
                            self.edit_named(&name, logs);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.menu_button("...", |ui| {
                                if ui.button("Duplicate").clicked() {
                                    self.duplicate(&name, logs);
                                    ui.close();
                                }
                                if ui.button("Remove").clicked() {
                                    self.pending_delete = Some(name.clone());
                                    ui.close();
                                }
                            });
                        });
                    });
                }
            });
    }

    fn save(&mut self, logs: &mut Vec<(LogLevel, String)>) {
        self.controller.graph.name.clone_from(&self.name_edit);
        match self.store.save(&self.controller.graph) {
            Ok(()) => {
                self.controller.dirty = false;
                self.loaded_name = Some(self.controller.graph.name.clone());
                logs.push((
                    LogLevel::Info,
                    format!("Saved data flow '{}'", self.controller.graph.name),
                ));
            }
            Err(error) => logs.push((LogLevel::Error, error)),
        }
    }

    fn new_graph(&mut self, logs: &mut Vec<(LogLevel, String)>) {
        if self.controller.dirty {
            logs.push((
                LogLevel::Warning,
                "Discarded unsaved data-flow changes".to_owned(),
            ));
        }
        self.name_edit = "untitled".to_owned();
        self.replace_graph(Graph::new(&self.name_edit));
        self.loaded_name = None;
        self.add_menu = None;
    }

    fn edit_named(&mut self, name: &str, logs: &mut Vec<(LogLevel, String)>) {
        if self.controller.dirty {
            logs.push((
                LogLevel::Warning,
                "Discarded unsaved data-flow changes".to_owned(),
            ));
        }
        match self.store.load(name) {
            Ok(graph) => {
                self.name_edit.clone_from(&graph.name);
                self.replace_graph(graph);
                self.loaded_name = Some(name.to_owned());
                self.add_menu = None;
            }
            Err(error) => logs.push((LogLevel::Error, error)),
        }
    }

    fn duplicate(&mut self, name: &str, logs: &mut Vec<(LogLevel, String)>) {
        match self.store.load(name) {
            Ok(mut graph) => {
                let copy = available_copy_name(&self.store.list(), name);
                graph.name.clone_from(&copy);
                match self.store.save(&graph) {
                    Ok(()) => logs.push((
                        LogLevel::Info,
                        format!("Duplicated data flow '{name}' as '{copy}'"),
                    )),
                    Err(error) => logs.push((LogLevel::Error, error)),
                }
            }
            Err(error) => logs.push((LogLevel::Error, error)),
        }
    }

    fn delete_confirm(&mut self, ctx: &egui::Context, logs: &mut Vec<(LogLevel, String)>) {
        let Some(name) = self.pending_delete.clone() else {
            return;
        };
        let mut keep_open = true;
        let mut decision: Option<bool> = None;
        egui::Window::new("Delete data flow?")
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .resizable(false)
            .open(&mut keep_open)
            .show(ctx, |ui| {
                ui.label(format!("Delete \u{201c}{name}\u{201d}? This cannot be undone."));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        if !keep_open {
            decision = decision.or(Some(false));
        }
        match decision {
            Some(true) => {
                match self.store.delete(&name) {
                    Ok(()) => {
                        if self.loaded_name.as_deref() == Some(name.as_str()) {
                            self.loaded_name = None;
                        }
                        logs.push((LogLevel::Info, format!("Deleted data flow '{name}'")));
                    }
                    Err(error) => logs.push((LogLevel::Error, error)),
                }
                self.pending_delete = None;
            }
            Some(false) => self.pending_delete = None,
            None => {}
        }
    }

    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Arc<StoreSnapshot>,
        logs: &mut Vec<(LogLevel, String)>,
    ) {
        ui.heading("Inspector");
        let Some(id) = self.controller.sole_selection() else {
            let count = self.controller.selection.len();
            if count > 1 {
                ui.weak(format!("{count} nodes selected"));
            } else {
                ui.weak("Select a node to inspect it");
            }
            return;
        };
        let Some(node) = self.controller.graph.node(id) else {
            return;
        };
        ui.strong(node.kind.label());
        for diagnostic in self.controller.diagnostics_for(id) {
            ui.colored_label(ui.visuals().error_fg_color, diagnostic);
        }
        ui.separator();

        // Source disambiguation (session-only binding, not an undoable edit):
        // only shown when this field's topic/field is present in >1 source.
        let mut source_choice = None;
        if let NodeKind::DataField(selector) = &node.kind {
            let candidates = delog_flow::resolve::candidate_source_labels(snapshot, selector);
            if candidates.len() > 1 {
                let mut chosen = selector.source.clone();
                egui::ComboBox::from_label("Source")
                    .selected_text(chosen.as_deref().unwrap_or("(choose)"))
                    .show_ui(ui, |ui| {
                        for label in &candidates {
                            ui.selectable_value(&mut chosen, Some(label.clone()), label);
                        }
                    });
                if chosen != selector.source {
                    source_choice = Some(chosen);
                }
            }
        }
        if let Some(choice) = source_choice {
            self.controller.set_field_source(id, choice);
        }

        let Some(node) = self.controller.graph.node(id) else {
            return;
        };
        let mut edited = node.kind.clone();
        let mut structural_edit = None;
        match &mut edited {
            NodeKind::DataField(selector) => show_selector(ui, selector),
            NodeKind::Constant { value } => {
                ui.horizontal(|ui| {
                    ui.label("Value");
                    ui.add(egui::DragValue::new(value));
                });
            }
            NodeKind::ScaleOffset { multiplier, offset } => {
                ui.horizontal(|ui| {
                    ui.label("Multiplier");
                    ui.add(egui::DragValue::new(multiplier));
                });
                ui.horizontal(|ui| {
                    ui.label("Offset");
                    ui.add(egui::DragValue::new(offset));
                });
            }
            NodeKind::Convert { kind } => {
                egui::ComboBox::from_label("Conversion")
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for option in delog_flow::graph::ConversionKind::ALL {
                            ui.selectable_value(kind, option, option.label());
                        }
                    });
            }
            NodeKind::Align { mode } => {
                egui::ComboBox::from_id_salt(("dataflow-align-mode", id.0))
                    .selected_text(mode.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(mode, AlignMode::Prev, "prev");
                        ui.selectable_value(mode, AlignMode::Nearest, "nearest");
                        ui.selectable_value(mode, AlignMode::Linear, "linear");
                    });
            }
            NodeKind::Output(spec) => {
                ui.horizontal(|ui| {
                    ui.label("Topic");
                    ui.text_edit_singleline(&mut spec.topic);
                });
                let mut remove = None;
                for (index, field) in spec.fields.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut field.name);
                        let mut unit = field.unit.clone().unwrap_or_default();
                        if ui
                            .add(egui::TextEdit::singleline(&mut unit).hint_text("unit"))
                            .changed()
                        {
                            field.unit = (!unit.is_empty()).then_some(unit);
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    structural_edit = Some(GraphCommand::RemoveOutputField { id, index });
                }
                if ui.button("Add field").clicked() {
                    structural_edit = Some(GraphCommand::InsertOutputField {
                        id,
                        index: spec.fields.len(),
                        field: OutputFieldSpec {
                            name: format!("field_{}", spec.fields.len() + 1),
                            unit: None,
                        },
                        connection: None,
                    });
                }
            }
            #[cfg(feature = "scripting")]
            NodeKind::Script(spec) => {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut spec.name);
                });

                ui.separator();
                ui.strong("Inputs");
                let mut remove_input = None;
                for (index, input) in spec.inputs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut input.name);
                        if ui.button("Remove").clicked() {
                            remove_input = Some(index);
                        }
                    });
                }
                if let Some(index) = remove_input {
                    structural_edit = Some(GraphCommand::RemoveScriptInput { id, index });
                }
                if ui.button("Add input").clicked() {
                    structural_edit = Some(GraphCommand::InsertScriptInput {
                        id,
                        index: spec.inputs.len(),
                        input: ScriptInputSpec {
                            name: format!("in_{}", spec.inputs.len() + 1),
                        },
                        connection: None,
                    });
                }

                ui.separator();
                ui.strong("Outputs");
                let mut remove_output = None;
                for (index, output) in spec.outputs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut output.name);
                        let mut unit = output.unit.clone().unwrap_or_default();
                        if ui
                            .add(egui::TextEdit::singleline(&mut unit).hint_text("unit"))
                            .changed()
                        {
                            output.unit = (!unit.is_empty()).then_some(unit);
                        }
                        if ui.button("Remove").clicked() {
                            remove_output = Some(index);
                        }
                    });
                }
                if let Some(index) = remove_output {
                    structural_edit = Some(GraphCommand::RemoveScriptOutput { id, index });
                }
                if ui.button("Add output").clicked() {
                    structural_edit = Some(GraphCommand::InsertScriptOutput {
                        id,
                        index: spec.outputs.len(),
                        output: ScriptOutputSpec {
                            name: format!("out_{}", spec.outputs.len() + 1),
                            unit: None,
                        },
                    });
                }

                ui.separator();
                ui.strong("Code");
                let buffer = script_editor_buffer(&mut self.script_editor, id, &spec.code);
                CodeEditor::default()
                    .id_source(format!("dataflow-script-code-{}", id.0))
                    .with_rows(14)
                    .with_theme(ColorTheme::GITHUB_DARK)
                    .show(ui, buffer, &Syntax::python());

                if ui.button("Apply").clicked() {
                    let mut applied = spec.clone();
                    applied.code = self
                        .script_editor
                        .as_ref()
                        .map_or_else(|| spec.code.clone(), |state| state.buffer.clone());
                    structural_edit = Some(GraphCommand::SetKind {
                        id,
                        kind: NodeKind::Script(applied),
                    });
                }

                ui.separator();
                ui.strong("Preview");
                for (index, output) in spec.outputs.iter().enumerate() {
                    if let Some(preview) = self.controller.preview_for(id, index) {
                        ui.label(output.name.as_str());
                        egui::Grid::new(("dataflow-script-preview", id.0, index))
                            .num_columns(2)
                            .show(ui, |ui| {
                                stat_row(ui, "Count", preview.count);
                                stat_row(ui, "NaN", preview.nan_count);
                                stat_row(ui, "Min", preview.min);
                                stat_row(ui, "Max", preview.max);
                                stat_row(ui, "Mean", preview.mean);
                                stat_row(ui, "Stddev", preview.stddev);
                                stat_row(ui, "Start (us)", preview.t0_us);
                                stat_row(ui, "End (us)", preview.t1_us);
                            });
                    }
                }
            }
            NodeKind::Add
            | NodeKind::Subtract
            | NodeKind::Multiply
            | NodeKind::Divide
            | NodeKind::Unknown(_) => {}
        }
        #[cfg(feature = "scripting")]
        let has_own_preview_section = matches!(node.kind, NodeKind::Script(_));
        #[cfg(not(feature = "scripting"))]
        let has_own_preview_section = false;
        if let Some(command) = structural_edit {
            self.apply(command, logs);
        } else if edited != node.kind {
            self.apply(GraphCommand::SetKind { id, kind: edited }, logs);
        }

        if !has_own_preview_section
            && let Some(preview) = self.controller.preview_for(id, 0)
        {
            ui.separator();
            ui.strong("Preview");
            egui::Grid::new(("dataflow-preview", id.0))
                .num_columns(2)
                .show(ui, |ui| {
                    stat_row(ui, "Count", preview.count);
                    stat_row(ui, "NaN", preview.nan_count);
                    stat_row(ui, "Min", preview.min);
                    stat_row(ui, "Max", preview.max);
                    stat_row(ui, "Mean", preview.mean);
                    stat_row(ui, "Stddev", preview.stddev);
                    stat_row(ui, "Start (us)", preview.t0_us);
                    stat_row(ui, "End (us)", preview.t1_us);
                });
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

    fn apply(&mut self, command: GraphCommand, logs: &mut Vec<(LogLevel, String)>) {
        if let Err(error) = self.controller.apply(command) {
            logs.push((LogLevel::Error, format!("Data-flow edit failed: {error}")));
        }
    }

    fn update_open(&mut self, open: bool) {
        self.open = open;
        if !open {
            self.add_menu = None;
        }
    }

    fn replace_graph(&mut self, graph: Graph) {
        self.canvas.reset(&graph);
        if let Some(source) = self.controller.live_source() {
            self.orphaned_live_sources.push(source);
        }
        self.clipboard = Clipboard::default();
        self.controller.replace_graph(graph);
    }
}

fn should_tick_live(now_s: f64, last_s: f64, throttle_ms: u32, epoch: u64, last_epoch: u64) -> bool {
    epoch != last_epoch && (now_s - last_s) * 1000.0 >= throttle_ms as f64
}

fn show_selector(ui: &mut egui::Ui, selector: &FieldSelector) {
    ui.label(format!("Topic: {}", selector.topic));
    if let Some(instance) = selector.instance {
        ui.label(format!("Instance: {instance}"));
    }
    ui.label(format!("Field: {}", selector.field));
}

/// Resets the code-editor buffer to `code` when the selected node changes or
/// when `code` moves out from under the buffer (undo/redo, load, or the
/// Apply button's own commit reconciling on the next frame).
#[cfg(feature = "scripting")]
fn script_editor_buffer<'a>(
    state: &'a mut Option<ScriptEditorState>,
    id: NodeId,
    code: &str,
) -> &'a mut String {
    let needs_reset = match state {
        Some(existing) => existing.node != id || existing.baseline != code,
        None => true,
    };
    if needs_reset {
        *state = Some(ScriptEditorState {
            node: id,
            baseline: code.to_owned(),
            buffer: code.to_owned(),
        });
    }
    &mut state.as_mut().expect("just reset if absent").buffer
}

fn stat_row(ui: &mut egui::Ui, label: &str, value: impl std::fmt::Display) {
    ui.label(label);
    ui.label(value.to_string());
    ui.end_row();
}

fn show_add_menu(
    ctx: &egui::Context,
    state: &mut AddMenuState,
    snapshot: &StoreSnapshot,
) -> Option<AddAction> {
    let was_dismiss_armed = std::mem::replace(&mut state.dismiss_armed, true);
    let template_hits = if state.mode == AddMenuMode::Templates {
        search_templates(&state.query)
    } else {
        Vec::new()
    };
    let data_hits = if state.mode == AddMenuMode::Data {
        search_fields(snapshot, &state.query, 24)
    } else {
        Vec::new()
    };
    let row_count = match state.mode {
        AddMenuMode::Templates => template_hits.len() + 1,
        AddMenuMode::Data => data_hits.len(),
    };
    state.highlighted = state.highlighted.min(row_count.saturating_sub(1));
    match handle_menu_keys(ctx, &mut state.highlighted, row_count) {
        Some(MenuKey::Close) => return Some(AddAction::Close),
        Some(MenuKey::Accept) => {
            return accept_add_action(state, &template_hits, &data_hits);
        }
        None => {}
    }
    {
        let mut action = None;
        let area = egui::Area::new(egui::Id::new("dataflow-add-menu"))
            .fixed_pos(state.screen_pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(320.0);
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut state.query)
                            .desired_width(f32::INFINITY)
                            .hint_text(match state.mode {
                                AddMenuMode::Templates => "Search nodes",
                                AddMenuMode::Data => "Search source, topic, field, or unit",
                            }),
                    );
                    if state.focus_requested {
                        search.request_focus();
                        state.focus_requested = false;
                    }
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .min_scrolled_height(360.0)
                        .max_height(360.0)
                        .show(ui, |ui| match state.mode {
                            AddMenuMode::Templates => {
                                if menu_row(ui, state.highlighted == 0, "Add Data...") {
                                    action = Some(AddAction::Template(ADD_DATA_INDEX));
                                }
                                let mut category = "";
                                for (row, hit) in template_hits.iter().enumerate() {
                                    let template = &templates()[hit.index];
                                    if state.query.trim().is_empty()
                                        && template.category != category
                                    {
                                        category = template.category;
                                        ui.weak(category);
                                    }
                                    if menu_row(ui, state.highlighted == row + 1, template.name) {
                                        action = Some(AddAction::Template(hit.index));
                                    }
                                }
                            }
                            AddMenuMode::Data => {
                                if data_hits.is_empty() {
                                    ui.weak("No matching numeric fields");
                                }
                                for (row, hit) in data_hits.iter().enumerate() {
                                    let label = match &hit.unit {
                                        Some(unit) => {
                                            format!("{}  {}  {} rows", hit.label, unit, hit.rows)
                                        }
                                        None => format!("{}  {} rows", hit.label, hit.rows),
                                    };
                                    if menu_row(ui, state.highlighted == row, &label) {
                                        action = Some(AddAction::Data(hit.clone()));
                                    }
                                }
                            }
                        });
                });
            });
        let clicked_outside = ctx.input(|input| {
            input.pointer.any_click()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|position| !area.response.rect.contains(position))
        });
        if action.is_none() && should_close_add_menu(was_dismiss_armed, clicked_outside) {
            Some(AddAction::Close)
        } else {
            action
        }
    }
}

fn should_close_add_menu(was_dismiss_armed: bool, clicked_outside: bool) -> bool {
    was_dismiss_armed && clicked_outside
}

fn menu_row(ui: &mut egui::Ui, highlighted: bool, label: &str) -> bool {
    ui.add_sized(
        [ui.available_width(), 24.0],
        egui::Button::new(label).selected(highlighted),
    )
    .clicked()
}

enum MenuKey {
    Accept,
    Close,
}

fn handle_menu_keys(ctx: &egui::Context, highlighted: &mut usize, len: usize) -> Option<MenuKey> {
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        return Some(MenuKey::Close);
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
        *highlighted = move_highlight(*highlighted, len, 1);
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
        *highlighted = move_highlight(*highlighted, len, -1);
    }
    if len > 0 && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        return Some(MenuKey::Accept);
    }
    None
}

fn accept_add_action(
    state: &AddMenuState,
    template_hits: &[super::registry::MenuEntry],
    data_hits: &[DataHit],
) -> Option<AddAction> {
    match state.mode {
        AddMenuMode::Templates if state.highlighted == 0 => {
            Some(AddAction::Template(ADD_DATA_INDEX))
        }
        AddMenuMode::Templates => template_hits
            .get(state.highlighted - 1)
            .map(|hit| AddAction::Template(hit.index)),
        AddMenuMode::Data => data_hits
            .get(state.highlighted)
            .cloned()
            .map(AddAction::Data),
    }
}

fn move_highlight(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (current as isize + delta).rem_euclid(len as isize) as usize
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

fn descendant_layers(ctx: &egui::Context, root: egui::LayerId) -> Vec<egui::LayerId> {
    ctx.memory(|memory| {
        let areas = memory.areas();
        let mut out: Vec<egui::LayerId> = Vec::new();
        let mut stack: Vec<egui::LayerId> = areas.child_layers(root).collect();
        while let Some(layer) = stack.pop() {
            if out.contains(&layer) {
                continue;
            }
            out.push(layer);
            stack.extend(areas.child_layers(layer));
        }
        out
    })
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

fn available_copy_name(existing: &[String], name: &str) -> String {
    let base = format!("{name}_copy");
    if !existing.iter().any(|candidate| candidate == &base) {
        return base;
    }
    for i in 2.. {
        let candidate = format!("{base}_{i}");
        if !existing.iter().any(|existing| existing == &candidate) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use crate::dataflow::canvas_state::ui_node_id;
    use delog_core::ingest::ingest_channel;
    use delog_flow::graph::{NodeId, OutputSpec, Viewport};

    use super::*;

    fn node_kinds_that_must_not_expand_the_window() -> Vec<NodeKind> {
        #[allow(unused_mut)]
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
                crate::settings::DataFlowSettings::default(),
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
        let id = flow.controller.graph.alloc_id();
        flow.controller.graph.insert_node(Node {
            id,
            pos: [0.0, 0.0],
            kind: NodeKind::Add,
        });
        flow.controller.selection = HashSet::from([id]);
        for _ in 0..4 {
            let _ = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
        }
        let window_layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("Data Flow"));
        assert!(
            !flow.canvas_layers.is_empty(),
            "canvas should paint sublayers that would otherwise shadow the window"
        );

        // The window's title highlight is decided at its `begin`, right after the
        // re-parenting, mid-pass. Observe `top_layer_id()` at that same moment.
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
    fn live_cadence_fires_only_on_new_epoch_after_interval() {
        // New epoch + interval elapsed -> fire.
        assert!(super::should_tick_live(1.000, 0.700, 200, 5, 4));
        // New epoch but interval not elapsed -> hold.
        assert!(!super::should_tick_live(0.800, 0.700, 200, 5, 4));
        // Interval elapsed but no new epoch -> hold.
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

            let id = flow.controller.graph.alloc_id();
            flow.controller.graph.insert_node(Node {
                id,
                pos: [0.0, 0.0],
                kind,
            });
            flow.controller.selection = HashSet::from([id]);

            let node_frame = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);
            let following_frame =
                render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![]);

            assert_eq!(
                node_frame.width(),
                initial.width(),
                "node frame regrew for {:?}",
                flow.controller.graph.node(id).unwrap().kind
            );
            assert_eq!(
                following_frame.width(),
                initial.width(),
                "following frame regrew for {:?}",
                flow.controller.graph.node(id).unwrap().kind
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

    fn render_add_menu_frame(ctx: &egui::Context, menu: &mut AddMenuState) -> egui::Rect {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_200.0, 800.0),
            )),
            ..Default::default()
        };
        let snapshot = StoreSnapshot::empty();
        let _ = ctx.run_ui(input, |_ui| {
            assert!(show_add_menu(ctx, menu, &snapshot).is_none());
        });
        ctx.memory(|memory| {
            memory
                .area_rect(egui::Id::new("dataflow-add-menu"))
                .expect("Add menu area should exist")
        })
    }

    #[test]
    fn add_menu_grows_back_after_filter_is_cleared() {
        let ctx = egui::Context::default();
        let mut menu = AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]);

        let _ = render_add_menu_frame(&ctx, &mut menu);
        let full = render_add_menu_frame(&ctx, &mut menu);

        menu.query = "query-that-matches-no-template".to_owned();
        let filtered = render_add_menu_frame(&ctx, &mut menu);
        assert!(filtered.height() < full.height() - 100.0);

        menu.query.clear();
        let restored = render_add_menu_frame(&ctx, &mut menu);
        assert_eq!(restored.height(), full.height());
    }

    #[test]
    fn new_add_menu_ignores_opening_outside_click_then_arms_dismissal() {
        let mut menu = AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]);

        let opening_frame_armed = std::mem::replace(&mut menu.dismiss_armed, true);

        assert!(!should_close_add_menu(opening_frame_armed, true));
        assert!(menu.dismiss_armed);
        assert!(should_close_add_menu(menu.dismiss_armed, true));
    }

    #[test]
    fn click_inside_add_menu_never_closes_it() {
        assert!(!should_close_add_menu(true, false));
        assert!(!should_close_add_menu(false, false));
    }

    #[test]
    fn menu_navigation_wraps_in_both_directions() {
        assert_eq!(move_highlight(0, 4, -1), 3);
        assert_eq!(move_highlight(3, 4, 1), 0);
        assert_eq!(move_highlight(1, 4, 1), 2);
        assert_eq!(move_highlight(9, 0, 1), 0);
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
        flow.add_menu = Some(menu);

        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = render_data_flow_frame(&ctx, &mut flow, &snapshot, &sender, vec![enter]);

        let menu = flow.add_menu.expect("add menu stays open after choosing Add Data");
        assert_eq!(menu.mode, AddMenuMode::Data);
        assert_eq!(menu.query, "altitude");
    }

    #[test]
    fn closing_data_flow_clears_open_add_menu() {
        let mut ui = DataFlowUi::new();
        ui.open = true;
        ui.add_menu = Some(AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]));

        ui.update_open(false);

        assert!(!ui.open);
        assert!(ui.add_menu.is_none());
    }

    #[test]
    fn loaded_graph_resets_canvas_from_loaded_positions() {
        let mut ui = DataFlowUi::new();
        ui.canvas
            .view
            .layout
            .insert(ui_node_id(NodeId(99)), egui::pos2(1.0, 2.0));
        let mut graph = Graph::new("loaded");
        graph.viewport = Viewport {
            offset: [12.0, -4.0],
            zoom: 1.5,
        };
        graph.insert_node(Node {
            id: NodeId(7),
            pos: [30.0, 40.0],
            kind: NodeKind::Add,
        });

        ui.replace_graph(graph);

        assert_eq!(ui.controller.graph.name, "loaded");
        assert_eq!(ui.canvas.viewport, ui.controller.graph.viewport);
        assert_eq!(
            ui.canvas.view.layout[&ui_node_id(NodeId(7))],
            egui::pos2(30.0, 40.0)
        );
        assert!(!ui.canvas.view.layout.contains_key(&ui_node_id(NodeId(99))));
    }

    #[test]
    fn new_graph_replacement_clears_selection_and_undo_history() {
        let mut ui = DataFlowUi::new();
        let id = ui.controller.graph.alloc_id();
        ui.controller
            .apply(GraphCommand::AddNode {
                node: Node {
                    id,
                    pos: [0.0, 0.0],
                    kind: NodeKind::Constant { value: 1.0 },
                },
            })
            .unwrap();
        ui.controller.selection = HashSet::from([id]);

        ui.replace_graph(Graph::new("untitled"));

        assert!(ui.controller.selection.is_empty());
        assert!(!ui.controller.can_undo());
        assert!(!ui.controller.dirty);
        assert!(ui.canvas.view.layout.is_empty());
        assert_eq!(ui.canvas.viewport, Viewport::default());
    }
}

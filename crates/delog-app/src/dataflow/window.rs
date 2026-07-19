use std::sync::Arc;

use delog_core::align::AlignMode;
use delog_core::ingest::IngestSender;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::command::GraphCommand;
use delog_flow::graph::{FieldSelector, Graph, Node, NodeId, NodeKind, OutputFieldSpec};

use super::canvas::{CanvasEvent, CanvasState, show_canvas};
use super::controller::DataFlowController;
use super::picker::{DataHit, search_fields};
use super::registry::{ADD_DATA_INDEX, search_templates, templates};
use super::store::GraphStore;
use crate::logging::LogLevel;

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

pub struct DataFlowUi {
    pub open: bool,
    controller: DataFlowController,
    store: GraphStore,
    canvas: CanvasState,
    add_menu: Option<AddMenuState>,
    name_edit: String,
    loaded_name: Option<String>,
    pending_delete: Option<String>,
    canvas_layers: Vec<egui::LayerId>,
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
            canvas_layers: Vec::new(),
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        sender: &IngestSender,
    ) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        let mut open = self.open;
        let window_layer = self.reassert_canvas_sublayers(ctx);
        egui::Window::new("Data Flow")
            .open(&mut open)
            .default_size([980.0, 640.0])
            .min_size([720.0, 420.0])
            .show(ctx, |ui| {
                bounded_window_body(ui, |ui| {
                    egui::Panel::bottom("dataflow_footer")
                        .show_inside(ui, |ui| self.footer(ui));
                    egui::Panel::left("dataflow_library_drawer")
                        .resizable(true)
                        .default_size(180.0)
                        .size_range(140.0..=260.0)
                        .show_inside(ui, |ui| self.library_drawer(ui, &mut logs));
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        self.toolbar(ui, snapshot, &mut logs);
                        ui.separator();

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
                                            self.controller.selection,
                                            &mut self.canvas,
                                        )
                                    },
                                )
                                .inner;
                            ui.separator();
                            ui.allocate_ui_with_layout(
                                egui::vec2(260.0, height),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| self.inspector(ui, &mut logs),
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
                    self.controller.selection = Some(id);
                }
                Some(AddAction::Data(hit)) => {
                    let id = self.controller.graph.alloc_id();
                    let node = Node {
                        id,
                        pos: menu.canvas_pos,
                        kind: NodeKind::DataField(hit.selector),
                    };
                    self.apply(GraphCommand::AddNode { node }, &mut logs);
                    self.controller.selection = Some(id);
                }
                Some(AddAction::Close) => {}
                None => self.add_menu = Some(menu),
            }
        }

        if self.controller.needs_eval() {
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
            if icon_btn_enabled(ui, true, crate::icons::play(), "Run").clicked() {
                self.controller.request_publish(Arc::clone(snapshot));
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

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.controller.is_evaluating() {
                ui.spinner();
            }
            ui.weak("Snapshot only - processes currently loaded data");
        });
    }

    fn library_drawer(&mut self, ui: &mut egui::Ui, logs: &mut Vec<(LogLevel, String)>) {
        ui.horizontal(|ui| {
            ui.strong("Data Flows");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

    fn inspector(&mut self, ui: &mut egui::Ui, logs: &mut Vec<(LogLevel, String)>) {
        ui.heading("Inspector");
        let Some(id) = self.controller.selection else {
            ui.weak("Select a node to inspect it");
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
            NodeKind::Add
            | NodeKind::Subtract
            | NodeKind::Multiply
            | NodeKind::Divide
            | NodeKind::Unknown(_) => {}
        }
        if let Some(command) = structural_edit {
            self.apply(command, logs);
        } else if edited != node.kind {
            self.apply(GraphCommand::SetKind { id, kind: edited }, logs);
        }

        if let Some(preview) = self.controller.preview_for(id) {
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
                CanvasEvent::Moved { id, from, to } => {
                    let _ = from;
                    self.apply(GraphCommand::MoveNode { id, to }, logs);
                }
                CanvasEvent::Connect { from, to, to_port } => {
                    match self.controller.graph.check_connect(from, to, to_port) {
                        Ok(()) => self.apply(GraphCommand::Connect { from, to, to_port }, logs),
                        Err(error) => {
                            logs.push((LogLevel::Error, format!("Cannot connect nodes: {error:?}")))
                        }
                    }
                }
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
        self.controller.replace_graph(graph);
    }
}

fn show_selector(ui: &mut egui::Ui, selector: &FieldSelector) {
    ui.label(format!(
        "Source: {}",
        selector.source.as_deref().unwrap_or("any")
    ));
    ui.label(format!("Topic: {}", selector.topic));
    if let Some(instance) = selector.instance {
        ui.label(format!("Instance: {instance}"));
    }
    ui.label(format!("Field: {}", selector.field));
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
        vec![
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
        ]
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
            let _ = flow.show(ui.ctx(), snapshot, sender);
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
        flow.controller.selection = Some(id);
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
            graph.connect(NodeId(1), id, 0).unwrap();
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
            flow.controller.selection = Some(id);

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
        ui.controller.selection = Some(id);

        ui.replace_graph(Graph::new("untitled"));

        assert!(ui.controller.selection.is_none());
        assert!(!ui.controller.can_undo());
        assert!(!ui.controller.dirty);
        assert!(ui.canvas.view.layout.is_empty());
        assert_eq!(ui.canvas.viewport, Viewport::default());
    }
}

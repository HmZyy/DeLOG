use std::collections::BTreeMap;
use std::sync::Arc;

use delog_core::ingest::IngestSender;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::graph::Graph;
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};

use super::controller::PublishedSources;
use super::editor::{DataFlowEditor, EditorAction};
use super::store::GraphStore;
use crate::ui::components::LibraryAction;
use crate::ui::logging::LogLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataFlowTab {
    Dataflows,
    Editor(u64),
    Inspector,
}

impl DataFlowTab {
    fn is_closeable(self) -> bool {
        matches!(self, Self::Editor(_))
    }
}

#[derive(Clone, Copy)]
enum CloseDecision {
    Save,
    Discard,
    Cancel,
}

enum WorkspaceAction {
    New,
    Open(String),
    Duplicate(String),
    Delete(String),
    Close(u64),
    Save(u64),
    Run(u64),
}

pub struct DataFlowUi {
    pub open: bool,
    dock: DockState<DataFlowTab>,
    editors: BTreeMap<u64, DataFlowEditor>,
    active: u64,
    next_id: u64,
    published: PublishedSources,
    store: GraphStore,
    pending_close: Option<u64>,
    close_error: Option<String>,
    pending_delete: Option<String>,
    closed_editors: Vec<DataFlowEditor>,
    canvas_layers: Vec<egui::LayerId>,
}

impl DataFlowUi {
    pub fn new() -> Self {
        let published = PublishedSources::default();
        let mut dock = DockState::new(vec![DataFlowTab::Editor(1)]);
        let [center, _] = dock.main_surface_mut().split_left(
            NodeIndex::root(),
            0.18,
            vec![DataFlowTab::Dataflows],
        );
        dock.main_surface_mut()
            .split_right(center, 0.74, vec![DataFlowTab::Inspector]);
        Self {
            open: false,
            dock,
            editors: BTreeMap::from([(
                1,
                DataFlowEditor::new(1, Graph::new("untitled"), None, Arc::clone(&published)),
            )]),
            active: 1,
            next_id: 2,
            published,
            store: GraphStore::new(
                GraphStore::default_dir()
                    .unwrap_or_else(|| std::env::temp_dir().join("delog-dataflows")),
            ),
            pending_close: None,
            close_error: None,
            pending_delete: None,
            closed_editors: Vec::new(),
            canvas_layers: Vec::new(),
        }
    }

    #[cfg(feature = "scripting")]
    pub fn has_script_node(&self) -> bool {
        self.editors.values().any(|editor| {
            editor
                .controller
                .graph
                .nodes
                .iter()
                .any(|node| matches!(node.kind, delog_flow::graph::NodeKind::Script(_)))
        })
    }

    #[cfg(feature = "scripting")]
    pub fn set_script_host(&mut self, host: Option<delog_script::flow::EngineFlowHost>) {
        let host =
            host.map(|host| Arc::new(host) as Arc<dyn delog_flow::script::ScriptNodeHost + Sync>);
        for editor in self.editors.values_mut() {
            editor.controller.set_script_host(host.clone());
        }
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
        let mut actions = Vec::new();
        let window_layer = self.reassert_canvas_sublayers(ctx);
        let mut dock = std::mem::replace(&mut self.dock, DockState::new(Vec::new()));
        egui::Window::new("Data Flow")
            .open(&mut open)
            .default_size([1180.0, 720.0])
            .min_size([800.0, 420.0])
            .show(ctx, |ui| {
                bounded_window_body(ui, |ui| {
                    let style = workspace_style(ui.style());
                    DockArea::new(&mut dock)
                        .id(egui::Id::new("dataflow-workspace"))
                        .style(style)
                        .allowed_splits(egui_dock::AllowedSplits::None)
                        .draggable_tabs(false)
                        .tab_context_menus(false)
                        .show_close_buttons(true)
                        .show_leaf_close_all_buttons(false)
                        .show_leaf_collapse_buttons(false)
                        .show_inside(
                            ui,
                            &mut WorkspaceViewer {
                                workspace: self,
                                snapshot,
                                live_connected,
                                logs: &mut logs,
                                actions: &mut actions,
                            },
                        );
                });
            });
        self.dock = dock;
        for action in actions {
            match action {
                WorkspaceAction::New => self.new_graph(),
                WorkspaceAction::Open(name) => self.edit_named(&name, &mut logs),
                WorkspaceAction::Duplicate(name) => self.duplicate(&name, &mut logs),
                WorkspaceAction::Delete(name) => self.pending_delete = Some(name),
                WorkspaceAction::Close(id) => self.request_close(id),
                WorkspaceAction::Save(id) => {
                    self.save_editor(id, &mut logs);
                }
                WorkspaceAction::Run(id) => {
                    if self.name_available(id, &mut logs) {
                        self.editors
                            .get_mut(&id)
                            .unwrap()
                            .run(snapshot, live_connected);
                    }
                }
            }
            ctx.request_repaint();
        }
        self.canvas_layers = descendant_layers(ctx, window_layer);
        self.update_open(open);
        self.close_confirm(ctx, &mut logs);
        self.delete_confirm(ctx, &mut logs);
        if self.open && self.pending_close.is_none() && self.pending_delete.is_none() {
            self.active_editor_mut()
                .show_add_menu(ctx, snapshot, &mut logs);
            let pointer_over_window = ctx
                .memory(|memory| memory.area_rect(egui::Id::new("Data Flow")))
                .zip(ctx.pointer_hover_pos())
                .is_some_and(|(rect, pointer)| rect.contains(pointer));
            self.active_editor_mut()
                .handle_shortcuts(ctx, pointer_over_window, &mut logs);
        }
        logs
    }

    pub fn drive(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        sender: &IngestSender,
        live_connected: bool,
        settings: crate::config::settings::DataFlowSettings,
    ) -> Vec<(LogLevel, String)> {
        for mut editor in self.closed_editors.drain(..) {
            editor.controller.stop(sender);
        }
        let mut logs = Vec::new();
        for editor in self.editors.values_mut() {
            logs.extend(editor.drive(ctx, snapshot, sender, live_connected, settings));
        }
        logs
    }

    fn active_editor(&self) -> &DataFlowEditor {
        &self.editors[&self.active]
    }

    fn active_editor_mut(&mut self) -> &mut DataFlowEditor {
        self.editors
            .get_mut(&self.active)
            .expect("an editor is always open")
    }

    fn activate(&mut self, id: u64) {
        if self.active != id && self.editors.contains_key(&id) {
            self.active_editor_mut().add_menu = None;
            self.active = id;
        }
    }

    fn focus_editor(&mut self, id: u64) {
        if let Some(path) = self.dock.find_tab(&DataFlowTab::Editor(id)) {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
            self.activate(id);
        }
    }

    fn add_editor(&mut self, graph: Graph, loaded_name: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let path = self
            .dock
            .find_tab(&DataFlowTab::Editor(self.active))
            .expect("the active editor has a dock tab");
        self.dock.main_surface_mut()[path.node].append_tab(DataFlowTab::Editor(id));
        self.editors.insert(
            id,
            DataFlowEditor::new(id, graph, loaded_name, Arc::clone(&self.published)),
        );
        self.focus_editor(id);
        id
    }

    fn new_graph(&mut self) {
        let mut names = self.store.list();
        names.extend(self.editors.values().map(|editor| editor.name_edit.clone()));
        let mut name = "untitled".to_owned();
        for suffix in 2.. {
            if !names.contains(&name) {
                break;
            }
            name = format!("untitled {suffix}");
        }
        self.add_editor(Graph::new(&name), None);
    }

    fn edit_named(&mut self, name: &str, logs: &mut Vec<(LogLevel, String)>) {
        if let Some((&id, _)) = self
            .editors
            .iter()
            .find(|(_, editor)| editor.loaded_name.as_deref() == Some(name))
        {
            self.focus_editor(id);
            return;
        }
        match self.store.load(name) {
            Ok(graph) => {
                let empty = self.editors.len() == 1
                    && self.active_editor().loaded_name.is_none()
                    && self.active_editor().name_edit == "untitled"
                    && self.active_editor().controller.graph.nodes.is_empty()
                    && !self.active_editor().has_unsaved_changes();
                let previous = self.active;
                self.add_editor(graph, Some(name.to_owned()));
                if empty {
                    self.close_editor(previous);
                }
            }
            Err(error) => logs.push((LogLevel::Error, error)),
        }
    }

    fn name_available(&self, id: u64, logs: &mut Vec<(LogLevel, String)>) -> bool {
        let name = &self.editors[&id].name_edit;
        if self.editors.iter().any(|(&other_id, editor)| {
            other_id != id
                && (editor.loaded_name.as_deref() == Some(name.as_str())
                    || editor.name_edit == *name)
        }) {
            logs.push((
                LogLevel::Error,
                format!(
                    "Data flow '{name}' is already open in another tab. Choose a different name."
                ),
            ));
            return false;
        }
        true
    }

    fn save_editor(&mut self, id: u64, logs: &mut Vec<(LogLevel, String)>) -> bool {
        self.name_available(id, logs) && self.editors.get_mut(&id).unwrap().save(&self.store, logs)
    }

    fn request_close(&mut self, id: u64) {
        if self.pending_close.is_some() {
            return;
        }
        if let Some(editor) = self.editors.get(&id) {
            if editor.has_unsaved_changes() {
                self.pending_close = Some(id);
                self.close_error = None;
                self.focus_editor(id);
            } else {
                self.close_editor(id);
            }
        }
    }

    fn close_editor(&mut self, id: u64) {
        let Some(path) = self.dock.find_tab(&DataFlowTab::Editor(id)) else {
            return;
        };
        if self.editors.len() == 1 {
            self.add_editor(Graph::new("untitled"), None);
        }
        self.dock.remove_tab(path);
        if let Some(editor) = self.editors.remove(&id) {
            self.closed_editors.push(editor);
        }
        if self.active == id {
            let next = self
                .dock
                .main_surface_mut()
                .find_active_focused()
                .and_then(|(_, tab)| match tab {
                    DataFlowTab::Editor(id) => Some(*id),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    *self
                        .editors
                        .keys()
                        .next()
                        .expect("an editor is always open")
                });
            self.active = next;
            self.focus_editor(next);
        }
    }

    fn close_confirm(&mut self, ctx: &egui::Context, logs: &mut Vec<(LogLevel, String)>) {
        let Some(id) = self.pending_close else {
            return;
        };
        let Some(editor) = self.editors.get(&id) else {
            self.pending_close = None;
            return;
        };
        let mut decision = None;
        let response = egui::Modal::new(egui::Id::new("dataflow-close-confirm")).show(ctx, |ui| {
            ui.heading("Save changes?");
            ui.label(format!(
                "Save changes to “{}” before closing?",
                editor.name_edit
            ));
            if let Some(error) = &self.close_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    decision = Some(CloseDecision::Save);
                }
                if ui.button("Discard").clicked() {
                    decision = Some(CloseDecision::Discard);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(CloseDecision::Cancel);
                }
            });
        });
        if response.should_close() && decision.is_none() {
            decision = Some(CloseDecision::Cancel);
        }
        match decision {
            Some(CloseDecision::Save) => {
                if self.save_editor(id, logs) {
                    self.pending_close = None;
                    self.close_error = None;
                    self.close_editor(id);
                } else {
                    self.close_error = logs.last().map(|(_, message)| message.clone());
                }
            }
            Some(CloseDecision::Discard) => {
                self.pending_close = None;
                self.close_error = None;
                self.close_editor(id);
            }
            Some(CloseDecision::Cancel) => {
                self.pending_close = None;
                self.close_error = None;
            }
            None => {}
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
        let mut decision = None;
        let response = egui::Modal::new(egui::Id::new("dataflow-delete-confirm")).show(ctx, |ui| {
            ui.heading("Delete data flow?");
            ui.label(format!("Delete “{name}” from the library?"));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        if response.should_close() && decision.is_none() {
            decision = Some(false);
        }
        if let Some(delete) = decision {
            if delete {
                match self.store.delete(&name) {
                    Ok(()) => {
                        for editor in self.editors.values_mut() {
                            if editor.loaded_name.as_deref() == Some(&name) {
                                editor.loaded_name = None;
                                editor.controller.dirty = true;
                            }
                        }
                        logs.push((LogLevel::Info, format!("Deleted data flow '{name}'")));
                    }
                    Err(error) => logs.push((LogLevel::Error, error)),
                }
            }
            self.pending_delete = None;
        }
    }

    fn update_open(&mut self, open: bool) {
        self.open = open;
        if !open {
            for editor in self.editors.values_mut() {
                editor.add_menu = None;
            }
        }
    }

    fn reassert_canvas_sublayers(&self, ctx: &egui::Context) -> egui::LayerId {
        let window_layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("Data Flow"));
        for child in &self.canvas_layers {
            ctx.set_sublayer(window_layer, *child);
        }
        window_layer
    }
}

struct WorkspaceViewer<'a> {
    workspace: &'a mut DataFlowUi,
    snapshot: &'a Arc<StoreSnapshot>,
    live_connected: bool,
    logs: &'a mut Vec<(LogLevel, String)>,
    actions: &'a mut Vec<WorkspaceAction>,
}

impl TabViewer for WorkspaceViewer<'_> {
    type Tab = DataFlowTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            DataFlowTab::Dataflows => "Dataflows".into(),
            DataFlowTab::Inspector => "Inspector".into(),
            DataFlowTab::Editor(id) => self.workspace.editors[id].title().into(),
        }
    }

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        match tab {
            DataFlowTab::Dataflows => egui::Id::new("dataflow-library-tab"),
            DataFlowTab::Inspector => egui::Id::new("dataflow-inspector-tab"),
            DataFlowTab::Editor(id) => egui::Id::new(("dataflow-editor-tab", *id)),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match *tab {
            DataFlowTab::Dataflows => {
                egui::Frame::new().inner_margin(8).show(ui, |ui| {
                    if ui
                        .add_sized([ui.available_width(), 28.0], egui::Button::new("+ New"))
                        .clicked()
                    {
                        self.actions.push(WorkspaceAction::New);
                    }
                    ui.add_space(8.0);
                    let names = self.workspace.store.list();
                    if names.is_empty() {
                        ui.weak("No saved dataflows");
                        return;
                    }
                    let selected = self.workspace.active_editor().loaded_name.as_deref();
                    let event = egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            crate::ui::components::library_tree(
                                ui,
                                egui::Id::new("dataflow_library_tree"),
                                &names,
                                selected,
                                &[LibraryAction::Duplicate, LibraryAction::Remove],
                                "Open data flow",
                            )
                        })
                        .inner;
                    if let Some(event) = event {
                        self.actions.push(match event.action {
                            LibraryAction::Load | LibraryAction::Edit => {
                                WorkspaceAction::Open(event.name)
                            }
                            LibraryAction::Duplicate => WorkspaceAction::Duplicate(event.name),
                            LibraryAction::Remove => WorkspaceAction::Delete(event.name),
                        });
                    }
                });
            }
            DataFlowTab::Editor(id) => {
                self.workspace.activate(id);
                if let Some(action) = self.workspace.editors.get_mut(&id).unwrap().show(
                    ui,
                    self.snapshot,
                    self.live_connected,
                    self.logs,
                ) {
                    self.actions.push(match action {
                        EditorAction::Save => WorkspaceAction::Save(id),
                        EditorAction::Run => WorkspaceAction::Run(id),
                    });
                }
            }
            DataFlowTab::Inspector => {
                let id = self.workspace.active;
                ui.push_id(id, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Frame::new().inner_margin(12).show(ui, |ui| {
                                self.workspace.active_editor_mut().inspector(
                                    ui,
                                    self.snapshot,
                                    self.logs,
                                );
                            });
                        });
                });
            }
        }
    }

    fn on_tab_button(&mut self, tab: &mut Self::Tab, response: &egui::Response) {
        let selected = match *tab {
            DataFlowTab::Editor(id) => self.workspace.active == id,
            _ => true,
        };
        let title = self.title(tab);
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                selected,
                title.text(),
            )
        });
        response.ctx.accesskit_node_builder(response.id, |node| {
            node.set_role(egui::accesskit::Role::Tab);
        });
        if response.clicked()
            && let DataFlowTab::Editor(id) = *tab
        {
            self.workspace.activate(id);
        }
    }

    fn is_closeable(&self, tab: &Self::Tab) -> bool {
        tab.is_closeable()
    }

    fn on_close(&mut self, tab: &mut Self::Tab) -> OnCloseResponse {
        if let DataFlowTab::Editor(id) = *tab {
            self.actions.push(WorkspaceAction::Close(id));
        }
        OnCloseResponse::Ignore
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }

    fn scroll_bars(&self, _tab: &Self::Tab) -> [bool; 2] {
        [false, false]
    }
}

fn workspace_style(style: &egui::Style) -> egui_dock::Style {
    let mut dock = crate::ui::docks::dock_style(style);
    let border = style.visuals.widgets.noninteractive.bg_stroke.color;
    dock.dock_area_padding = Some(egui::Margin::ZERO);
    dock.main_surface_border_stroke = egui::Stroke::NONE;
    dock.main_surface_border_rounding = egui::CornerRadius::ZERO;
    dock.tab_bar.height = 30.0;
    dock.tab_bar.bg_fill = style.visuals.faint_bg_color;
    dock.tab_bar.hline_color = border;
    dock.tab_bar.corner_radius = egui::CornerRadius::ZERO;
    dock.tab.tab_body.inner_margin = egui::Margin::ZERO;
    dock.tab.tab_body.stroke = egui::Stroke::NONE;
    dock.tab.tab_body.corner_radius = egui::CornerRadius::ZERO;
    dock.tab.tab_body.bg_fill = style.visuals.panel_fill;
    dock.tab.spacing = 1.0;
    dock.separator.width = 1.0;
    dock.separator.extra_interact_width = 6.0;
    dock.separator.extra = 150.0;
    dock.separator.color_idle = border;
    dock.separator.color_hovered = style.visuals.selection.bg_fill;
    dock.separator.color_dragged = style.visuals.selection.bg_fill;
    dock
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
#[path = "window_tests.rs"]
mod tests;

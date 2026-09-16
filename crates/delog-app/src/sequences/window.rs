use super::doc::{SequenceDoc, StepKind};
use super::runner::{RunStatus, SequenceRunner};
use super::store::SequenceStore;
use crate::ui::{components, icons};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct Catalog {
    pub parsers: Vec<String>,
    pub scripts: Vec<String>,
    pub dataflows: Vec<String>,
    pub layouts: Vec<String>,
}
impl Catalog {
    fn items(&self, kind: StepKind) -> &[String] {
        match kind {
            StepKind::Parser => &self.parsers,
            StepKind::Script => &self.scripts,
            StepKind::Dataflow => &self.dataflows,
            StepKind::Layout => &self.layouts,
        }
    }
}

pub enum ManagerAction {
    Run(SequenceDoc),
    Delete(String),
    Cancel,
}

pub struct SequenceManager {
    pub open: bool,
    pub draft: Option<SequenceDoc>,
    store: Option<SequenceStore>,
    saved_name: Option<String>,
    names: Vec<String>,
    error: Option<String>,
    step_search: String,
    delete_pending: Option<String>,
}

#[derive(Clone)]
struct StepDrag {
    sequence: String,
    step: u64,
}

impl Default for SequenceManager {
    fn default() -> Self {
        Self {
            open: false,
            draft: None,
            store: SequenceStore::default_store(),
            saved_name: None,
            names: Vec::new(),
            error: None,
            step_search: String::new(),
            delete_pending: None,
        }
    }
}

impl SequenceManager {
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn refresh(&mut self) {
        match self
            .store
            .as_ref()
            .ok_or_else(|| "application data directory is unavailable".to_owned())
            .and_then(SequenceStore::list)
        {
            Ok(names) => self.names = names,
            Err(error) => self.error = Some(error),
        }
    }
    fn save(&mut self) -> Result<(), String> {
        let doc = self.draft.as_ref().ok_or("no sequence selected")?;
        doc.validate()?;
        let store = self
            .store
            .as_ref()
            .ok_or("application data directory is unavailable")?;
        if self.saved_name.as_deref() != Some(&doc.name) && self.names.contains(&doc.name) {
            return Err("a sequence with that name already exists".into());
        }
        if let Some(old) = &self.saved_name {
            if old != &doc.name {
                let mut previous_name = doc.clone();
                previous_name.name = old.clone();
                store.save(&previous_name)?;
                store.rename(old, &doc.name)?;
            } else {
                store.save(doc)?;
            }
        } else {
            store.save(doc)?;
        }
        self.saved_name = Some(doc.name.clone());
        self.refresh();
        Ok(())
    }
    fn save_or_report(&mut self) -> bool {
        match self.save() {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
    fn library_action(&mut self, event: components::LibraryEvent) {
        use components::LibraryAction;
        if event.action == LibraryAction::Remove {
            self.delete_pending = Some(event.name);
            return;
        }
        let selected = self.saved_name.as_ref() == Some(&event.name);
        if self.draft.is_some() && !self.save_or_report() {
            return;
        }
        let name = if selected {
            self.saved_name.as_deref().unwrap_or(&event.name)
        } else {
            &event.name
        };
        let result = self
            .store
            .as_ref()
            .ok_or_else(|| "application data directory is unavailable".to_owned())
            .and_then(|store| store.load(name));
        match result {
            Ok(doc) => {
                if event.action == LibraryAction::Duplicate {
                    match self.duplicate(&doc) {
                        Ok(copy) => {
                            self.saved_name = Some(copy.name.clone());
                            self.draft = Some(copy);
                            self.refresh();
                        }
                        Err(error) => self.error = Some(error),
                    }
                } else {
                    self.saved_name = Some(doc.name.clone());
                    self.draft = Some(doc);
                }
                self.delete_pending = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn duplicate(&self, source: &SequenceDoc) -> Result<SequenceDoc, String> {
        let store = self
            .store
            .as_ref()
            .ok_or("application data directory is unavailable")?;
        let names = store.list()?;
        let mut name = format!("{} copy", source.name);
        let mut index = 2;
        while names.contains(&name) {
            name = format!("{} copy {index}", source.name);
            index += 1;
        }
        let mut copy = SequenceDoc::new(&name);
        for step in &source.steps {
            copy.push(step.kind, &step.reference);
        }
        store.save(&copy)?;
        Ok(copy)
    }

    fn delete_confirm(
        &mut self,
        ctx: &egui::Context,
        runs: &BTreeMap<String, SequenceRunner>,
        actions: &mut Vec<ManagerAction>,
    ) {
        let Some(name) = self.delete_pending.clone() else {
            return;
        };
        let running = runs
            .values()
            .any(|run| run.doc.name == name && run.status == RunStatus::Running);
        let mut decision = None;
        let response = egui::Modal::new(egui::Id::new("sequence-delete-confirm")).show(ctx, |ui| {
            ui.heading("Delete sequence?");
            ui.label(format!("Delete “{name}” from the library?"));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!running, egui::Button::new("Delete"))
                    .on_disabled_hover_text("Cancel the running sequence before deleting it")
                    .clicked()
                {
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
                let result = self
                    .store
                    .as_ref()
                    .ok_or_else(|| "application data directory is unavailable".to_owned())
                    .and_then(|store| {
                        let doc = store.load(&name)?;
                        store.delete(&name)?;
                        Ok(doc)
                    });
                match result {
                    Ok(doc) => {
                        actions.push(ManagerAction::Delete(doc.id));
                        if self.saved_name.as_ref() == Some(&name) {
                            self.saved_name = None;
                            self.draft = None;
                        }
                        self.refresh();
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            self.delete_pending = None;
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        catalog: &Catalog,
        runs: &BTreeMap<String, SequenceRunner>,
        live: &BTreeSet<String>,
    ) -> Vec<ManagerAction> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        let mut open = self.open;
        egui::Window::new("Sequences")
            .open(&mut open)
            .collapsible(false)
            .default_size([760.0, 440.0])
            .min_size([640.0, 320.0])
            .show(ctx, |ui| {
                ui.scope(|ui| {
                    let library_max_width = (ui.available_width() - 400.0).max(160.0);
                    egui::Panel::left("sequence-library-panel")
                        .resizable(true)
                        .default_size(190.0)
                        .size_range(160.0..=library_max_width)
                        .frame(egui::Frame::NONE)
                        .show_inside(ui, |ui| {
                            egui::Frame::new().inner_margin(8).show(ui, |ui| {
                                if ui
                                    .add_sized(
                                        [ui.available_width(), 28.0],
                                        egui::Button::new("+ New"),
                                    )
                                    .clicked()
                                    && (self.draft.is_none() || self.save_or_report())
                                {
                                    let mut index = 1;
                                    while self.names.contains(&format!("Sequence {index}")) {
                                        index += 1;
                                    }
                                    self.draft =
                                        Some(SequenceDoc::new(&format!("Sequence {index}")));
                                    self.saved_name = None;
                                    self.delete_pending = None;
                                    self.save_or_report();
                                }
                                ui.add_space(8.0);
                                let names = self.names.clone();
                                egui::ScrollArea::vertical().id_salt("sequence-list").show(
                                    ui,
                                    |ui| {
                                        if let Some(event) = components::library_tree(
                                            ui,
                                            egui::Id::new("sequence-library"),
                                            &names,
                                            self.saved_name.as_deref(),
                                            &[
                                                components::LibraryAction::Duplicate,
                                                components::LibraryAction::Remove,
                                            ],
                                            "Edit sequence",
                                        ) {
                                            self.library_action(event);
                                        }
                                        if names.is_empty() {
                                            ui.weak("No saved sequences");
                                        }
                                    },
                                );
                            });
                        });
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().inner_margin(8))
                        .show_inside(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_min_width(380.0);
                                let mut save = false;
                                let mut run_clicked = false;
                                if let Some(doc) = &mut self.draft {
                                    let run = runs.get(&doc.id);
                                    let running =
                                        run.is_some_and(|run| run.status == RunStatus::Running);
                                    ui.horizontal(|ui| {
                                        ui.add_enabled(
                                            !running,
                                            egui::TextEdit::singleline(&mut doc.name)
                                                .hint_text("Sequence name")
                                                .desired_width(160.0),
                                        );
                                        run_clicked = ui
                                            .add_enabled_ui(!running, |ui| {
                                                components::icon_button(
                                                    ui,
                                                    icons::play(),
                                                    "Run sequence",
                                                    false,
                                                )
                                                .clicked()
                                            })
                                            .inner;
                                        save = components::icon_button(
                                            ui,
                                            icons::save(),
                                            "Save sequence",
                                            false,
                                        )
                                        .clicked();
                                        if running && ui.button("Cancel").clicked() {
                                            actions.push(ManagerAction::Cancel);
                                        }
                                        if ui
                                            .add_enabled_ui(!running, |ui| {
                                                components::icon_button(
                                                    ui,
                                                    icons::trash(),
                                                    "Delete sequence",
                                                    false,
                                                )
                                                .clicked()
                                            })
                                            .inner
                                        {
                                            self.delete_pending = self.saved_name.clone();
                                        }
                                    });
                                    if let Some(run) = run {
                                        ui.label(format!("Last run: {:?}", run.status));
                                        if let Some(error) = &run.error {
                                            ui.colored_label(ui.visuals().error_fg_color, error);
                                        }
                                    }
                                    if live.contains(&doc.id) {
                                        ui.label("Dataflows active in background");
                                    }
                                    ui.add_space(12.0);
                                    let mut remove = None;
                                    let mut movement = None;
                                    egui::ScrollArea::vertical()
                                        .id_salt("sequence-steps")
                                        .max_height(250.0)
                                        .show(ui, |ui| {
                                            for (index, step) in doc.steps.iter().enumerate() {
                                                let payload = StepDrag {
                                                    sequence: doc.id.clone(),
                                                    step: step.id,
                                                };
                                                let row = ui
                                                    .horizontal(|ui| {
                                                        ui.set_min_height(40.0);
                                                        ui.dnd_drag_source(
                                                            egui::Id::new((
                                                                "sequence-step",
                                                                &doc.id,
                                                                step.id,
                                                            )),
                                                            payload,
                                                            |ui| {
                                                                ui.add(
                                                                    egui::Image::new(
                                                                        icons::grip_vertical(),
                                                                    )
                                                                    .fit_to_exact_size(egui::vec2(
                                                                        18.0, 18.0,
                                                                    ))
                                                                    .tint(
                                                                        ui.visuals()
                                                                            .weak_text_color(),
                                                                    )
                                                                    .alt_text("Drag to reorder"),
                                                                )
                                                                .on_hover_text("Drag to reorder");
                                                            },
                                                        );
                                                        ui.weak(format!("{:02}", index + 1));
                                                        ui.vertical(|ui| {
                                                            ui.strong(&step.reference);
                                                            ui.weak(step.kind.label());
                                                        });
                                                        if let Some(run) = run {
                                                            if let Some(position) = run
                                                                .doc
                                                                .steps
                                                                .iter()
                                                                .position(|s| s.id == step.id)
                                                            {
                                                                ui.weak(format!(
                                                                    "{:?}",
                                                                    run.states[position]
                                                                ));
                                                            }
                                                        }
                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(
                                                                egui::Align::Center,
                                                            ),
                                                            |ui| {
                                                                if components::icon_button(
                                                                    ui,
                                                                    icons::trash(),
                                                                    "Remove step",
                                                                    false,
                                                                )
                                                                .clicked()
                                                                {
                                                                    remove = Some(index);
                                                                }
                                                            },
                                                        );
                                                    })
                                                    .response;
                                                if row.contains_pointer()
                                                    && egui::DragAndDrop::has_payload_of_type::<
                                                        StepDrag,
                                                    >(
                                                        ui.ctx()
                                                    )
                                                {
                                                    ui.painter().hline(
                                                        row.rect.x_range(),
                                                        row.rect.top(),
                                                        ui.visuals().selection.stroke,
                                                    );
                                                }
                                                if let Some(payload) =
                                                    row.dnd_release_payload::<StepDrag>()
                                                {
                                                    if payload.sequence == doc.id
                                                        && let Some(from) = doc
                                                            .steps
                                                            .iter()
                                                            .position(|s| s.id == payload.step)
                                                    {
                                                        movement = Some((from, index));
                                                    }
                                                }
                                            }
                                        });
                                    if let Some(index) = remove {
                                        doc.steps.remove(index);
                                    } else if let Some((from, to)) = movement {
                                        let _ = doc.move_step(from, to);
                                    }
                                    ui.add_space(12.0);
                                    ui.menu_button("+ Add step", |ui| {
                                        ui.set_min_width(260.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.step_search)
                                                .hint_text("Search saved items"),
                                        );
                                        let query = self.step_search.to_lowercase();
                                        egui::ScrollArea::vertical().max_height(280.0).show(
                                            ui,
                                            |ui| {
                                                let mut found = false;
                                                for kind in StepKind::ALL {
                                                    let items: Vec<_> = catalog
                                                        .items(kind)
                                                        .iter()
                                                        .filter(|name| {
                                                            name.to_lowercase().contains(&query)
                                                        })
                                                        .collect();
                                                    if items.is_empty() {
                                                        continue;
                                                    }
                                                    found = true;
                                                    ui.add_space(8.0);
                                                    ui.weak(kind.label());
                                                    for name in items {
                                                        if ui.button(name).clicked() {
                                                            doc.push(kind, name);
                                                            self.step_search.clear();
                                                            ui.close();
                                                        }
                                                    }
                                                }
                                                if !found {
                                                    ui.weak("No matching saved items");
                                                }
                                            },
                                        );
                                    });
                                    if running {
                                        ui.weak("Edits apply to the next run.");
                                    }
                                } else {
                                    ui.centered_and_justified(|ui| {
                                        ui.label("Create or select a sequence.");
                                    });
                                }
                                if (save || run_clicked) && self.save_or_report() && run_clicked {
                                    actions.push(ManagerAction::Run(
                                        self.draft.as_ref().unwrap().clone(),
                                    ));
                                }
                            });
                        });
                });
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        self.delete_confirm(ctx, runs, &mut actions);
        self.open = open;
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        manager: &mut SequenceManager,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                manager.show(
                    ui.ctx(),
                    &Catalog::default(),
                    &BTreeMap::new(),
                    &BTreeSet::new(),
                );
            },
        )
    }

    #[test]
    fn duplicate_preserves_order_with_an_independent_identity_and_unique_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = SequenceStore::new(dir.path().into());
        let mut source = SequenceDoc::new("startup");
        source.push(StepKind::Script, "prepare");
        source.push(StepKind::Layout, "overview");
        source.push(StepKind::Script, "prepare");
        store.save(&source).unwrap();
        let mut manager = SequenceManager::default();
        manager.store = Some(store);
        let copy = manager.duplicate(&source).unwrap();
        let second = manager.duplicate(&source).unwrap();
        assert_ne!(source.id, copy.id);
        assert_ne!(copy.id, second.id);
        assert_eq!(copy.name, "startup copy");
        assert_eq!(second.name, "startup copy 2");
        assert_eq!(copy.steps, source.steps);
        assert_eq!(
            manager.store.as_ref().unwrap().load("startup").unwrap(),
            source
        );
        assert_eq!(
            manager.store.as_ref().unwrap().load(&copy.name).unwrap(),
            copy
        );
    }

    #[test]
    fn removing_an_unselected_sequence_requires_confirmation_and_keeps_selection() {
        let dir = tempfile::tempdir().unwrap();
        let store = SequenceStore::new(dir.path().into());
        let selected = SequenceDoc::new("selected");
        let target = SequenceDoc::new("target");
        store.save(&selected).unwrap();
        store.save(&target).unwrap();
        let mut manager = SequenceManager::default();
        manager.store = Some(store);
        manager.saved_name = Some(selected.name.clone());
        manager.draft = Some(selected.clone());
        manager.library_action(components::LibraryEvent {
            name: target.name.clone(),
            action: components::LibraryAction::Remove,
        });
        assert!(manager.store.as_ref().unwrap().load(&target.name).is_ok());
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let mut render = |events| {
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    manager.delete_confirm(ui.ctx(), &BTreeMap::new(), &mut actions);
                },
            )
        };
        render(vec![]);
        let output = render(vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    (text.galley.text() == "Delete").then(|| text.pos + text.galley.size() * 0.5)
                } else {
                    None
                }
            })
            .unwrap();
        for pressed in [true, false] {
            render(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(manager.store.as_ref().unwrap().load(&target.name).is_err());
        assert_eq!(manager.draft, Some(selected));
        assert!(matches!(&actions[..], [ManagerAction::Delete(id)] if id == &target.id));
        assert!(manager.delete_pending.is_none());
    }

    #[test]
    fn dragging_a_step_handle_changes_execution_order() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut manager = SequenceManager::default();
        manager.open = true;
        let mut doc = SequenceDoc::new("drag-test");
        doc.push(StepKind::Script, "first");
        doc.push(StepKind::Layout, "second");
        manager.draft = Some(doc);
        frame(&ctx, &mut manager, vec![]);
        frame(&ctx, &mut manager, vec![]);
        frame(&ctx, &mut manager, vec![]);
        let doc = manager.draft.as_ref().unwrap();
        let handles: Vec<_> = doc
            .steps
            .iter()
            .map(|step| {
                ctx.read_response(egui::Id::new(("sequence-step", &doc.id, step.id)))
                    .unwrap()
                    .rect
                    .center()
            })
            .collect();
        assert_eq!(handles.len(), 2);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &ctx,
            &mut manager,
            vec![
                egui::Event::PointerMoved(handles[1]),
                button(handles[1], true),
            ],
        );
        frame(
            &ctx,
            &mut manager,
            vec![egui::Event::PointerMoved(
                handles[1] + egui::vec2(15.0, 0.0),
            )],
        );
        frame(
            &ctx,
            &mut manager,
            vec![egui::Event::PointerMoved(handles[0])],
        );
        frame(&ctx, &mut manager, vec![button(handles[0], false)]);
        assert_eq!(manager.draft.as_ref().unwrap().steps[0].reference, "second");
        assert_eq!(manager.draft.as_ref().unwrap().steps[1].reference, "first");
    }
}

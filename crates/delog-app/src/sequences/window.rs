use super::doc::{SequenceDoc, StepKind};
use super::runner::{RunStatus, SequenceRunner};
use super::store::SequenceStore;
use crate::ui::{components, icons};
use std::collections::{BTreeMap, BTreeSet};

const STEP_STATUS_WIDTH: f32 = 72.0;

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
    step_kind: StepKind,
    step_search: String,
    step_reference: Option<String>,
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
            step_kind: StepKind::Script,
            step_search: String::new(),
            step_reference: None,
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
                                            ui.weak(crate::ui::empty::no_saved("sequences"));
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
                                                        let status = run.and_then(|run| {
                                                            run.doc
                                                                .steps
                                                                .iter()
                                                                .position(|s| s.id == step.id)
                                                                .map(|position| {
                                                                    format!(
                                                                        "{:?}",
                                                                        run.states[position]
                                                                    )
                                                                })
                                                        });
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
                                                                ui.allocate_ui_with_layout(
                                                                    egui::vec2(
                                                                        STEP_STATUS_WIDTH,
                                                                        ui.spacing()
                                                                            .interact_size
                                                                            .y,
                                                                    ),
                                                                    egui::Layout::left_to_right(
                                                                        egui::Align::Center,
                                                                    ),
                                                                    |ui| {
                                                                        if let Some(status) = status
                                                                        {
                                                                            ui.weak(status);
                                                                        }
                                                                    },
                                                                );
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
                                    ui.horizontal(|ui| {
                                        egui::ComboBox::from_id_salt("sequence-step-kind")
                                            .width(120.0)
                                            .selected_text(self.step_kind.label())
                                            .show_ui(ui, |ui| {
                                                for kind in StepKind::ALL {
                                                    if ui
                                                        .selectable_value(
                                                            &mut self.step_kind,
                                                            kind,
                                                            kind.label(),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.step_search.clear();
                                                        self.step_reference = None;
                                                    }
                                                }
                                            });
                                        let kind = self.step_kind;
                                        let search_id = egui::Id::new("sequence-step-search");
                                        let picker =
                                            egui::ComboBox::from_id_salt("sequence-step-reference")
                                                .width(220.0)
                                                .close_behavior(
                                                    egui::PopupCloseBehavior::CloseOnClickOutside,
                                                )
                                                .selected_text(match &self.step_reference {
                                                    Some(reference) => reference.clone(),
                                                    None => format!(
                                                        "Select a {}",
                                                        kind.label().to_lowercase()
                                                    ),
                                                })
                                                .show_ui(ui, |ui| {
                                                    ui.add(
                                                        egui::TextEdit::singleline(
                                                            &mut self.step_search,
                                                        )
                                                        .id(search_id)
                                                        .hint_text("Search saved items"),
                                                    );
                                                    let query = self.step_search.to_lowercase();
                                                    let items: Vec<_> = catalog
                                                        .items(kind)
                                                        .iter()
                                                        .filter(|name| {
                                                            name.to_lowercase().contains(&query)
                                                        })
                                                        .collect();
                                                    let plural =
                                                        format!("{}s", kind.label().to_lowercase());
                                                    if catalog.items(kind).is_empty() {
                                                        ui.weak(crate::ui::empty::no_saved(
                                                            &plural,
                                                        ));
                                                    } else if items.is_empty() {
                                                        ui.weak(crate::ui::empty::no_matching(
                                                            &plural,
                                                        ));
                                                    }
                                                    egui::ScrollArea::vertical()
                                                        .max_height(280.0)
                                                        .show(ui, |ui| {
                                                            for name in items {
                                                                if ui.button(name).clicked() {
                                                                    self.step_reference =
                                                                        Some(name.clone());
                                                                    self.step_search.clear();
                                                                    ui.close();
                                                                }
                                                            }
                                                        });
                                                });
                                        if picker.response.clicked() {
                                            ui.ctx().memory_mut(|memory| {
                                                memory.request_focus(search_id)
                                            });
                                        }
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let ready = self.step_reference.is_some();
                                                if ui
                                                    .add_enabled(
                                                        ready,
                                                        egui::Button::new("Add step"),
                                                    )
                                                    .on_disabled_hover_text(
                                                        "Select a saved item first",
                                                    )
                                                    .clicked()
                                                    && let Some(reference) =
                                                        self.step_reference.take()
                                                {
                                                    doc.push(kind, &reference);
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
    use crate::sequences::runner::StepStatus;

    fn frame(
        ctx: &egui::Context,
        manager: &mut SequenceManager,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        frame_with(ctx, manager, &Catalog::default(), events)
    }

    fn frame_with(
        ctx: &egui::Context,
        manager: &mut SequenceManager,
        catalog: &Catalog,
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
                manager.show(ui.ctx(), catalog, &BTreeMap::new(), &BTreeSet::new());
            },
        )
    }

    fn frame_with_runs(
        ctx: &egui::Context,
        manager: &mut SequenceManager,
        runs: &BTreeMap<String, SequenceRunner>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                manager.show(ui.ctx(), &Catalog::default(), runs, &BTreeSet::new());
            },
        )
    }

    fn text_rect(output: &egui::FullOutput, wanted: &str) -> Option<egui::Rect> {
        fn walk(shape: &egui::epaint::Shape, wanted: &str, out: &mut Option<egui::Rect>) {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == wanted => {
                    *out = Some(egui::Rect::from_min_size(text.pos, text.galley.size()));
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|s| walk(s, wanted, out));
                }
                _ => {}
            }
        }
        let mut found = None;
        for clipped in &output.shapes {
            walk(&clipped.shape, wanted, &mut found);
        }
        found
    }

    fn painted(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    fn text_pos(output: &egui::FullOutput, wanted: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::epaint::Shape, wanted: &str, out: &mut Option<egui::Pos2>) {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == wanted => {
                    *out = Some(text.visual_bounding_rect().center());
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|s| walk(s, wanted, out));
                }
                _ => {}
            }
        }
        let mut found = None;
        for clipped in &output.shapes {
            walk(&clipped.shape, wanted, &mut found);
        }
        found
    }

    fn click(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn step_catalog() -> Catalog {
        Catalog {
            parsers: vec!["csv-parser".to_owned()],
            scripts: vec!["derive-speed".to_owned(), "derive-wind".to_owned()],
            dataflows: vec!["fusion".to_owned()],
            layouts: vec!["overview".to_owned(), "overview-wide".to_owned()],
        }
    }

    fn button_disabled(output: &egui::FullOutput, label: &str) -> Option<bool> {
        output
            .platform_output
            .accesskit_update
            .as_ref()?
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.label() == Some(label)
            })
            .map(|(_, node)| node.is_disabled())
    }

    fn open_step_manager(ctx: &egui::Context) -> SequenceManager {
        egui_extras::install_image_loaders(ctx);
        ctx.enable_accesskit();
        SequenceManager {
            open: true,
            draft: Some(SequenceDoc::new("steps")),
            ..Default::default()
        }
    }

    #[test]
    fn the_reference_dropdown_lists_only_the_items_of_the_kind_chosen_in_the_first_dropdown() {
        let ctx = egui::Context::default();
        let mut manager = open_step_manager(&ctx);
        let catalog = step_catalog();
        let render =
            |manager: &mut SequenceManager, events| frame_with(&ctx, manager, &catalog, events);

        render(&mut manager, vec![]);
        let output = render(&mut manager, vec![]);
        assert_eq!(manager.step_kind, StepKind::Script);
        let kind_combo = text_pos(&output, "Script").expect("the kind dropdown should be shown");
        render(&mut manager, click(kind_combo));
        let output = render(&mut manager, vec![]);
        for kind in StepKind::ALL {
            assert!(
                painted(&output).contains(&kind.label().to_owned()),
                "the kind dropdown should offer {}, painted {:?}",
                kind.label(),
                painted(&output)
            );
        }

        let layout = text_pos(&output, "Layout").expect("Layout should be offered");
        render(&mut manager, click(layout));
        let output = render(&mut manager, vec![]);
        assert_eq!(manager.step_kind, StepKind::Layout);

        let picker = text_pos(&output, "Select a layout").expect("the reference dropdown");
        render(&mut manager, click(picker));
        let output = render(&mut manager, vec![]);
        let listed = painted(&output);
        assert!(listed.contains(&"overview".to_owned()));
        assert!(listed.contains(&"overview-wide".to_owned()));
        for other in ["csv-parser", "derive-speed", "derive-wind", "fusion"] {
            assert!(
                !listed.contains(&other.to_owned()),
                "{other} belongs to another kind and must not be offered, painted {listed:?}"
            );
        }

        let item = text_pos(&output, "overview-wide").expect("the layout should be listed");
        render(&mut manager, click(item));
        let output = render(&mut manager, vec![]);
        assert!(
            manager.draft.as_ref().unwrap().steps.is_empty(),
            "picking a reference only arms the button, it must not append a step"
        );
        assert_eq!(button_disabled(&output, "Add step"), Some(false));
        assert!(painted(&output).contains(&"overview-wide".to_owned()));

        let add = text_pos(&output, "Add step").expect("the add button should be shown");
        render(&mut manager, click(add));
        let steps = &manager.draft.as_ref().unwrap().steps;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, StepKind::Layout);
        assert_eq!(steps[0].reference, "overview-wide");
    }

    #[test]
    fn the_add_step_button_stays_disabled_until_a_reference_is_picked() {
        let ctx = egui::Context::default();
        let mut manager = open_step_manager(&ctx);
        let catalog = step_catalog();
        let render =
            |manager: &mut SequenceManager, events| frame_with(&ctx, manager, &catalog, events);

        render(&mut manager, vec![]);
        let output = render(&mut manager, vec![]);
        assert_eq!(button_disabled(&output, "Add step"), Some(true));

        let picker = text_pos(&output, "Select a script").expect("the reference dropdown");
        render(&mut manager, click(picker));
        let output = render(&mut manager, vec![]);
        let item = text_pos(&output, "derive-speed").expect("the script should be listed");
        render(&mut manager, click(item));
        let output = render(&mut manager, vec![]);
        assert_eq!(button_disabled(&output, "Add step"), Some(false));

        let add = text_pos(&output, "Add step").expect("the add button should be shown");
        render(&mut manager, click(add));
        let output = render(&mut manager, vec![]);
        assert_eq!(
            manager.draft.as_ref().unwrap().steps[0].reference,
            "derive-speed"
        );
        assert_eq!(
            button_disabled(&output, "Add step"),
            Some(true),
            "adding the step should clear the pending reference again"
        );
    }

    #[test]
    fn changing_the_kind_drops_a_reference_picked_for_the_previous_kind() {
        let ctx = egui::Context::default();
        let mut manager = open_step_manager(&ctx);
        manager.step_reference = Some("derive-speed".to_owned());
        let catalog = step_catalog();
        let render =
            |manager: &mut SequenceManager, events| frame_with(&ctx, manager, &catalog, events);

        render(&mut manager, vec![]);
        let output = render(&mut manager, vec![]);
        let kind_combo = text_pos(&output, "Script").expect("the kind dropdown should be shown");
        render(&mut manager, click(kind_combo));
        let output = render(&mut manager, vec![]);
        let layout = text_pos(&output, "Layout").expect("Layout should be offered");
        render(&mut manager, click(layout));
        let output = render(&mut manager, vec![]);
        assert_eq!(manager.step_reference, None);
        assert_eq!(button_disabled(&output, "Add step"), Some(true));
        assert!(painted(&output).contains(&"Select a layout".to_owned()));
    }

    #[test]
    fn the_reference_dropdown_reports_a_kind_with_nothing_saved() {
        let ctx = egui::Context::default();
        let mut manager = open_step_manager(&ctx);
        manager.step_kind = StepKind::Dataflow;
        let catalog = Catalog {
            scripts: vec!["derive-speed".to_owned()],
            ..Catalog::default()
        };
        let render =
            |manager: &mut SequenceManager, events| frame_with(&ctx, manager, &catalog, events);

        render(&mut manager, vec![]);
        let output = render(&mut manager, vec![]);
        let picker = text_pos(&output, "Select a dataflow").expect("the reference dropdown");
        render(&mut manager, click(picker));
        let output = render(&mut manager, vec![]);
        assert!(painted(&output).contains(&crate::ui::empty::no_saved("dataflows")));
        assert!(!painted(&output).contains(&"derive-speed".to_owned()));
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

    #[test]
    fn step_status_indicators_line_up_whatever_the_step_reference_is() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut manager = SequenceManager::default();
        manager.open = true;
        let mut doc = SequenceDoc::new("status-column");
        doc.push(StepKind::Script, "a");
        doc.push(StepKind::Layout, "a-much-longer-step-reference");
        doc.push(StepKind::Parser, "mid");
        let mut runner = SequenceRunner::start(doc.clone(), 1);
        runner.states[0] = StepStatus::Failed;
        runner.states[1] = StepStatus::Pending;
        runner.states[2] = StepStatus::Succeeded;
        let mut runs = BTreeMap::new();
        runs.insert(doc.id.clone(), runner);
        manager.draft = Some(doc);

        frame_with_runs(&ctx, &mut manager, &runs);
        frame_with_runs(&ctx, &mut manager, &runs);
        let output = frame_with_runs(&ctx, &mut manager, &runs);

        let failed = text_rect(&output, "Failed").expect("the failed step should read out");
        let pending = text_rect(&output, "Pending").expect("the pending step should read out");
        let succeeded =
            text_rect(&output, "Succeeded").expect("the succeeded step should read out");

        assert_eq!(
            (failed.left(), failed.left()),
            (pending.left(), succeeded.left()),
            "status indicators must share one column"
        );
        assert!(
            succeeded.width() <= STEP_STATUS_WIDTH && succeeded.height() == failed.height(),
            "the widest status must fit its column on one line, got {} x {}",
            succeeded.width(),
            succeeded.height()
        );
    }
}

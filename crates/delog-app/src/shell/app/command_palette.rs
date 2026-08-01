use crate::shell::app::commands::{
    AppCommand, CommandAvailability, CommandId, CommandPresentation,
};

#[derive(Default)]
pub struct CommandPaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    focus_search: bool,
}

#[derive(Clone)]
pub struct PaletteEntry {
    pub command: AppCommand,
    pub label: String,
    pub subtitle: Option<String>,
    pub search_text: String,
    pub availability: CommandAvailability,
    pub selected: Option<bool>,
}

impl PaletteEntry {
    #[cfg(test)]
    pub fn enabled(command: AppCommand, label: &str, search_terms: &str) -> Self {
        Self {
            command,
            label: label.to_owned(),
            subtitle: None,
            search_text: format!("{label} {search_terms}"),
            availability: CommandAvailability::Enabled,
            selected: None,
        }
    }

    pub fn from_presentation(presentation: CommandPresentation, search_terms: &str) -> Self {
        let (label, selected) = match &presentation.command {
            AppCommand::Static(CommandId::ToggleDataBrowser) => {
                ("Toggle Data Browser".to_owned(), None)
            }
            AppCommand::Static(CommandId::ToggleInspector) => ("Toggle Inspector".to_owned(), None),
            AppCommand::Static(CommandId::ToggleScene3d) => ("Toggle 3D Scene".to_owned(), None),
            AppCommand::Static(CommandId::ToggleLegends) => ("Toggle Legends".to_owned(), None),
            _ => (presentation.label, presentation.selected),
        };
        Self {
            command: presentation.command,
            search_text: format!("{label} {search_terms}"),
            label,
            subtitle: presentation.shortcut.map(str::to_owned),
            availability: presentation.availability,
            selected,
        }
    }
}

impl CommandPaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus_search = true;
    }

    pub fn entries(
        presentations: impl IntoIterator<Item = CommandPresentation>,
    ) -> Vec<PaletteEntry> {
        presentations
            .into_iter()
            .map(|presentation| PaletteEntry::from_presentation(presentation, ""))
            .collect()
    }

    pub fn handle_key(
        &mut self,
        ctx: &egui::Context,
        entries: &[PaletteEntry],
    ) -> Option<AppCommand> {
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return None;
        }

        let ranked = ranked_entries(&self.query, entries);
        if ranked.is_empty() {
            self.selected = 0;
            return None;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
            self.selected = (self.selected + 1).min(ranked.len() - 1);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(ranked.len() - 1);
        if ctx.input(|input| input.key_pressed(egui::Key::Enter))
            && ranked[self.selected].availability == CommandAvailability::Enabled
        {
            self.open = false;
            return Some(ranked[self.selected].command.clone());
        }
        None
    }

    pub fn show(&mut self, ctx: &egui::Context, entries: &[PaletteEntry]) -> Option<AppCommand> {
        if !self.open {
            return None;
        }
        let mut selected_command = self.handle_key(ctx, entries);
        let ranked = ranked_entries(&self.query, entries);
        let screen = ctx.content_rect();
        egui::Window::new("Command palette")
            .id(egui::Id::new("command-palette"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(egui::vec2(
                (screen.width() * 0.42).clamp(420.0, 680.0),
                420.0,
            ))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
            .show(ctx, |ui| {
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search commands…")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_search {
                    search.request_focus();
                    self.focus_search = false;
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if ranked.is_empty() {
                        ui.weak("No matching commands");
                    }
                    for (index, entry) in ranked.iter().enumerate() {
                        let enabled = entry.availability == CommandAvailability::Enabled;
                        let mut label = entry.label.clone();
                        if entry.selected == Some(true) {
                            label.insert_str(0, "✓  ");
                        }
                        let subtitle = match &entry.command {
                            AppCommand::Static(_) => {
                                if let Some(shortcut) = &entry.subtitle {
                                    label.push_str(&format!("    {shortcut}"));
                                }
                                None
                            }
                            _ => entry.subtitle.as_deref(),
                        };
                        let label = palette_row_text(ui, &label, subtitle);
                        let response = ui.add_enabled(
                            enabled,
                            egui::Button::new(label)
                                .selected(index == self.selected)
                                .min_size(egui::vec2(
                                    ui.available_width(),
                                    if subtitle.is_some() { 42.0 } else { 30.0 },
                                )),
                        );
                        let response = match &entry.availability {
                            CommandAvailability::Disabled(reason) => {
                                response.on_disabled_hover_text(*reason)
                            }
                            CommandAvailability::Enabled => response,
                        };
                        if response.hovered() {
                            self.selected = index;
                        }
                        if response.clicked() {
                            selected_command = Some(entry.command.clone());
                            self.open = false;
                        }
                    }
                });
            });
        selected_command
    }
}

fn palette_row_text(ui: &egui::Ui, label: &str, subtitle: Option<&str>) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat {
            font_id: egui::TextStyle::Button.resolve(ui.style()),
            color: ui.visuals().text_color(),
            ..Default::default()
        },
    );
    if let Some(subtitle) = subtitle {
        job.append("\n", 0.0, egui::TextFormat::default());
        job.append(
            subtitle,
            0.0,
            egui::TextFormat {
                font_id: egui::TextStyle::Small.resolve(ui.style()),
                color: ui.visuals().weak_text_color(),
                ..Default::default()
            },
        );
    }
    job
}

pub fn should_toggle_palette(ctrl_k: bool, wants_keyboard_input: bool) -> bool {
    ctrl_k && !wants_keyboard_input
}

pub fn ranked_entries<'a>(query: &str, entries: &'a [PaletteEntry]) -> Vec<&'a PaletteEntry> {
    if query.trim().is_empty() {
        return entries.iter().collect();
    }
    let mut ranked: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            crate::ui::fuzzy::fuzzy_match_score(query, &entry.search_text)
                .map(|score| (score, entry))
        })
        .collect();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        a_score.cmp(b_score).then_with(|| a.label.cmp(&b.label))
    });
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::app::commands::{AppCommand, CommandId};

    fn find_text_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_text_rect(shape, expected)),
            _ => None,
        }
    }

    fn palette_frame(
        ctx: &egui::Context,
        palette: &mut CommandPaletteState,
        entries: &[PaletteEntry],
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<AppCommand>) {
        let mut selected = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ui| selected = palette.show(ui.ctx(), entries),
        );
        (output, selected)
    }

    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn palette_ranks_exact_command_before_loose_match() {
        let entries = vec![
            PaletteEntry::enabled(
                AppCommand::Static(CommandId::Open),
                "Open log",
                "source file",
            ),
            PaletteEntry::enabled(
                AppCommand::Static(CommandId::OpenLogging),
                "Open logging",
                "panel logs",
            ),
        ];
        let ranked = ranked_entries("open log", &entries);
        assert_eq!(ranked[0].command, AppCommand::Static(CommandId::Open));
    }

    #[test]
    fn ctrl_k_is_ignored_when_an_editor_owns_text_input() {
        assert!(!should_toggle_palette(true, true));
        assert!(should_toggle_palette(true, false));
    }

    #[test]
    fn disabled_entries_cannot_be_dispatched_with_enter() {
        let mut palette = CommandPaletteState {
            open: true,
            ..Default::default()
        };
        let entries = vec![PaletteEntry {
            command: AppCommand::Static(CommandId::SyncSources),
            label: "Sync sources".to_owned(),
            subtitle: None,
            search_text: "Sync sources".to_owned(),
            availability: CommandAvailability::Disabled("Open two sources"),
            selected: None,
        }];
        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        });
        assert_eq!(palette.handle_key(&context, &entries), None);
        let _ = context.end_pass();
    }

    #[test]
    fn rendered_palette_click_dispatches_enabled_entry_but_not_disabled_fit_all() {
        let ctx = egui::Context::default();
        let enabled = PaletteEntry::enabled(
            AppCommand::Static(CommandId::Open),
            "Open test log",
            "file",
        );
        let disabled = PaletteEntry {
            command: AppCommand::FitAll,
            label: "Fit all plots".to_owned(),
            subtitle: None,
            search_text: "Fit all plots".to_owned(),
            availability: CommandAvailability::Disabled(
                "Open a log or connect a live source first",
            ),
            selected: None,
        };

        for (entry, expected) in [
            (enabled, Some(AppCommand::Static(CommandId::Open))),
            (disabled, None),
        ] {
            let mut palette = CommandPaletteState::default();
            palette.open();
            let entries = [entry];
            let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);
            let (output, _) = palette_frame(&ctx, &mut palette, &entries, vec![]);
            let rect = output
                .shapes
                .iter()
                .find_map(|shape| find_text_rect(&shape.shape, &entries[0].label))
                .expect("palette row should be painted");
            let pos = rect.center();
            let _ = palette_frame(
                &ctx,
                &mut palette,
                &entries,
                vec![egui::Event::PointerMoved(pos), pointer_button(pos, true)],
            );
            let (_, selected) = palette_frame(
                &ctx,
                &mut palette,
                &entries,
                vec![egui::Event::PointerMoved(pos), pointer_button(pos, false)],
            );

            assert_eq!(selected, expected);
        }
    }

    #[test]
    fn palette_entries_preserve_the_catalog_commands_and_selected_state() {
        let presentations = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext::default(),
            &crate::shell::app::commands::PresentationState {
                shell_emphasis_live: true,
                cursor_sampling: delog_core::field_view::SampleMode::Linear,
                ..crate::shell::app::commands::PresentationState::default()
            },
            [],
        );
        let expected: Vec<_> = presentations
            .iter()
            .map(|presentation| presentation.command.clone())
            .collect();
        let entries = CommandPaletteState::entries(presentations);

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.command.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(entries.iter().any(|entry| {
            entry.command
                == AppCommand::SetCursorSampling(
                    delog_core::field_view::SampleMode::Linear,
                )
                && entry.selected == Some(true)
        }));
    }

    #[test]
    fn palette_uses_plain_toggle_actions_for_view_toggles() {
        let presentations = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext::default(),
            &crate::shell::app::commands::PresentationState {
                data_browser_open: true,
                inspector_open: true,
                scene_3d_open: true,
                legends_visible: true,
                ..crate::shell::app::commands::PresentationState::default()
            },
            [],
        );
        let entries = CommandPaletteState::entries(presentations);

        for (command, label) in [
            (CommandId::ToggleDataBrowser, "Toggle Data Browser"),
            (CommandId::ToggleInspector, "Toggle Inspector"),
            (CommandId::ToggleScene3d, "Toggle 3D Scene"),
            (CommandId::ToggleLegends, "Toggle Legends"),
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry.command == AppCommand::Static(command))
                .expect("toggle command should be present in the palette");
            assert_eq!(entry.label, label);
            assert_eq!(entry.selected, None);
        }
    }

    #[test]
    fn palette_renders_family_context_on_a_secondary_line() {
        let ctx = egui::Context::default();
        let mut palette = CommandPaletteState::default();
        palette.open();
        let entries = [PaletteEntry {
            command: AppCommand::RunScript("shared".into()),
            label: "shared".to_owned(),
            subtitle: Some("Tools › Scripts › Run Scripts".to_owned()),
            search_text: "shared script run execute".to_owned(),
            availability: CommandAvailability::Enabled,
            selected: None,
        }];

        let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);
        let (output, _) = palette_frame(&ctx, &mut palette, &entries, vec![]);

        assert!(output.shapes.iter().any(|shape| {
            find_text_rect(
                &shape.shape,
                "shared\nTools › Scripts › Run Scripts",
            )
            .is_some()
        }));
    }
}

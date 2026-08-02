use crate::shell::app::commands::{
    AppCommand, CommandAvailability, CommandId, CommandPresentation,
};

#[derive(Default)]
pub struct CommandPaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    focus_search: bool,
    scroll_to_selected: bool,
    hover: HoverGate,
}

#[derive(Default, PartialEq)]
enum HoverGate {
    #[default]
    JustOpened,
    Waiting(Option<egui::Pos2>),
    Armed,
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
        self.scroll_to_selected = true;
        self.hover = HoverGate::JustOpened;
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
            self.scroll_to_selected = false;
            return None;
        }
        let selected_before_key = self.selected;
        if ctx.input(|input| {
            input.key_pressed(egui::Key::ArrowDown)
                || (input.modifiers.ctrl && input.key_pressed(egui::Key::N))
        }) {
            self.selected = (self.selected + 1).min(ranked.len() - 1);
        }
        if ctx.input(|input| {
            input.key_pressed(egui::Key::ArrowUp)
                || (input.modifiers.ctrl && input.key_pressed(egui::Key::P))
        }) {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(ranked.len() - 1);
        self.scroll_to_selected |= self.selected != selected_before_key;
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
        let scroll_to_selected = std::mem::take(&mut self.scroll_to_selected);
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
                let pointer = ui.ctx().pointer_latest_pos();
                self.hover = match self.hover {
                    HoverGate::JustOpened => HoverGate::Waiting(pointer),
                    HoverGate::Waiting(anchor) => {
                        let moved = match (anchor, pointer) {
                            (Some(from), Some(to)) => from.distance(to) > 2.0,
                            (from, to) => from.is_some() != to.is_some(),
                        };
                        if moved {
                            HoverGate::Armed
                        } else {
                            HoverGate::Waiting(anchor)
                        }
                    }
                    HoverGate::Armed => HoverGate::Armed,
                };
                let hover_armed = self.hover == HoverGate::Armed;
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
                            CommandAvailability::Disabled(reason) if hover_armed => {
                                response.on_disabled_hover_text(*reason)
                            }
                            _ => response,
                        };
                        if scroll_to_selected && index == self.selected {
                            response.scroll_to_me(None);
                        }
                        if hover_armed && response.hovered() {
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
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                modifiers,
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

    fn ctrl_key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }
    }

    fn painted_inside_clip(output: &egui::FullOutput, expected: &str) -> bool {
        output.shapes.iter().any(|clipped| {
            find_text_rect(&clipped.shape, expected)
                .is_some_and(|rect| clipped.clip_rect.contains(rect.center()))
        })
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
    fn ctrl_n_and_ctrl_p_move_the_palette_selection() {
        let entries = vec![
            PaletteEntry::enabled(AppCommand::Static(CommandId::Open), "First", ""),
            PaletteEntry::enabled(AppCommand::Static(CommandId::OpenLogging), "Second", ""),
            PaletteEntry::enabled(AppCommand::Static(CommandId::OpenSettings), "Third", ""),
        ];
        let context = egui::Context::default();
        let mut palette = CommandPaletteState {
            open: true,
            selected: 1,
            ..Default::default()
        };

        for (key, expected) in [(egui::Key::N, 2), (egui::Key::P, 1)] {
            context.begin_pass(egui::RawInput {
                modifiers: egui::Modifiers::CTRL,
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                }],
                ..Default::default()
            });
            assert_eq!(palette.handle_key(&context, &entries), None);
            assert_eq!(palette.selected, expected);
            let _ = context.end_pass();
        }
    }

    #[test]
    fn ctrl_n_scrolls_the_selected_palette_row_into_view() {
        let entries = (0..30)
            .map(|index| {
                PaletteEntry::enabled(
                    AppCommand::Static(CommandId::Open),
                    &format!("Command {index:02}"),
                    "",
                )
            })
            .collect::<Vec<_>>();
        let context = egui::Context::default();
        let mut palette = CommandPaletteState::default();
        palette.open();
        let _ = palette_frame(&context, &mut palette, &entries, vec![]);

        let mut output = None;
        for _ in 0..20 {
            let (frame, _) = palette_frame(
                &context,
                &mut palette,
                &entries,
                vec![ctrl_key(egui::Key::N)],
            );
            output = Some(frame);
        }
        for _ in 0..8 {
            let (frame, _) = palette_frame(&context, &mut palette, &entries, vec![]);
            output = Some(frame);
        }

        assert_eq!(palette.selected, 20);
        assert!(painted_inside_clip(&output.unwrap(), "Command 20"));
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
    fn opening_under_the_pointer_keeps_the_first_entry_selected() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let entries = [
            PaletteEntry::enabled(AppCommand::Static(CommandId::Open), "First", ""),
            PaletteEntry::enabled(AppCommand::Static(CommandId::ConnectLive), "Second", ""),
            PaletteEntry::enabled(AppCommand::Static(CommandId::OpenSettings), "Third", ""),
        ];

        let mut palette = CommandPaletteState::default();
        palette.open();
        let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);
        let (output, _) = palette_frame(&ctx, &mut palette, &entries, vec![]);
        let third = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Third"))
            .expect("the third row should be painted")
            .center();

        // Park the pointer over the third row, then reopen underneath it.
        let _ = palette_frame(
            &ctx,
            &mut palette,
            &entries,
            vec![egui::Event::PointerMoved(third)],
        );
        palette.open();
        let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);
        let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);

        assert_eq!(
            palette.selected, 0,
            "a palette opened under the pointer should still start on the first entry"
        );
    }

    #[test]
    fn moving_the_pointer_after_opening_selects_the_hovered_entry() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let entries = [
            PaletteEntry::enabled(AppCommand::Static(CommandId::Open), "First", ""),
            PaletteEntry::enabled(AppCommand::Static(CommandId::ConnectLive), "Second", ""),
            PaletteEntry::enabled(AppCommand::Static(CommandId::OpenSettings), "Third", ""),
        ];

        let mut palette = CommandPaletteState::default();
        palette.open();
        let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);
        let (output, _) = palette_frame(&ctx, &mut palette, &entries, vec![]);
        let third = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Third"))
            .expect("the third row should be painted")
            .center();

        let _ = palette_frame(
            &ctx,
            &mut palette,
            &entries,
            vec![egui::Event::PointerMoved(third)],
        );
        let _ = palette_frame(&ctx, &mut palette, &entries, vec![]);

        assert_eq!(
            palette.selected, 2,
            "deliberately moving onto a row should select it"
        );
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

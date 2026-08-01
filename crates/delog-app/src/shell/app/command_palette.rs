use crate::shell::app::commands::{AppCommand, CommandAvailability, CommandPresentation};

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
        }
    }

    pub fn from_presentation(presentation: CommandPresentation, search_terms: &str) -> Self {
        Self {
            command: presentation.command,
            search_text: format!("{} {search_terms}", presentation.label),
            label: presentation.label,
            subtitle: presentation.shortcut.map(str::to_owned),
            availability: presentation.availability,
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
                        if let Some(shortcut) = &entry.subtitle {
                            label.push_str(&format!("    {shortcut}"));
                        }
                        let response = ui.add_enabled(
                            enabled,
                            egui::Button::new(label)
                                .selected(index == self.selected)
                                .min_size(egui::vec2(ui.available_width(), 30.0)),
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
}

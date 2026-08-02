#[derive(Clone)]
pub struct PickerItem<T> {
    pub key: T,
    pub label: String,
    pub subtitle: Option<String>,
    pub search_text: String,
    pub disabled_reason: Option<&'static str>,
    pub checked: bool,
}

impl<T> PickerItem<T> {
    pub fn new(key: T, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            key,
            search_text: label.clone(),
            label,
            subtitle: None,
            disabled_reason: None,
            checked: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }
}

#[derive(Default, PartialEq)]
enum HoverGate {
    #[default]
    JustOpened,
    Waiting(Option<egui::Pos2>),
    Armed,
}

#[derive(Default)]
pub struct PickerState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    focus_search: bool,
    scroll_to_selected: bool,
    hover: HoverGate,
}

impl PickerState {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus_search = true;
        self.scroll_to_selected = true;
        self.hover = HoverGate::JustOpened;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn handle_key<T: Clone>(
        &mut self,
        ctx: &egui::Context,
        items: &[PickerItem<T>],
    ) -> Option<T> {
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return None;
        }

        let ranked = ranked_items(&self.query, items);
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
        if ctx.input(|input| input.key_pressed(egui::Key::Enter)) && ranked[self.selected].is_enabled()
        {
            self.open = false;
            return Some(ranked[self.selected].key.clone());
        }
        None
    }

    pub fn show<T: Clone>(
        &mut self,
        ctx: &egui::Context,
        id: &'static str,
        hint: &str,
        empty_text: &str,
        items: &[PickerItem<T>],
    ) -> Option<T> {
        if !self.open {
            return None;
        }
        let mut picked = self.handle_key(ctx, items);
        let ranked = ranked_items(&self.query, items);
        let scroll_to_selected = std::mem::take(&mut self.scroll_to_selected);
        let screen = ctx.content_rect();
        egui::Window::new(id)
            .id(egui::Id::new(id))
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
                        .hint_text(hint)
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
                        ui.weak(empty_text);
                    }
                    for (index, item) in ranked.iter().enumerate() {
                        let mut label = item.label.clone();
                        if item.checked {
                            label.insert_str(0, "✓  ");
                        }
                        let subtitle = item.subtitle.as_deref();
                        let text = picker_row_text(ui, &label, subtitle);
                        let response = ui.add_enabled(
                            item.is_enabled(),
                            egui::Button::new(text)
                                .selected(index == self.selected)
                                .min_size(egui::vec2(
                                    ui.available_width(),
                                    if subtitle.is_some() { 42.0 } else { 30.0 },
                                )),
                        );
                        let response = match item.disabled_reason {
                            Some(reason) if hover_armed => response.on_disabled_hover_text(reason),
                            _ => response,
                        };
                        if scroll_to_selected && index == self.selected {
                            response.scroll_to_me(None);
                        }
                        if hover_armed && response.hovered() {
                            self.selected = index;
                        }
                        if response.clicked() {
                            picked = Some(item.key.clone());
                            self.open = false;
                        }
                    }
                });
            });
        picked
    }
}

fn picker_row_text(ui: &egui::Ui, label: &str, subtitle: Option<&str>) -> egui::text::LayoutJob {
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

pub fn ranked_items<'a, T>(query: &str, items: &'a [PickerItem<T>]) -> Vec<&'a PickerItem<T>> {
    if query.trim().is_empty() {
        return items.iter().collect();
    }
    let mut ranked: Vec<_> = items
        .iter()
        .filter_map(|item| {
            crate::ui::fuzzy::fuzzy_match_score(query, &item.search_text).map(|score| (score, item))
        })
        .collect();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        a_score.cmp(b_score).then_with(|| a.label.cmp(&b.label))
    });
    ranked.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layouts() -> Vec<PickerItem<String>> {
        ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|name| PickerItem::new(name.to_owned(), name))
            .collect()
    }

    fn press(
        ctx: &egui::Context,
        state: &mut PickerState,
        items: &[PickerItem<String>],
        key: egui::Key,
        ctrl: bool,
    ) -> Option<String> {
        let modifiers = if ctrl {
            egui::Modifiers::CTRL
        } else {
            egui::Modifiers::NONE
        };
        ctx.begin_pass(egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        });
        let picked = state.handle_key(ctx, items);
        let _ = ctx.end_pass();
        picked
    }

    #[test]
    fn escape_closes_the_picker_without_choosing() {
        let ctx = egui::Context::default();
        let items = layouts();
        let mut state = PickerState::default();
        state.open();

        let picked = press(&ctx, &mut state, &items, egui::Key::Escape, false);

        assert!(picked.is_none());
        assert!(!state.open, "escape should close the picker");
    }

    #[test]
    fn ctrl_n_and_ctrl_p_walk_the_list() {
        let ctx = egui::Context::default();
        let items = layouts();
        let mut state = PickerState::default();
        state.open();

        press(&ctx, &mut state, &items, egui::Key::N, true);
        assert_eq!(state.selected, 1);
        press(&ctx, &mut state, &items, egui::Key::N, true);
        assert_eq!(state.selected, 2);
        press(&ctx, &mut state, &items, egui::Key::N, true);
        assert_eq!(state.selected, 2, "selection should stop at the last item");
        press(&ctx, &mut state, &items, egui::Key::P, true);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn enter_picks_the_selected_item_and_closes() {
        let ctx = egui::Context::default();
        let items = layouts();
        let mut state = PickerState::default();
        state.open();

        press(&ctx, &mut state, &items, egui::Key::N, true);
        let picked = press(&ctx, &mut state, &items, egui::Key::Enter, false);

        assert_eq!(picked.as_deref(), Some("beta"));
        assert!(!state.open, "choosing should close the picker");
    }

    #[test]
    fn enter_does_not_pick_a_disabled_item() {
        let ctx = egui::Context::default();
        let mut items = layouts();
        items[0].disabled_reason = Some("nope");
        let mut state = PickerState::default();
        state.open();

        let picked = press(&ctx, &mut state, &items, egui::Key::Enter, false);

        assert!(picked.is_none());
        assert!(state.open, "a disabled item should not close the picker");
    }

    #[test]
    fn the_query_filters_and_ranks_items() {
        let items = layouts();
        let ranked = ranked_items("gam", &items);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].label, "gamma");
    }
}

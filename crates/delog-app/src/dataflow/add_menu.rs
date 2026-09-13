use super::picker::{DataHit, search_fields};
use super::registry::{ADD_DATA_INDEX, search_templates, templates};
use delog_core::snapshot::StoreSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AddMenuMode {
    Templates,
    Data,
}

pub(super) struct AddMenuState {
    pub(super) screen_pos: egui::Pos2,
    pub(super) canvas_pos: [f32; 2],
    pub(super) query: String,
    pub(super) mode: AddMenuMode,
    pub(super) highlighted: usize,
    pub(super) focus_requested: bool,
    pub(super) dismiss_armed: bool,
}

impl AddMenuState {
    pub(super) fn new(screen_pos: egui::Pos2, canvas_pos: [f32; 2]) -> Self {
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

pub(super) enum AddAction {
    Template(usize),
    Data(DataHit),
    Close,
}

pub(super) fn show_add_menu(
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

#[cfg(test)]
mod tests {
    use super::*;
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
}

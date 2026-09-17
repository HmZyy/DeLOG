use super::picker::{DataHit, search_fields, topic_display};
use super::registry::{ADD_DATA_INDEX, search_templates, templates};
use delog_core::snapshot::StoreSnapshot;

const MENU_WIDTH: f32 = 520.0;
const ROW_HEIGHT: f32 = 24.0;

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
                    ui.set_width(MENU_WIDTH);
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
                                let columns = data_columns(ui, &data_hits);
                                for (row, hit) in data_hits.iter().enumerate() {
                                    if data_row(ui, state.highlighted == row, hit, &columns) {
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

const SEPARATOR: &str = "\u{203a}";

struct DataColumns {
    widths: [f32; 4],
    separator: f32,
    height: f32,
}

fn data_cells(hit: &DataHit) -> [String; 4] {
    [
        hit.selector.source.clone().unwrap_or_default(),
        topic_display(&hit.selector),
        hit.selector.field.clone(),
        hit.unit.clone().unwrap_or_default(),
    ]
}

fn data_columns(ui: &egui::Ui, hits: &[DataHit]) -> DataColumns {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let measure = |text: &str| {
        ui.ctx().fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
        })
    };
    let mut columns = DataColumns {
        widths: [0.0; 4],
        separator: measure(SEPARATOR).x,
        height: measure("Ag").y,
    };
    for hit in hits {
        for (width, cell) in columns.widths.iter_mut().zip(data_cells(hit)) {
            *width = width.max(measure(&cell).x);
        }
    }
    columns
}

fn data_row(ui: &mut egui::Ui, highlighted: bool, hit: &DataHit, columns: &DataColumns) -> bool {
    use egui::AtomExt as _;
    let cell = |text: egui::RichText, width: f32| {
        text.atom_size(egui::vec2(width, columns.height))
            .atom_align(egui::Align2::LEFT_CENTER)
    };
    let mut atoms: Vec<egui::Atom<'static>> = Vec::new();
    for (column, (text, width)) in data_cells(hit).into_iter().zip(columns.widths).enumerate() {
        if column == 1 || column == 2 {
            atoms.push(cell(
                egui::RichText::new(SEPARATOR).weak(),
                columns.separator,
            ));
        }
        let text = egui::RichText::new(text);
        let text = if column == 3 { text.weak() } else { text };
        atoms.push(cell(text, width));
    }
    atoms.push(egui::Atom::grow());
    atoms.push(
        egui::RichText::new(format!("{} rows", hit.rows))
            .weak()
            .into(),
    );
    ui.add(
        egui::Button::new(egui::Atoms::from(atoms))
            .selected(highlighted)
            .min_size(egui::vec2(ui.available_width(), ROW_HEIGHT)),
    )
    .clicked()
}

fn menu_row(ui: &mut egui::Ui, highlighted: bool, label: &str) -> bool {
    ui.add(
        egui::Button::new(label)
            .selected(highlighted)
            .min_size(egui::vec2(ui.available_width(), ROW_HEIGHT)),
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

    fn text_layout_rects(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(text) => out.push((
                    text.galley.job.text.clone(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
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

    fn painted(ctx: &egui::Context, menu: &mut AddMenuState) -> Vec<(String, egui::Rect)> {
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_600.0, 900.0),
            )),
            ..Default::default()
        };
        let snapshot = crate::dataflow::picker::snapshot_two_sources();
        let _ = ctx.run_ui(input(), |_ui| {
            let _ = show_add_menu(ctx, menu, &snapshot);
        });
        let output = ctx.run_ui(input(), |_ui| {
            let _ = show_add_menu(ctx, menu, &snapshot);
        });
        text_layout_rects(&output)
    }

    #[test]
    fn node_rows_share_one_left_aligned_column() {
        let ctx = egui::Context::default();
        let mut menu = AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]);

        let rects = painted(&ctx, &mut menu);
        let names: Vec<_> = templates().iter().map(|t| t.name).collect();
        let lefts: Vec<_> = rects
            .iter()
            .filter(|(text, _)| text == "Add Data..." || names.contains(&text.as_str()))
            .map(|(text, rect)| (text.clone(), rect.left()))
            .collect();

        assert!(lefts.len() > 2, "the menu should paint its node rows");
        let first = lefts[0].1;
        assert!(
            lefts.iter().all(|(_, left)| *left == first),
            "every node row must start at the same x, got {lefts:?}"
        );
    }

    #[test]
    fn add_data_rows_lay_out_as_columns_with_the_row_count_last() {
        let ctx = egui::Context::default();
        let mut menu = AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]);
        menu.mode = AddMenuMode::Data;

        let rects = painted(&ctx, &mut menu);
        let column = |members: &[&str]| -> Vec<(String, egui::Rect)> {
            rects
                .iter()
                .filter(|(text, _)| members.contains(&text.as_str()))
                .cloned()
                .collect()
        };
        let one_left = |cells: &[(String, egui::Rect)], what: &str| {
            assert!(cells.len() > 1, "{what} should paint on several rows");
            let first = cells[0].1.left();
            assert!(
                cells.iter().all(|(_, rect)| rect.left() == first),
                "every {what} must start at the same x, got {cells:?}"
            );
            first
        };

        let sources = column(&["flight_01", "flight_02"]);
        let topics = column(&["IMU[0]", "IMU[1]", "GPS"]);
        let fields = column(&["AccX", "Alt"]);
        let source_left = one_left(&sources, "source");
        let topic_left = one_left(&topics, "topic");
        let field_left = one_left(&fields, "field");

        assert!(
            source_left < topic_left && topic_left < field_left,
            "the columns must run source, topic, field"
        );

        let separators: Vec<_> = rects
            .iter()
            .filter(|(text, _)| text == "\u{203a}")
            .cloned()
            .collect();
        assert_eq!(
            separators.len(),
            sources.len() * 2,
            "each row keeps a separator between source, topic and field"
        );
        let after_source: Vec<_> = separators
            .iter()
            .filter(|(_, rect)| rect.left() < topic_left)
            .collect();
        assert_eq!(after_source.len(), sources.len());
        let first = after_source[0].1.left();
        assert!(
            after_source.iter().all(|(_, rect)| rect.left() == first),
            "the separators must line up too, got {after_source:?}"
        );

        let counts: Vec<_> = rects
            .iter()
            .filter(|(text, _)| text.ends_with(" rows"))
            .cloned()
            .collect();
        assert!(counts.len() > 1, "each row should carry its row count");
        let right = counts[0].1.right();
        assert!(
            counts.iter().all(|(_, rect)| rect.right() == right),
            "row counts must share one right edge, got {counts:?}"
        );
        let widest = rects
            .iter()
            .map(|(_, rect)| rect.right())
            .fold(f32::MIN, f32::max);
        assert_eq!(
            right, widest,
            "the row count must sit at the very right of its row"
        );
    }

    #[test]
    fn the_menu_keeps_its_own_width_whatever_the_screen_is() {
        let width = |screen: f32, mode: AddMenuMode| {
            let ctx = egui::Context::default();
            let mut menu = AddMenuState::new(egui::pos2(20.0, 30.0), [4.0, 5.0]);
            menu.mode = mode;
            let input = || egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(screen, 900.0),
                )),
                ..Default::default()
            };
            let snapshot = crate::dataflow::picker::snapshot_two_sources();
            for _ in 0..2 {
                let _ = ctx.run_ui(input(), |_ui| {
                    let _ = show_add_menu(&ctx, &mut menu, &snapshot);
                });
            }
            ctx.memory(|memory| memory.area_rect(egui::Id::new("dataflow-add-menu")))
                .expect("Add menu area should exist")
                .width()
        };

        let narrow = width(900.0, AddMenuMode::Templates);
        assert_eq!(narrow, width(2_400.0, AddMenuMode::Templates));
        assert_eq!(narrow, width(1_600.0, AddMenuMode::Data));
        assert!(
            (MENU_WIDTH..MENU_WIDTH + 32.0).contains(&narrow),
            "the menu should size itself from MENU_WIDTH, got {narrow}"
        );
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

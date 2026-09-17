use crate::ui::design_tokens::DesignTokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Neutral,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChip {
    pub label: String,
    pub detail: Option<String>,
    pub state: StatusState,
}

impl StatusChip {
    #[cfg(test)]
    pub fn connected(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: Some(detail.into()),
            state: StatusState::Success,
        }
    }

    pub fn text(&self) -> String {
        match self.detail.as_deref() {
            Some(detail) => format!("{} · {detail}", self.label),
            None => self.label.clone(),
        }
    }
}

pub fn icon_button(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let tokens = DesignTokens::from_style(ui.style());
    icon_button_sized(
        ui,
        icon,
        tooltip,
        selected,
        egui::Vec2::splat(tokens.control_height),
        egui::Vec2::splat(tokens.icon_size),
    )
}

pub fn icon_button_sized(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    tooltip: &str,
    selected: bool,
    button_size: egui::Vec2,
    icon_size: egui::Vec2,
) -> egui::Response {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(icon_size)
        .tint(ui.visuals().text_color())
        .alt_text(tooltip);
    let response = ui.add_sized(button_size, egui::Button::image(image).selected(selected));
    let enabled = response.enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, tooltip)
    });
    response.on_hover_text(tooltip)
}

pub fn icon_text_button(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    label: &str,
    selected: bool,
) -> egui::Response {
    let tokens = DesignTokens::from_style(ui.style());
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::Vec2::splat(tokens.icon_size))
        .tint(ui.visuals().text_color());
    ui.add_sized(
        [0.0, tokens.control_height],
        egui::Button::image_and_text(image, label).selected(selected),
    )
}

pub fn status_chip(
    ui: &mut egui::Ui,
    chip: &StatusChip,
    theme: crate::ui::theme::ThemeChoice,
) -> egui::Response {
    let color = match chip.state {
        StatusState::Neutral => theme.neutral(),
        StatusState::Success => theme.success(),
        StatusState::Warning => theme.warning(),
        StatusState::Error => theme.error(),
    };
    ui.add(
        egui::Button::new(egui::RichText::new(chip.text()).color(color))
            .sense(egui::Sense::hover()),
    )
}

pub fn panel_header(ui: &mut egui::Ui, title: &str) -> egui::Response {
    ui.add(egui::Label::new(egui::RichText::new(title).strong()))
}

pub fn menu_row(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
    disabled_reason: Option<&str>,
) -> egui::Response {
    let mut button = egui::Button::new(label).wrap_mode(egui::TextWrapMode::Extend);
    if let Some(key) = shortcut {
        button = button.shortcut_text(key);
    }
    let response = ui.add_enabled(enabled, button);
    match disabled_reason {
        Some(reason) if !enabled => response.on_disabled_hover_text(reason),
        _ => response,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryAction {
    Load,
    Edit,
    Duplicate,
    Remove,
}

impl LibraryAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Load => "Load",
            Self::Edit => "Edit",
            Self::Duplicate => "Duplicate",
            Self::Remove => "Remove",
        }
    }

    pub fn icon(self) -> egui::ImageSource<'static> {
        match self {
            Self::Load => crate::ui::icons::folder_open(),
            Self::Edit => crate::ui::icons::pencil(),
            Self::Duplicate => crate::ui::icons::copy(),
            Self::Remove => crate::ui::icons::trash(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEvent {
    pub name: String,
    pub action: LibraryAction,
}

pub fn dense_rows(ui: &mut egui::Ui) {
    let tokens = DesignTokens::from_style(ui.style());
    ui.spacing_mut().interact_size.y = tokens.dense_row_height;
    ui.spacing_mut().item_spacing.y = tokens.dense_row_gap;
    ui.spacing_mut().button_padding.y = tokens.dense_row_gap;
}

const CONTEXT_MENU_WIDTH: f32 = 140.0;

fn context_menu_item(ui: &mut egui::Ui, action: LibraryAction) -> egui::Response {
    let icon = egui::Image::new(action.icon())
        .fit_to_exact_size(egui::Vec2::splat(14.0))
        .tint(ui.visuals().text_color());
    ui.add(egui::Button::image_and_text(icon, action.label()))
}

pub fn library_tree(
    ui: &mut egui::Ui,
    id: egui::Id,
    names: &[String],
    selected: Option<&str>,
    menu_actions: &[LibraryAction],
    hover: &str,
) -> Option<LibraryEvent> {
    if names.is_empty() {
        return None;
    }

    let width_id = id.with("rendered_width");
    let available_width = ui.available_width();
    let width_changed = ui
        .data(|data| data.get_temp::<f32>(width_id))
        .is_none_or(|last| (last - available_width).abs() > 0.5);
    let mut state = if width_changed {
        egui_ltreeview::TreeViewState::<usize>::default()
    } else {
        egui_ltreeview::TreeViewState::<usize>::load(ui, id).unwrap_or_default()
    };
    match selected.and_then(|name| names.iter().position(|n| n == name)) {
        Some(index) => state.set_one_selected(index),
        None => state.set_selected(Vec::new()),
    }

    let mut menu_event = None;
    let (_, actions) = egui_ltreeview::TreeView::new(id)
        .allow_multi_selection(false)
        .allow_drag_and_drop(false)
        .show_state(ui, &mut state, |builder| {
            for (index, name) in names.iter().enumerate() {
                let mut node = egui_ltreeview::NodeBuilder::leaf(index).label_ui(|ui| {
                    ui.add(egui::Label::new(name).selectable(false).truncate())
                        .on_hover_text(hover);
                });
                if !menu_actions.is_empty() {
                    node = node.context_menu(|ui| {
                        dense_rows(ui);
                        ui.set_min_width(CONTEXT_MENU_WIDTH);
                        for action in menu_actions {
                            if context_menu_item(ui, *action).clicked() {
                                menu_event = Some(LibraryEvent {
                                    name: name.clone(),
                                    action: *action,
                                });
                                ui.close();
                            }
                        }
                    });
                }
                builder.node(node);
            }
        });

    state.set_selected(Vec::new());
    state.store(ui, id);
    ui.data_mut(|data| data.insert_temp(width_id, available_width));

    if let Some(event) = menu_event {
        return Some(event);
    }
    actions.into_iter().find_map(|action| match action {
        egui_ltreeview::Action::SetSelected(selected) => selected
            .first()
            .and_then(|index| names.get(*index))
            .map(|name| LibraryEvent {
                name: name.clone(),
                action: LibraryAction::Load,
            }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_menu_row_keeps_its_full_width_after_the_popup_was_sized_for_shorter_rows() {
        fn accesskit_rect(output: &egui::FullOutput, prefix: &str) -> Option<egui::Rect> {
            let update = output.platform_output.accesskit_update.as_ref()?;
            update
                .nodes
                .iter()
                .find(|(_, node)| node.label().is_some_and(|label| label.starts_with(prefix)))
                .and_then(|(_, node)| node.bounds())
                .map(|bounds| {
                    egui::Rect::from_min_max(
                        egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                    )
                })
        }

        fn painted_widths(output: &egui::FullOutput) -> Vec<(String, f32)> {
            fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, f32)>) {
                match shape {
                    egui::epaint::Shape::Text(text) => {
                        out.push((text.galley.job.text.clone(), text.galley.rect.width()));
                    }
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

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let rows = std::cell::RefCell::new(vec!["a".to_owned()]);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        let frame = |events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.horizontal(|ui| {
                        ui.menu_button("Tools", |ui| {
                            ui.menu_button("Run script", |ui| {
                                for row in rows.borrow().iter() {
                                    menu_row(ui, row, None, true, None);
                                }
                            });
                        });
                    });
                },
            )
        };
        let click = |pos: egui::Pos2| {
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
        };
        let open_submenu = || {
            let output = frame(Vec::new());
            let tools =
                accesskit_rect(&output, "Tools").expect("the Tools menu button should exist");
            let output = frame(click(tools.center()));
            let submenu =
                accesskit_rect(&output, "Run script").expect("the submenu button should exist");
            let hover = vec![egui::Event::PointerMoved(submenu.center())];
            let _sizing_pass = frame(hover.clone());
            frame(hover)
        };

        let short = open_submenu();
        assert!(
            painted_widths(&short).iter().any(|(text, _)| text == "a"),
            "the short row should open the submenu and size its popup"
        );
        let _ = frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        let long = "a-very-long-saved-script-name";
        rows.replace(vec![long.to_owned()]);
        let reopened = open_submenu();
        let painted = painted_widths(&reopened);
        let (_, width) = painted
            .iter()
            .find(|(text, _)| text == long)
            .unwrap_or_else(|| panic!("the long row should be painted, got {painted:?}"));

        let font = egui::TextStyle::Button.resolve(&ctx.global_style());
        let natural = ctx
            .fonts_mut(|fonts| fonts.layout_no_wrap(long.to_owned(), font, egui::Color32::WHITE))
            .rect
            .width();
        assert!(
            (*width - natural).abs() < 1.0,
            "a menu row must lay out at its natural width even when the popup was first sized for \
             shorter rows, got {width} instead of {natural}"
        );
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

    #[test]
    fn menu_row_shortcuts_share_one_right_aligned_column() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        let frame = |events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.horizontal(|ui| {
                        ui.menu_button("File", |ui| {
                            menu_row(ui, "Open", Some("Ctrl+O"), true, None);
                            menu_row(
                                ui,
                                "Export workspace image",
                                Some("Ctrl+Shift+E"),
                                true,
                                None,
                            );
                        });
                    });
                },
            )
        };
        let output = frame(Vec::new());
        let menu = text_layout_rects(&output)
            .into_iter()
            .find(|(text, _)| text == "File")
            .expect("the File menu button should be painted")
            .1;
        let pos = menu.center();
        let _ = frame(vec![
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
        ]);
        let output = frame(Vec::new());
        let rects = text_layout_rects(&output);
        let find = |wanted: &str| {
            rects
                .iter()
                .find(|(text, _)| text == wanted)
                .unwrap_or_else(|| panic!("{wanted} should be painted, got {rects:?}"))
                .1
        };

        assert_eq!(
            find("Ctrl+O").right(),
            find("Ctrl+Shift+E").right(),
            "menu shortcuts must share one right-aligned column"
        );
    }

    fn library_names() -> Vec<String> {
        vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()]
    }

    fn run_library_tree(
        selected: Option<&str>,
    ) -> (Option<LibraryEvent>, egui::FullOutput, Vec<String>) {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = library_names();
        let mut event = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 600.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            event = library_tree(
                ui,
                egui::Id::new("library-tree-test"),
                &names,
                selected,
                &[LibraryAction::Edit, LibraryAction::Remove],
                "Load entry",
            );
        });
        (event, output, names)
    }

    fn painted_text(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => out.push(text.galley.job.text.clone()),
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

    #[test]
    fn library_tree_renders_every_entry_without_an_inline_menu() {
        let (event, output, names) = run_library_tree(Some("beta"));
        assert!(event.is_none());
        let painted = painted_text(&output);
        for name in &names {
            assert!(
                painted.iter().any(|text| text == name),
                "{name} should be painted as a row, got {painted:?}"
            );
        }
        assert!(
            !painted.iter().any(|text| text.as_str() == "..."),
            "rows must not carry an inline overflow menu, got {painted:?}"
        );
    }

    fn find_label_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_label_rect(shape, expected)),
            _ => None,
        }
    }

    fn selection_highlight_y(
        ctx: &egui::Context,
        id: egui::Id,
        names: &[String],
        selected: Option<&str>,
    ) -> Option<f32> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 600.0),
            )),
            ..Default::default()
        };
        let visuals = ctx.global_style().visuals.clone();
        let focused_fill = visuals.selection.bg_fill;
        let unfocused_fill = visuals.widgets.inactive.weak_bg_fill.linear_multiply(0.3);
        let output = ctx.run_ui(input, |ui| {
            library_tree(ui, id, names, selected, &[], "Load entry");
        });
        fn walk(
            shape: &egui::epaint::Shape,
            fills: (egui::Color32, egui::Color32),
            out: &mut Option<f32>,
        ) {
            match shape {
                egui::epaint::Shape::Rect(rect) if rect.fill == fills.0 || rect.fill == fills.1 => {
                    *out = Some(rect.rect.center().y);
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|s| walk(s, fills, out));
                }
                _ => {}
            }
        }
        let mut found = None;
        for clipped in &output.shapes {
            walk(&clipped.shape, (focused_fill, unfocused_fill), &mut found);
        }
        found
    }

    #[test]
    fn library_tree_highlights_only_the_loaded_entry() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = library_names();
        let id = egui::Id::new("library-selection-test");

        assert!(
            selection_highlight_y(&ctx, id, &names, None).is_none(),
            "nothing should be highlighted when no entry is loaded"
        );

        let first = selection_highlight_y(&ctx, id, &names, Some("alpha"))
            .expect("the loaded entry should be highlighted");
        let last = selection_highlight_y(&ctx, id, &names, Some("gamma"))
            .expect("the loaded entry should be highlighted");

        assert!(
            last > first,
            "the highlight should follow the loaded entry down the list ({first} -> {last})"
        );
    }

    fn row_rect_at_width(
        ctx: &egui::Context,
        id: egui::Id,
        names: &[String],
        label: &str,
        w: f32,
    ) -> Option<egui::Rect> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::Panel::left("menu-visibility-drawer")
                .resizable(false)
                .exact_size(w)
                .show_inside(ui, |ui| {
                    library_tree(ui, id, names, None, &[LibraryAction::Remove], "Load entry");
                });
        });
        output
            .shapes
            .iter()
            .find_map(|clipped| find_label_rect(&clipped.shape, label))
    }

    #[test]
    fn library_rows_stay_inside_the_drawer_after_shrinking_it() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = vec!["alpha".to_owned()];
        let id = egui::Id::new("library-menu-visibility");

        row_rect_at_width(&ctx, id, &names, "alpha", 400.0);
        row_rect_at_width(&ctx, id, &names, "alpha", 400.0);
        let narrow = row_rect_at_width(&ctx, id, &names, "alpha", 150.0)
            .expect("the entry should still be painted after shrinking");

        assert!(
            narrow.right() <= 150.0,
            "the row is drawn at x={} which is outside a 150 point drawer",
            narrow.right()
        );
    }

    #[test]
    fn an_entry_name_wider_than_the_drawer_is_truncated_inside_it() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let name = "a_long_saved_entry_name_that_overflows_the_drawer".to_owned();
        let names = vec![name.clone()];
        let id = egui::Id::new("library-menu-long-name");

        let narrow = row_rect_at_width(&ctx, id, &names, &name, 150.0)
            .expect("a long entry name must still be painted");

        assert!(
            narrow.right() <= 150.0,
            "the row is drawn at x={} which is outside a 150 point drawer",
            narrow.right()
        );
    }

    #[test]
    fn library_rows_stay_clickable_after_scrolling() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names: Vec<String> = (0..60).map(|index| format!("entry_{index:02}")).collect();
        let id = egui::Id::new("library-scroll-click");

        let mut clock = 0.0f64;
        let mut frame = |events: Vec<egui::Event>,
                         pointer: Option<egui::Pos2>|
         -> (Option<LibraryEvent>, Vec<(String, egui::Rect)>) {
            clock += 0.5;
            let mut input = egui::RawInput {
                time: Some(clock),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.push(egui::Event::PointerMoved(pos));
            }
            input.events.extend(events);
            let mut event = None;
            let output = ctx.run_ui(input, |ui| {
                event = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        library_tree(ui, id, &names, None, &[], "Load entry")
                    })
                    .inner;
            });
            fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
                match shape {
                    egui::epaint::Shape::Text(text) => {
                        out.push((text.galley.job.text.clone(), text.visual_bounding_rect()));
                    }
                    egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut rows = Vec::new();
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut rows);
            }
            (event, rows)
        };

        frame(vec![], None);
        let mut rows = Vec::new();
        for _ in 0..8 {
            rows = frame(
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -120.0),
                    modifiers: Default::default(),
                    phase: egui::TouchPhase::Move,
                }],
                Some(egui::pos2(200.0, 100.0)),
            )
            .1;
        }

        for _ in 0..30 {
            let next = frame(vec![], None).1;
            let settled = next
                .iter()
                .map(|(text, rect)| (text.clone(), rect.top().round() as i32))
                .eq(rows.iter().map(|(text, rect): &(String, egui::Rect)| {
                    (text.clone(), rect.top().round() as i32)
                }));
            rows = next;
            if settled {
                break;
            }
        }

        let (label, rect) = rows
            .iter()
            .filter(|(text, rect)| text.starts_with("entry_") && rect.top() > 100.0)
            .max_by(|a, b| a.1.top().total_cmp(&b.1.top()))
            .cloned()
            .unwrap_or_else(|| panic!("rows should be painted after scrolling, got {rows:?}"));
        let target = rect.center();

        let mut clicked = None;
        for pressed in [true, false] {
            let (event, _) = frame(
                vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                }],
                Some(target),
            );
            clicked = clicked.or(event);
        }

        assert_eq!(
            clicked,
            Some(LibraryEvent {
                name: label.clone(),
                action: LibraryAction::Load,
            }),
            "clicking {label} at {target:?} after scrolling should load it"
        );
    }

    #[test]
    fn dense_rows_applies_the_dense_row_tokens() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let tokens = DesignTokens::default();
        let mut spacing = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            dense_rows(ui);
            spacing = Some(ui.spacing().clone());
        });
        let spacing = spacing.expect("the menu ui should have been built");
        assert_eq!(spacing.interact_size.y, tokens.dense_row_height);
        assert_eq!(spacing.item_spacing.y, tokens.dense_row_gap);
        assert_eq!(spacing.button_padding.y, tokens.dense_row_gap);
    }

    #[test]
    fn library_action_labels_are_stable() {
        assert_eq!(LibraryAction::Edit.label(), "Edit");
        assert_eq!(LibraryAction::Duplicate.label(), "Duplicate");
        assert_eq!(LibraryAction::Remove.label(), "Remove");
    }

    #[test]
    fn status_chip_uses_text_and_not_only_color() {
        let model = StatusChip::connected("UDP 14550", "48 Hz");
        assert_eq!(model.label, "UDP 14550");
        assert_eq!(model.detail.as_deref(), Some("48 Hz"));
        assert_eq!(model.state, StatusState::Success);
    }

    #[test]
    fn icon_buttons_emit_accessible_labels_and_selected_state() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let texture =
                egui::load::SizedTexture::new(egui::TextureId::default(), egui::Vec2::splat(1.0));
            icon_button(ui, texture.into(), "Pin plot", true);
            icon_button(ui, texture.into(), "Unpinned plot", false);
        });
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let find = |label: &str| {
            update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .find(|node| node.label() == Some(label))
                .expect("labelled icon button should exist")
        };

        let selected = find("Pin plot");
        assert_eq!(selected.role(), egui::accesskit::Role::Button);
        assert_eq!(selected.toggled(), Some(egui::accesskit::Toggled::True));
        assert_eq!(
            find("Unpinned plot").toggled(),
            Some(egui::accesskit::Toggled::False)
        );
    }

    #[test]
    fn right_clicking_a_row_opens_a_context_menu_that_emits_its_action() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = library_names();
        let id = egui::Id::new("library-context-menu");
        let actions = [LibraryAction::Duplicate, LibraryAction::Remove];

        let event = std::cell::RefCell::new(None);
        let frame = |events: Vec<egui::Event>| -> Vec<egui::accesskit::Node> {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let found = library_tree(ui, id, &names, None, &actions, "Load entry");
                    if found.is_some() {
                        *event.borrow_mut() = found;
                    }
                },
            );
            output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .map(|(_, node)| node)
                .collect()
        };

        let nodes = frame(Vec::new());
        assert!(
            !nodes.iter().any(|node| node.label() == Some("Remove")),
            "the context menu must stay closed until a right click"
        );

        let row = egui::pos2(60.0, 20.0);
        for pressed in [true, false] {
            frame(vec![
                egui::Event::PointerMoved(row),
                egui::Event::PointerButton {
                    pos: row,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        let nodes = frame(Vec::new());
        let remove = nodes
            .iter()
            .find(|node| node.label() == Some("Remove"))
            .expect("right clicking a row opens its context menu");
        assert!(
            nodes.iter().any(|node| node.label() == Some("Duplicate")),
            "every configured action is offered"
        );
        assert!(
            event.borrow().is_none(),
            "opening the menu must not load the entry"
        );

        let bounds = remove.bounds().unwrap();
        let click = egui::pos2(
            ((bounds.x0 + bounds.x1) * 0.5) as f32,
            ((bounds.y0 + bounds.y1) * 0.5) as f32,
        );
        for pressed in [true, false] {
            frame(vec![
                egui::Event::PointerMoved(click),
                egui::Event::PointerButton {
                    pos: click,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }

        assert_eq!(
            event.into_inner(),
            Some(LibraryEvent {
                name: "alpha".to_owned(),
                action: LibraryAction::Remove,
            })
        );
    }
}

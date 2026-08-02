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
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            enabled,
            selected,
            tooltip,
        )
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
    let text = shortcut.map_or_else(|| label.to_owned(), |key| format!("{label}\t{key}"));
    let response = ui.add_enabled(enabled, egui::Button::new(text));
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

    let mut state = egui_ltreeview::TreeViewState::default();
    match selected.and_then(|name| names.iter().position(|n| n == name)) {
        Some(index) => state.set_one_selected(index),
        None => state.set_selected(Vec::new()),
    }

    let mut menu_event = None;
    let mut menu_consumed_click = false;
    let (_, actions) = egui_ltreeview::TreeView::new(id)
        .allow_multi_selection(false)
        .allow_drag_and_drop(false)
        .show_state(ui, &mut state, |builder| {
            for (index, name) in names.iter().enumerate() {
                builder.node(
                    egui_ltreeview::NodeBuilder::leaf(index).label_ui(|ui| {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if !menu_actions.is_empty() {
                                    let menu = ui.menu_button("...", |ui| {
                                        dense_rows(ui);
                                        for action in menu_actions {
                                            if ui.button(action.label()).clicked() {
                                                menu_event = Some(LibraryEvent {
                                                    name: name.clone(),
                                                    action: *action,
                                                });
                                                ui.close();
                                            }
                                        }
                                    });
                                    if menu.response.clicked() || menu.inner.is_some() {
                                        menu_consumed_click = true;
                                    }
                                }
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(name)
                                                .selectable(false)
                                                .truncate(),
                                        )
                                        .on_hover_text(hover);
                                    },
                                );
                            },
                        );
                    }),
                );
            }
            });

    if let Some(event) = menu_event {
        return Some(event);
    }
    if menu_consumed_click {
        return None;
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
    fn library_tree_renders_every_entry_with_its_menu() {
        let (event, output, names) = run_library_tree(Some("beta"));
        assert!(event.is_none());
        let painted = painted_text(&output);
        for name in &names {
            assert!(
                painted.iter().any(|text| text == name),
                "{name} should be painted as a row, got {painted:?}"
            );
        }
        assert_eq!(
            painted.iter().filter(|text| text.as_str() == "...").count(),
            names.len(),
            "every row should carry its overflow menu"
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
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == fills.0 || rect.fill == fills.1 =>
                {
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

    fn menu_rect_at_width(ctx: &egui::Context, id: egui::Id, names: &[String], w: f32) -> Option<egui::Rect> {
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
            .find_map(|clipped| find_label_rect(&clipped.shape, "..."))
    }

    #[test]
    fn library_row_menu_stays_visible_after_shrinking_the_drawer() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = vec!["alpha".to_owned()];
        let id = egui::Id::new("library-menu-visibility");

        menu_rect_at_width(&ctx, id, &names, 400.0);
        menu_rect_at_width(&ctx, id, &names, 400.0);
        let narrow = menu_rect_at_width(&ctx, id, &names, 150.0)
            .expect("the overflow menu should still be painted after shrinking");

        assert!(
            narrow.right() <= 150.0,
            "the row menu is drawn at x={} which is outside a 150 point drawer",
            narrow.right()
        );
    }

    #[test]
    fn library_row_menu_survives_an_entry_name_wider_than_the_drawer() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = vec!["a_long_saved_entry_name_that_overflows_the_drawer".to_owned()];
        let id = egui::Id::new("library-menu-long-name");

        let narrow = menu_rect_at_width(&ctx, id, &names, 150.0)
            .expect("a long entry name must not push the overflow menu out of the drawer");

        assert!(
            narrow.right() <= 150.0,
            "the row menu is drawn at x={} which is outside a 150 point drawer",
            narrow.right()
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
            let texture = egui::load::SizedTexture::new(
                egui::TextureId::default(),
                egui::Vec2::splat(1.0),
            );
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
}

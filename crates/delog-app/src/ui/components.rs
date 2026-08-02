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

pub fn clamp_to_available_width<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let max_rect = ui.available_rect_before_wrap();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(max_rect));
    child.set_clip_rect(ui.clip_rect().intersect(max_rect));
    let result = add_contents(&mut child);
    let height = child.min_rect().height();
    ui.allocate_rect(
        egui::Rect::from_min_size(max_rect.min, egui::vec2(max_rect.width(), height)),
        egui::Sense::hover(),
    );
    result
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

    let mut state = egui_ltreeview::TreeViewState::load(ui, id).unwrap_or_default();
    match selected.and_then(|name| names.iter().position(|n| n == name)) {
        Some(index) => state.set_one_selected(index),
        None => state.set_selected(Vec::new()),
    }

    let mut menu_event = None;
    let mut menu_consumed_click = false;
    let (_, actions) = clamp_to_available_width(ui, |ui| {
        egui_ltreeview::TreeView::new(id)
        .allow_multi_selection(false)
        .allow_drag_and_drop(false)
        .show_state(ui, &mut state, |builder| {
            for (index, name) in names.iter().enumerate() {
                builder.node(
                    egui_ltreeview::NodeBuilder::leaf(index).label_ui(|ui| {
                        ui.add(egui::Label::new(name).selectable(false))
                            .on_hover_text(hover);
                        if menu_actions.is_empty() {
                            return;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let menu = ui.menu_button("...", |ui| {
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
                            },
                        );
                    }),
                );
            }
            })
    });
    state.store(ui, id);

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

    #[test]
    fn library_tree_marks_only_the_loaded_entry_selected() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = library_names();
        let id = egui::Id::new("library-selection-test");
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 600.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input(), |ui| {
            library_tree(ui, id, &names, Some("gamma"), &[], "Load entry");
        });
        let _ = ctx.run_ui(input(), |ui| {
            let state =
                egui_ltreeview::TreeViewState::<usize>::load(ui, id).expect("state was stored");
            assert_eq!(state.selected(), &vec![2]);
        });
    }

    #[test]
    fn library_tree_handles_an_empty_library() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let mut event = Some(LibraryEvent {
            name: "stale".to_owned(),
            action: LibraryAction::Load,
        });
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            event = library_tree(
                ui,
                egui::Id::new("empty-library-tree"),
                &[],
                None,
                &[LibraryAction::Remove],
                "Load entry",
            );
        });
        assert!(event.is_none());
    }

    #[test]
    fn clicking_an_entry_label_loads_it() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = library_names();
        let id = egui::Id::new("library-click-test");
        let mut event = None;
        let mut label_pos = None;

        let frame = |events: Vec<egui::Event>,
                     pointer: Option<egui::Pos2>,
                     event: &mut Option<LibraryEvent>,
                         label_pos: &mut Option<egui::Pos2>| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 600.0),
                )),
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.push(egui::Event::PointerMoved(pos));
            }
            input.events.extend(events);
            let output = ctx.run_ui(input, |ui| {
                if let Some(fired) = library_tree(ui, id, &names, None, &[], "Load entry") {
                    *event = Some(fired);
                }
            });
            if label_pos.is_none() {
                *label_pos = output
                    .shapes
                    .iter()
                    .find_map(|clipped| find_label_rect(&clipped.shape, "beta"))
                    .map(|rect| rect.center());
            }
        };

        frame(vec![], None, &mut event, &mut label_pos);
        frame(vec![], None, &mut event, &mut label_pos);
        let target = label_pos.expect("beta should be painted");

        frame(
            vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
            Some(target),
            &mut event,
            &mut label_pos,
        );
        frame(
            vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            Some(target),
            &mut event,
            &mut label_pos,
        );

        assert_eq!(
            event,
            Some(LibraryEvent {
                name: "beta".to_owned(),
                action: LibraryAction::Load,
            }),
            "clicking the entry text itself should load it"
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

    #[test]
    fn library_tree_does_not_pin_the_drawer_to_its_widest_layout() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let names = vec![
            "a_very_long_saved_entry_name_that_makes_the_tree_wide".to_owned(),
            "another_quite_long_saved_entry_name_here".to_owned(),
        ];
        let id = egui::Id::new("library-shrink-test");

        let measure = |panel_width: f32| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            };
            let mut used = 0.0;
            let _ = ctx.run_ui(input, |ui| {
                egui::Panel::left("shrink-drawer")
                    .resizable(false)
                    .exact_size(panel_width)
                    .show_inside(ui, |ui| {
                        library_tree(ui, id, &names, None, &[], "Load entry");
                        used = ui.min_rect().width();
                    });
            });
            used
        };

        measure(400.0);
        measure(400.0);
        let narrow = measure(150.0);

        assert!(
            narrow <= 160.0,
            "after being shown wide the tree still demands {narrow} points, \
             which pins the drawer open and blocks resizing it back down"
        );
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

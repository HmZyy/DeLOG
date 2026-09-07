use delog_core::identity::{FieldId, TopicId};

pub(super) fn combo_label<'a, T: PartialEq>(items: &'a [(T, String)], sel: &Option<T>) -> &'a str {
    match sel {
        Some(s) => items
            .iter()
            .find(|(v, _)| v == s)
            .map(|(_, l)| l.as_str())
            .unwrap_or("-"),
        None => "-",
    }
}

/// Returns `true` if the selection changed. Ids are derived from the calling
/// `ui` so repeated salts across several vehicles do not collide.
pub(super) fn searchable_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<T>,
    items: &[(T, String)],
) -> bool {
    let before = *sel;
    let filter_id = ui.make_persistent_id((salt, "filter"));
    let highlight_id = ui.make_persistent_id((salt, "highlight"));
    // `CloseOnClickOutside` keeps the popup open while typing in the search box;
    // a plain ComboBox closes on that click.
    let button = ui.button(combo_label(items, sel));
    egui::Popup::from_toggle_button_response(&button)
        .id(button.id.with("popup"))
        .width(170.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(170.0);
            ui.set_max_width(170.0);
            {
                let mut filter: String =
                    ui.memory_mut(|m| m.data.get_temp(filter_id).unwrap_or_default());
                let response = ui.add(
                    egui::TextEdit::singleline(&mut filter)
                        .hint_text("search…")
                        .desired_width(f32::INFINITY),
                );
                response.request_focus();
                let filter_changed = response.changed();
                let needle = filter.to_ascii_lowercase();
                ui.memory_mut(|m| m.data.insert_temp(filter_id, filter));
                let visible = items
                    .iter()
                    .filter(|(_, name)| {
                        needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
                    })
                    .map(|(value, name)| (*value, name.as_str()))
                    .collect::<Vec<_>>();
                let stored_highlight = ui.memory_mut(|m| m.data.get_temp::<usize>(highlight_id));
                let initialized_highlight = stored_highlight.is_none();
                let mut highlighted = stored_highlight.unwrap_or_else(|| {
                    visible
                        .iter()
                        .position(|(value, _)| *sel == Some(*value))
                        .unwrap_or(0)
                });
                let mut highlight_changed_by_keyboard = false;
                if !visible.is_empty() {
                    highlighted = highlighted.min(visible.len() - 1);
                    let move_down = ui
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
                    let move_up =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
                    let choose =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    if move_down {
                        let next = (highlighted + 1).min(visible.len() - 1);
                        highlight_changed_by_keyboard |= next != highlighted;
                        highlighted = next;
                    }
                    if move_up {
                        let next = highlighted.saturating_sub(1);
                        highlight_changed_by_keyboard |= next != highlighted;
                        highlighted = next;
                    }
                    if choose {
                        *sel = Some(visible[highlighted].0);
                        ui.close();
                    }
                }
                ui.memory_mut(|m| m.data.insert_temp(highlight_id, highlighted));
                let scroll_to_highlight =
                    highlight_changed_by_keyboard || initialized_highlight || filter_changed;
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for (i, (value, name)) in visible.iter().enumerate() {
                            let selected = *sel == Some(*value) || i == highlighted;
                            let response = ui.selectable_label(selected, *name);
                            if scroll_to_highlight && i == highlighted {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                ui.memory_mut(|m| m.data.insert_temp(highlight_id, i));
                                *sel = Some(*value);
                                ui.close();
                            }
                        }
                    });
            }
        });
    *sel != before
}

pub(super) fn field_combo(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<FieldId>,
    fields: &[(FieldId, String)],
) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(combo_label(fields, sel))
        .show_ui(ui, |ui| {
            for (id, name) in fields {
                ui.selectable_value(sel, Some(*id), name);
            }
        });
}

pub(super) fn choose_custom_glb_path(current_path: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Choose custom GLB")
        .add_filter("GLB models", &["glb", "GLB"])
        .add_filter("All files", &["*"]);
    let current = std::path::Path::new(current_path.trim());
    if let Some(parent) = current.parent()
        && !parent.as_os_str().is_empty()
    {
        dialog = dialog.set_directory(parent);
    }
    dialog
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn icon(ui: &egui::Ui, src: egui::ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(src)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color())
}

pub(super) fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).strong());
    ui.add_space(2.0);
}

pub(super) fn grid_field(
    ui: &mut egui::Ui,
    salt: &str,
    label: &str,
    sel: &mut Option<FieldId>,
    cols: &[(FieldId, String)],
) {
    ui.label(label);
    field_combo(ui, salt, sel, cols);
    ui.end_row();
}

pub(super) fn topic_combo(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<TopicId>,
    topics: &[(TopicId, String)],
) -> bool {
    searchable_combo(ui, salt, sel, topics)
}

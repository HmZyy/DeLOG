use delog_core::identity::FieldId;

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
pub(super) const LIST_MAX_HEIGHT: f32 = 260.0;

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
    let button = ui.add_sized(
        [control_width(ui), ui.spacing().interact_size.y],
        egui::Button::new((combo_label(items, sel), egui::Atom::grow()))
            .wrap_mode(egui::TextWrapMode::Truncate),
    );
    let popup_width = button.rect.width();
    egui::Popup::from_toggle_button_response(&button)
        .id(button.id.with("popup"))
        .width(popup_width)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(popup_width);
            combo_list(ui, filter_id, highlight_id, sel, items);
        });
    *sel != before
}

pub(super) fn combo_list<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    filter_id: egui::Id,
    highlight_id: egui::Id,
    sel: &mut Option<T>,
    items: &[(T, String)],
) {
    let search_height = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    ui.set_max_height(search_height + LIST_MAX_HEIGHT);

    let mut filter: String = ui.memory_mut(|m| m.data.get_temp(filter_id).unwrap_or_default());
    let response = ui.add(
        egui::TextEdit::singleline(&mut filter)
            .id(filter_id.with("edit"))
            .hint_text("search…")
            .desired_width(f32::INFINITY),
    );
    response.request_focus();
    let filter_changed = response.changed();
    let needle = filter.to_ascii_lowercase();
    ui.memory_mut(|m| m.data.insert_temp(filter_id, filter));
    let visible = items
        .iter()
        .filter(|(_, name)| needle.is_empty() || name.to_ascii_lowercase().contains(&needle))
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
    let mut chose = false;
    let mut highlight_changed_by_keyboard = false;
    if !visible.is_empty() {
        highlighted = highlighted.min(visible.len() - 1);
        let move_down =
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
        let move_up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
        let choose = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
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
            chose = true;
            ui.close();
        }
    }
    ui.memory_mut(|m| m.data.insert_temp(highlight_id, highlighted));
    let scroll_to_highlight =
        highlight_changed_by_keyboard || initialized_highlight || filter_changed;

    egui::ScrollArea::vertical()
        .max_height(LIST_MAX_HEIGHT)
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
                    *sel = Some(*value);
                    chose = true;
                    ui.close();
                }
            }
        });

    if chose {
        ui.memory_mut(|m| {
            m.data.remove::<String>(filter_id);
            m.data.remove::<usize>(highlight_id);
        });
    }
}

pub(super) fn field_combo(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<FieldId>,
    fields: &[(FieldId, String)],
) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(combo_label(fields, sel))
        .width(control_width(ui))
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

pub(super) fn control_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 4.0).clamp(120.0, 320.0)
}

pub(super) fn form_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(label);
    add(ui);
    ui.end_row();
}

pub(super) fn form_grid(ui: &mut egui::Ui, salt: &str, add: impl FnOnce(&mut egui::Ui)) {
    let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
    egui::Grid::new(salt)
        .num_columns(2)
        .spacing([tokens.space_md, tokens.space_sm])
        .min_col_width(92.0)
        .show(ui, add);
}

pub(super) fn section(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    title: &str,
    summary: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    let id = ui.make_persistent_id(salt);
    let state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
    let open = state.is_open();
    state
        .show_header(ui, |ui| {
            ui.label(egui::RichText::new(title).strong());
            if !open && !summary.is_empty() {
                ui.label(egui::RichText::new(summary).weak().small());
            }
        })
        .body(add);
}

pub(super) fn status_banner(ui: &mut egui::Ui, missing: &[&'static str]) {
    if missing.is_empty() {
        return;
    }
    let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
    let color = ui.visuals().warn_fg_color;
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .corner_radius(tokens.radius)
        .inner_margin(tokens.space_sm)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = tokens.space_xs;
                ui.add(
                    egui::Image::new(crate::ui::icons::circle_alert())
                        .fit_to_exact_size(egui::vec2(14.0, 14.0))
                        .tint(color),
                );
                ui.label(
                    egui::RichText::new(format!("Not rendered yet - set {}", missing.join(", ")))
                        .color(color),
                );
            });
        });
}

pub(super) fn swatch(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    ui.painter().rect_filled(rect, 3.0, color);
    ui.painter().rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
}

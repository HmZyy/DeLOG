//! Manual markers / bookmarks: user-placed time
//! markers with a label, colour and note. Distinct from the measurement
//! cursor (a single transient delta cursor) — these are multiple, labelled,
//! navigable, and persisted with the session.

use std::collections::HashSet;

/// One bookmark at a canonical time. `id` is a stable identity so the dock and
/// timeline can address a marker for edit/delete/drag even as the time-sorted
/// display order shifts.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub id: u64,
    pub t_us: i64,
    pub label: String,
    /// sRGB straight RGBA, like `TraceRef`.
    pub color: [f32; 4],
    pub note: String,
}

impl Marker {
    pub fn color32(&self) -> egui::Color32 {
        let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(
            u(self.color[0]),
            u(self.color[1]),
            u(self.color[2]),
            u(self.color[3]),
        )
    }
}

/// The session's marker collection. Monotonic `next_id` never reuses numbers,
/// so labels and ids stay stable across deletions.
#[derive(Debug, Default)]
pub struct Markers {
    items: Vec<Marker>,
    next_id: u64,
}

impl Markers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a marker at `t_us` with an auto label (`Marker N`) and the next
    /// palette colour. Returns the new id.
    pub fn add_at(&mut self, t_us: i64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let color = delog_render::palette::trace_color(id as usize).to_srgb_f32();
        self.items.push(Marker {
            id,
            t_us,
            label: format!("Marker {}", id + 1),
            color,
            note: String::new(),
        });
        id
    }

    /// Re-add a marker loaded from persistence, assigning a fresh id.
    pub fn push_loaded(&mut self, t_us: i64, label: String, color: [f32; 4], note: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Marker {
            id,
            t_us,
            label,
            color,
            note,
        });
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|m| m.id != id);
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Marker> {
        self.items.iter_mut().find(|m| m.id == id)
    }

    /// Markers sorted ascending by time (display order, flags, verticals).
    pub fn by_time(&self) -> Vec<&Marker> {
        let mut v: Vec<&Marker> = self.items.iter().collect();
        v.sort_by_key(|m| m.t_us);
        v
    }

    pub fn as_slice(&self) -> &[Marker] {
        &self.items
    }
}

/// Bottom dock listing the session's markers: per row a colour
/// swatch, relative time, editable label + note, and jump / delete controls.
/// Returns a jump target time when a row's jump button is clicked.
#[derive(Default)]
pub struct MarkersDock {
    pub open: bool,
    /// Ids of rows ticked for bulk delete (transient UI selection).
    selected: HashSet<u64>,
}

impl MarkersDock {
    pub fn ui(&mut self, ui: &mut egui::Ui, markers: &mut Markers, origin_us: i64) -> Option<i64> {
        let ids: Vec<u64> = markers.by_time().iter().map(|m| m.id).collect();
        // Drop selections for markers deleted elsewhere.
        self.selected.retain(|id| ids.contains(id));
        let selected_count = ids.iter().filter(|id| self.selected.contains(id)).count();
        let all_selected = !ids.is_empty() && selected_count == ids.len();
        let any_selected = selected_count > 0;

        let mut delete_selected = false;
        ui.horizontal(|ui| {
            // Tristate select-all: checked when every row is ticked, a dash when
            // only some are, empty when none. Clicking selects all unless all are
            // already selected (then it clears).
            let mut master = all_selected;
            let resp = ui
                .add_enabled(!ids.is_empty(), egui::Checkbox::new(&mut master, ""))
                .on_hover_text("Select all / none");
            if resp.clicked() {
                if all_selected {
                    self.selected.clear();
                } else {
                    self.selected = ids.iter().copied().collect();
                }
            }
            if any_selected && !all_selected {
                // Indeterminate dash painted across the (empty) checkbox box.
                let iw = ui.spacing().icon_width;
                ui.painter().hline(
                    egui::Rangef::new(resp.rect.left() + iw * 0.3, resp.rect.left() + iw * 0.7),
                    resp.rect.center().y,
                    egui::Stroke::new(2.0, ui.visuals().text_color()),
                );
            }
            ui.strong("Markers");
            ui.weak(format!("{} marker(s)", ids.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.open = false;
                }
                if ui
                    .add_enabled(
                        any_selected,
                        egui::Button::new(format!("Delete selected ({selected_count})")),
                    )
                    .clicked()
                {
                    delete_selected = true;
                }
            });
        });
        ui.separator();

        if delete_selected {
            for id in self.selected.drain() {
                markers.remove(id);
            }
        }

        let mut jump = None;
        let mut to_remove = None;
        // Always render the scroll region (even when empty), and let it fill
        // the panel height. egui stores the panel size from its content rect,
        // so a content-shrinking scroll area makes the dock snap back to
        // content height after a resize; `auto_shrink([false, false])` fills the
        // dragged height so the resize sticks (matching the performance dock).
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let row_ids: Vec<u64> = markers.by_time().iter().map(|m| m.id).collect();
                if row_ids.is_empty() {
                    ui.weak("No markers - press M to add one at the playhead.");
                    return;
                }
                for id in row_ids {
                    let selected = &mut self.selected;
                    let Some(m) = markers.get_mut(id) else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        let mut sel = selected.contains(&id);
                        if ui.checkbox(&mut sel, "").changed() {
                            if sel {
                                selected.insert(id);
                            } else {
                                selected.remove(&id);
                            }
                        }
                        let mut color = m.color32();
                        if egui::color_picker::color_edit_button_srgba(
                            ui,
                            &mut color,
                            egui::color_picker::Alpha::Opaque,
                        )
                        .changed()
                        {
                            m.color = crate::legend::color32_to_srgb(color);
                        }
                        ui.monospace(fmt_rel(m.t_us, origin_us));
                        ui.add(
                            egui::TextEdit::singleline(&mut m.label)
                                .desired_width(140.0)
                                .hint_text("label"),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut m.note)
                                .desired_width(180.0)
                                .hint_text("note"),
                        );
                        let icon_size = egui::Vec2::splat(ui.spacing().icon_width);
                        let jump_icon = egui::Image::new(crate::icons::crosshair())
                            .fit_to_exact_size(icon_size)
                            .tint(ui.visuals().text_color());
                        if ui
                            .add(egui::Button::image(jump_icon))
                            .on_hover_text("Jump to marker")
                            .clicked()
                        {
                            jump = Some(m.t_us);
                        }
                        let delete_icon = egui::Image::new(crate::icons::trash())
                            .fit_to_exact_size(icon_size)
                            .tint(ui.visuals().text_color());
                        if ui
                            .add(egui::Button::image(delete_icon))
                            .on_hover_text("Delete marker")
                            .clicked()
                        {
                            to_remove = Some(id);
                        }
                    });
                }
            });
        if let Some(id) = to_remove {
            markers.remove(id);
            self.selected.remove(&id);
        }
        jump
    }
}

/// Format a canonical time relative to the log origin as `m:ss.cc`.
fn fmt_rel(t_us: i64, origin_us: i64) -> String {
    let secs = (t_us - origin_us) as f64 * 1e-6;
    let sign = if secs < 0.0 { "-" } else { "" };
    let s = secs.abs();
    let m = (s / 60.0).floor() as i64;
    let rem = s - (m as f64) * 60.0;
    format!("{sign}{m}:{rem:05.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_assigns_increasing_ids_labels_and_distinct_colors() {
        let mut m = Markers::new();
        let a = m.add_at(100);
        let b = m.add_at(50);
        assert_eq!((a, b), (0, 1));
        assert_eq!(m.as_slice()[0].label, "Marker 1");
        assert_eq!(m.as_slice()[1].label, "Marker 2");
        assert_ne!(m.as_slice()[0].color, m.as_slice()[1].color);
    }

    #[test]
    fn by_time_sorts_ascending_regardless_of_insertion_order() {
        let mut m = Markers::new();
        m.add_at(100);
        m.add_at(50);
        m.add_at(75);
        let times: Vec<i64> = m.by_time().iter().map(|x| x.t_us).collect();
        assert_eq!(times, [50, 75, 100]);
    }

    #[test]
    fn remove_by_id_and_labels_do_not_reuse_numbers() {
        let mut m = Markers::new();
        let a = m.add_at(10);
        m.add_at(20);
        m.remove(a);
        assert_eq!(m.as_slice().len(), 1);
        // Next add keeps counting up — no reuse of "Marker 1".
        m.add_at(30);
        let labels: Vec<&str> = m.by_time().iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["Marker 2", "Marker 3"]);
    }

    #[test]
    fn get_mut_edits_in_place() {
        let mut m = Markers::new();
        let id = m.add_at(10);
        m.get_mut(id).unwrap().label = "Takeoff".to_string();
        assert_eq!(m.as_slice()[0].label, "Takeoff");
        assert!(m.get_mut(999).is_none());
    }

    #[test]
    fn fmt_rel_formats_minutes_seconds_centis() {
        assert_eq!(super::fmt_rel(3_210_000, 0), "0:03.21");
        assert_eq!(super::fmt_rel(62_000_000, 0), "1:02.00");
        assert_eq!(super::fmt_rel(0, 1_000_000), "-0:01.00");
    }
}

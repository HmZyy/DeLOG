#[cfg(feature = "scripting")]
use std::collections::HashMap;
use std::collections::HashSet;

#[cfg(feature = "scripting")]
use delog_script::{MarkerCommand, PendingMarker};

#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkerOrigin {
    Manual,
    #[cfg(feature = "scripting")]
    Script {
        owner: String,
        generation: u64,
    },
}

#[cfg(feature = "scripting")]
#[derive(Debug, Clone, Copy)]
struct ScriptMarkerState {
    generation: u64,
    next_palette_index: usize,
}

/// `id` is a stable identity so the dock and timeline can address a marker for
/// edit/delete/drag even as the time-sorted display order shifts.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub id: u64,
    pub t_us: i64,
    pub label: String,
    /// sRGB straight RGBA.
    pub color: [f32; 4],
    pub note: String,
    origin: MarkerOrigin,
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

/// Monotonic `next_id` never reuses numbers, so labels and ids stay stable
/// across deletions.
#[derive(Debug, Default)]
pub struct Markers {
    items: Vec<Marker>,
    next_id: u64,
    #[cfg(feature = "scripting")]
    script_states: HashMap<String, ScriptMarkerState>,
}

impl Markers {
    pub fn new() -> Self {
        Self::default()
    }

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
            origin: MarkerOrigin::Manual,
        });
        id
    }

    pub fn push_loaded(&mut self, t_us: i64, label: String, color: [f32; 4], note: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Marker {
            id,
            t_us,
            label,
            color,
            note,
            origin: MarkerOrigin::Manual,
        });
    }

    #[cfg(feature = "scripting")]
    pub fn apply_script_command(&mut self, command: MarkerCommand) {
        match command {
            MarkerCommand::Replace {
                owner,
                generation,
                markers,
            } => {
                self.remove_script_markers(&owner);
                let mut state = ScriptMarkerState {
                    generation,
                    next_palette_index: 0,
                };
                self.insert_script_markers(&owner, generation, markers, &mut state);
                self.script_states.insert(owner, state);
            }
            MarkerCommand::Append {
                owner,
                generation,
                markers,
            } => {
                let mut state = match self.script_states.get(&owner).copied() {
                    Some(state) if generation < state.generation => return,
                    Some(mut state) => {
                        state.generation = generation;
                        state
                    }
                    None => ScriptMarkerState {
                        generation,
                        next_palette_index: 0,
                    },
                };
                self.insert_script_markers(&owner, generation, markers, &mut state);
                self.script_states.insert(owner, state);
            }
            MarkerCommand::Remove { owner } => {
                self.script_states.remove(&owner);
                self.remove_script_markers(&owner);
            }
        }
    }

    #[cfg(feature = "scripting")]
    fn insert_script_markers(
        &mut self,
        owner: &str,
        generation: u64,
        markers: Vec<PendingMarker>,
        state: &mut ScriptMarkerState,
    ) {
        for marker in markers {
            let id = self.next_id;
            self.next_id += 1;
            let color = marker.color.unwrap_or_else(|| {
                delog_render::palette::trace_color(state.next_palette_index).to_srgb_f32()
            });
            state.next_palette_index += 1;
            self.items.push(Marker {
                id,
                t_us: marker.time_us,
                label: marker.label,
                color,
                note: marker.note,
                origin: MarkerOrigin::Script {
                    owner: owner.to_owned(),
                    generation,
                },
            });
        }
    }

    #[cfg(feature = "scripting")]
    fn remove_script_markers(&mut self, owner: &str) {
        self.items.retain(|marker| {
            !matches!(&marker.origin, MarkerOrigin::Script { owner: marker_owner, .. } if marker_owner == owner)
        });
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|m| m.id != id);
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Marker> {
        self.items.iter_mut().find(|m| m.id == id)
    }

    pub fn by_time(&self) -> Vec<&Marker> {
        let mut v: Vec<&Marker> = self.items.iter().collect();
        v.sort_by_key(|m| m.t_us);
        v
    }

    pub fn as_slice(&self) -> &[Marker] {
        &self.items
    }
}

#[derive(Default)]
pub struct MarkersDock {
    pub open: bool,
    selected: HashSet<u64>,
}

impl MarkersDock {
    pub fn ui(&mut self, ui: &mut egui::Ui, markers: &mut Markers, origin_us: i64) -> Option<i64> {
        let ids: Vec<u64> = markers.by_time().iter().map(|m| m.id).collect();
        self.selected.retain(|id| ids.contains(id));
        let selected_count = ids.iter().filter(|id| self.selected.contains(id)).count();
        let all_selected = !ids.is_empty() && selected_count == ids.len();
        let any_selected = selected_count > 0;

        let mut delete_selected = false;
        ui.horizontal(|ui| {
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
                let iw = ui.spacing().icon_width;
                ui.painter().hline(
                    egui::Rangef::new(resp.rect.left() + iw * 0.3, resp.rect.left() + iw * 0.7),
                    resp.rect.center().y,
                    egui::Stroke::new(2.0, ui.visuals().text_color()),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

        if delete_selected {
            for id in self.selected.drain() {
                markers.remove(id);
            }
        }

        let mut jump = None;
        let mut to_remove = None;
        // auto_shrink([false, false]) so the scroll area fills the dragged
        // height and the dock resize sticks (egui sizes the panel from content).
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
                            m.color = crate::plotting::legend::color32_to_srgb(color);
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
                        let jump_icon = egui::Image::new(crate::ui::icons::crosshair())
                            .fit_to_exact_size(icon_size)
                            .tint(ui.visuals().text_color());
                        if ui
                            .add(egui::Button::image(jump_icon))
                            .on_hover_text("Jump to marker")
                            .clicked()
                        {
                            jump = Some(m.t_us);
                        }
                        let delete_icon = egui::Image::new(crate::ui::icons::trash())
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

    #[cfg(feature = "scripting")]
    fn pending(time_us: i64, label: &str, color: Option<[f32; 4]>) -> PendingMarker {
        PendingMarker {
            time_us,
            label: label.into(),
            color,
            note: format!("note for {label}"),
        }
    }

    #[cfg(feature = "scripting")]
    fn labels(markers: &Markers) -> Vec<&str> {
        markers
            .as_slice()
            .iter()
            .map(|marker| marker.label.as_str())
            .collect()
    }

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

    #[test]
    #[cfg(feature = "scripting")]
    fn script_replace_preserves_manual_and_replaces_only_owner_generation() {
        let mut markers = Markers::new();
        markers.add_at(5);
        markers.push_loaded(6, "generated".into(), [0.1, 0.2, 0.3, 1.0], String::new());
        markers.apply_script_command(MarkerCommand::Replace {
            owner: "flight.py".into(),
            generation: 1,
            markers: vec![pending(10, "old", None)],
        });
        markers.apply_script_command(MarkerCommand::Replace {
            owner: "other.py".into(),
            generation: 1,
            markers: vec![pending(15, "other", None)],
        });
        markers.apply_script_command(MarkerCommand::Replace {
            owner: "flight.py".into(),
            generation: 2,
            markers: vec![pending(20, "new", None)],
        });

        assert_eq!(labels(&markers), ["Marker 1", "generated", "other", "new"]);
        assert_eq!(markers.as_slice()[0].origin, MarkerOrigin::Manual);
        assert_eq!(markers.as_slice()[1].origin, MarkerOrigin::Manual);
        assert_eq!(
            markers.as_slice()[3].origin,
            MarkerOrigin::Script {
                owner: "flight.py".into(),
                generation: 2,
            }
        );
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_append_rejects_lower_generation() {
        let mut markers = Markers::new();
        markers.apply_script_command(MarkerCommand::Append {
            owner: "console".into(),
            generation: 2,
            markers: vec![pending(10, "current", None)],
        });
        markers.apply_script_command(MarkerCommand::Append {
            owner: "console".into(),
            generation: 1,
            markers: vec![pending(20, "stale", None)],
        });

        assert_eq!(labels(&markers), ["current"]);
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_append_accumulates_at_equal_generation() {
        let mut markers = Markers::new();
        for label in ["first", "second"] {
            markers.apply_script_command(MarkerCommand::Append {
                owner: "console".into(),
                generation: 3,
                markers: vec![pending(10, label, None)],
            });
        }

        assert_eq!(labels(&markers), ["first", "second"]);
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_append_advances_generation_without_clearing_history() {
        let mut markers = Markers::new();
        markers.apply_script_command(MarkerCommand::Append {
            owner: "console".into(),
            generation: 1,
            markers: vec![pending(10, "history", None)],
        });
        markers.apply_script_command(MarkerCommand::Append {
            owner: "console".into(),
            generation: 2,
            markers: vec![pending(20, "latest", None)],
        });

        assert_eq!(labels(&markers), ["history", "latest"]);
        assert_eq!(
            markers.as_slice()[0].origin,
            MarkerOrigin::Script {
                owner: "console".into(),
                generation: 1,
            }
        );
        assert_eq!(
            markers.as_slice()[1].origin,
            MarkerOrigin::Script {
                owner: "console".into(),
                generation: 2,
            }
        );
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_remove_deletes_only_requested_owner() {
        let mut markers = Markers::new();
        markers.add_at(1);
        for owner in ["one", "two"] {
            markers.apply_script_command(MarkerCommand::Replace {
                owner: owner.into(),
                generation: 1,
                markers: vec![pending(10, owner, None)],
            });
        }
        markers.apply_script_command(MarkerCommand::Remove {
            owner: "one".into(),
        });

        assert_eq!(labels(&markers), ["Marker 1", "two"]);
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_markers_keep_duplicate_timestamps_and_explicit_colors() {
        let explicit = [0.1, 0.2, 0.3, 0.4];
        let mut markers = Markers::new();
        markers.apply_script_command(MarkerCommand::Replace {
            owner: "flight.py".into(),
            generation: 1,
            markers: vec![pending(42, "a", Some(explicit)), pending(42, "b", None)],
        });

        assert_eq!(markers.as_slice().len(), 2);
        assert_eq!(markers.as_slice()[0].t_us, 42);
        assert_eq!(markers.as_slice()[1].t_us, 42);
        assert_eq!(markers.as_slice()[0].color, explicit);
        assert_eq!(
            markers.as_slice()[1].color,
            delog_render::palette::trace_color(1).to_srgb_f32()
        );
    }

    #[test]
    #[cfg(feature = "scripting")]
    fn script_replace_resets_automatic_palette_ordinal() {
        let explicit = [0.9, 0.8, 0.7, 0.6];
        let mut markers = Markers::new();
        markers.apply_script_command(MarkerCommand::Replace {
            owner: "flight.py".into(),
            generation: 1,
            markers: vec![
                pending(1, "colored", Some(explicit)),
                pending(2, "auto", None),
            ],
        });
        assert_eq!(markers.as_slice()[0].color, explicit);
        assert_eq!(
            markers.as_slice()[1].color,
            delog_render::palette::trace_color(1).to_srgb_f32()
        );

        markers.apply_script_command(MarkerCommand::Replace {
            owner: "flight.py".into(),
            generation: 2,
            markers: vec![pending(3, "reset", None)],
        });
        assert_eq!(
            markers.as_slice()[0].color,
            delog_render::palette::trace_color(0).to_srgb_f32()
        );
    }
}

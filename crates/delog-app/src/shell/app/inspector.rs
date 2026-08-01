use std::sync::Arc;

use delog_core::field_view::{FieldView, SampleValue};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::plotting::field_stats::{FieldStatsController, StatsTab};
use crate::plotting::plot::{TraceMode, TraceRef};

#[derive(Debug, Clone)]
pub enum InspectorEvent {
    SetTraceColor {
        tile_id: egui_tiles::TileId,
        field: FieldId,
        color: egui::Color32,
    },
    SetTraceMode {
        tile_id: egui_tiles::TileId,
        field: FieldId,
        mode: TraceMode,
    },
    SetTraceWidth {
        tile_id: egui_tiles::TileId,
        field: FieldId,
        width_px: f32,
    },
    SetTraceLabel {
        tile_id: egui_tiles::TileId,
        field: FieldId,
        label: Option<String>,
    },
}

pub fn normalize_trace_width(width_px: f32) -> f32 {
    width_px.clamp(1.0, 12.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorSelection {
    Summary,
    Cursor {
        readout: CursorReadout,
    },
    Statistics(Vec<FieldId>),
    Trace {
        tile_id: egui_tiles::TileId,
        field: FieldId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorReadout {
    Hover {
        tile_id: egui_tiles::TileId,
        t_us: i64,
    },
    Playhead {
        tile_id: Option<egui_tiles::TileId>,
        t_us: i64,
    },
}

impl CursorReadout {
    pub const fn tile_id(self) -> Option<egui_tiles::TileId> {
        match self {
            Self::Hover { tile_id, .. } => Some(tile_id),
            Self::Playhead { tile_id, .. } => tile_id,
        }
    }

    pub const fn t_us(self) -> i64 {
        match self {
            Self::Hover { t_us, .. } | Self::Playhead { t_us, .. } => t_us,
        }
    }

    pub const fn time_label(self) -> &'static str {
        match self {
            Self::Hover { .. } => "Cursor time",
            Self::Playhead { .. } => "Playhead time",
        }
    }
}

pub const fn cursor_readout_for_frame(
    hover: Option<(egui_tiles::TileId, i64)>,
    playback_us: i64,
    focused_tile: Option<egui_tiles::TileId>,
) -> CursorReadout {
    match hover {
        Some((tile_id, t_us)) => CursorReadout::Hover { tile_id, t_us },
        None => CursorReadout::Playhead {
            tile_id: focused_tile,
            t_us: playback_us,
        },
    }
}

pub struct InspectorState {
    pub open: bool,
    pub selection: InspectorSelection,
}

impl Default for InspectorState {
    fn default() -> Self {
        Self {
            open: true,
            selection: InspectorSelection::Summary,
        }
    }
}

impl InspectorState {
    pub fn focus_statistics(&mut self, fields: Vec<FieldId>) {
        self.open = true;
        self.selection = InspectorSelection::Statistics(fields);
    }

    pub fn focus_trace(&mut self, tile_id: egui_tiles::TileId, field: FieldId) {
        self.open = true;
        self.selection = InspectorSelection::Trace { tile_id, field };
    }

    pub fn update_cursor_readout(&mut self, readout: CursorReadout) -> bool {
        if let InspectorSelection::Cursor {
            readout: current,
        } = &mut self.selection
        {
            if *current == readout {
                return false;
            }
            *current = readout;
            return true;
        }
        false
    }
}

pub fn inspector_action_for_stats(fields: Vec<FieldId>) -> InspectorSelection {
    InspectorSelection::Statistics(fields)
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut InspectorState,
    snapshot: &Arc<StoreSnapshot>,
    playback_us: i64,
    hover_mode: delog_core::field_view::SampleMode,
    cursor_fields: &[FieldId],
    focused_tile: Option<egui_tiles::TileId>,
    diagnostic_count: usize,
    stats: &mut FieldStatsController,
    inspected_trace: Option<&TraceRef>,
    live_summaries: &[super::context_header::LiveSummary],
) -> Vec<InspectorEvent> {
    let mut events = Vec::new();
    ui.horizontal(|ui| {
        crate::ui::components::panel_header(ui, "Inspector");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::close(),
                "Close Inspector",
                false,
            )
            .clicked()
            {
                state.open = false;
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(
                matches!(state.selection, InspectorSelection::Summary),
                "Summary",
            )
            .clicked()
        {
            state.selection = InspectorSelection::Summary;
        }
        if ui
            .selectable_label(
                matches!(state.selection, InspectorSelection::Cursor { .. }),
                "Cursor",
            )
            .clicked()
        {
            state.selection = InspectorSelection::Cursor {
                readout: cursor_readout_for_frame(None, playback_us, focused_tile),
            };
        }
        if !stats.fields().is_empty()
            && ui
                .selectable_label(
                    matches!(state.selection, InspectorSelection::Statistics(_)),
                    "Statistics",
                )
                .clicked()
        {
            state.selection = inspector_action_for_stats(stats.fields().to_vec());
        }
    });
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| match &state.selection {
        InspectorSelection::Summary => summary_ui(ui, snapshot, diagnostic_count, live_summaries),
        InspectorSelection::Cursor { readout } => {
            cursor_ui(ui, snapshot, *readout, hover_mode, cursor_fields)
        }
        InspectorSelection::Statistics(fields) => statistics_ui(ui, snapshot, fields, stats),
        InspectorSelection::Trace { tile_id, field } => {
            ui.weak(format!("Pane {tile_id:?}"));
            trace_inspector_ui(ui, snapshot, *tile_id, *field, inspected_trace, &mut events);
            ui.separator();
            trace_ui(
                ui,
                snapshot,
                *field,
                playback_us,
                hover_mode,
                "No sample at playhead",
            );
        }
    });
    events
}

fn trace_inspector_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    tile_id: egui_tiles::TileId,
    field: FieldId,
    trace: Option<&TraceRef>,
    events: &mut Vec<InspectorEvent>,
) {
    let Some(trace) = trace else {
        ui.weak("This trace is no longer available.");
        return;
    };
    ui.strong(crate::plotting::legend::trace_label(snapshot, field));
    ui.horizontal(|ui| {
        ui.label("Color");
        let mut color = trace.color32();
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut color,
            egui::color_picker::Alpha::Opaque,
        )
        .changed()
        {
            events.push(InspectorEvent::SetTraceColor {
                tile_id,
                field,
                color,
            });
        }
    });
    ui.horizontal(|ui| {
        ui.label("Mode");
        let mut mode = trace.mode;
        for candidate in TraceMode::ALL {
            ui.radio_value(&mut mode, candidate, candidate.label());
        }
        if mode != trace.mode {
            events.push(InspectorEvent::SetTraceMode {
                tile_id,
                field,
                mode,
            });
        }
    });
    let mut width_px = trace.width_px;
    if ui
        .add(
            egui::Slider::new(&mut width_px, 1.0..=12.0)
                .text("Width")
                .suffix(" px"),
        )
        .changed()
    {
        events.push(InspectorEvent::SetTraceWidth {
            tile_id,
            field,
            width_px: normalize_trace_width(width_px),
        });
    }
    let mut label = trace.label_override.clone().unwrap_or_default();
    if ui
        .add(egui::TextEdit::singleline(&mut label).hint_text("Default trace label"))
        .changed()
    {
        let label = (!label.trim().is_empty()).then_some(label);
        events.push(InspectorEvent::SetTraceLabel {
            tile_id,
            field,
            label,
        });
    }
}

fn summary_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    diagnostic_count: usize,
    live_summaries: &[super::context_header::LiveSummary],
) {
    let sources: Vec<_> = snapshot
        .sources
        .iter()
        .filter(|source| !source.entry.removed)
        .collect();
    property(ui, "Sources", sources.len().to_string());
    property(
        ui,
        "Topics",
        snapshot
            .topics
            .iter()
            .filter(|topic| !topic.entry.removed)
            .count()
            .to_string(),
    );
    property(
        ui,
        "Fields",
        snapshot
            .fields
            .iter()
            .filter(|field| !field.removed)
            .count()
            .to_string(),
    );
    property(ui, "Diagnostics", diagnostic_count.to_string());
    if let Some(range) = snapshot.global_time_range() {
        property(ui, "Start", super::format_time_us(range.min_us));
        property(ui, "End", super::format_time_us(range.max_us));
        property(
            ui,
            "Duration",
            super::format_time_us(range.max_us.saturating_sub(range.min_us)),
        );
    } else {
        ui.weak("No source data loaded");
    }
    if !sources.is_empty() {
        ui.separator();
        ui.strong("Formats");
        for source in sources {
            ui.horizontal(|ui| {
                ui.label(&source.entry.label);
                ui.weak(super::source_kind_label(&source.entry.label));
            });
        }
    }
    if !live_summaries.is_empty() {
        ui.separator();
        ui.strong("Live connections");
        for live in live_summaries {
            ui.group(|ui| {
                property(ui, "Endpoint", &live.endpoint);
                property(ui, "State", &live.state);
                property(ui, "Frames", live.rx_frames.to_string());
                property(ui, "Rows", live.rows.to_string());
                if let Some(recording) = &live.recording {
                    property(ui, "Recording", recording);
                }
            });
        }
    }
}

fn cursor_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    readout: CursorReadout,
    mode: delog_core::field_view::SampleMode,
    fields: &[FieldId],
) {
    property(ui, readout.time_label(), super::format_time_us(readout.t_us()));
    if fields.is_empty() {
        match readout {
            CursorReadout::Hover { .. } => ui.weak("The hovered plot has no visible traces."),
            CursorReadout::Playhead { .. } => {
                ui.weak("Focus a plot to inspect values at the playhead.")
            }
        };
        return;
    }
    ui.separator();
    let missing = match readout {
        CursorReadout::Hover { .. } => "No sample at cursor",
        CursorReadout::Playhead { .. } => "No sample at playhead",
    };
    for field in fields {
        trace_ui(ui, snapshot, *field, readout.t_us(), mode, missing);
    }
}

fn trace_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    field: FieldId,
    t_us: i64,
    mode: delog_core::field_view::SampleMode,
    missing: &str,
) {
    ui.strong(crate::plotting::legend::trace_label(snapshot, field));
    let value = FieldView::new(snapshot, field).ok().and_then(|view| {
        view.sample_at(t_us, mode)
            .map(|sample| format_sample(sample.value, snapshot, field))
    });
    match value {
        Some(value) => ui.monospace(value),
        None => ui.weak(missing),
    };
}

fn format_sample(value: SampleValue<'_>, snapshot: &StoreSnapshot, field: FieldId) -> String {
    if let Some(value) = value.as_f64() {
        let meta = super::field_metadata(snapshot, field);
        let scaled = value * meta.as_ref().map_or(1.0, |meta| meta.multiplier);
        let unit = meta.and_then(|meta| meta.unit).unwrap_or_default();
        return format!("{} {unit}", super::format_stat(scaled));
    }
    match value {
        SampleValue::Bool(value) => value.to_string(),
        SampleValue::Utf8(value) => value.to_owned(),
        SampleValue::Null => "—".to_owned(),
        SampleValue::Int(_) | SampleValue::UInt(_) | SampleValue::Float(_) => "—".to_owned(),
    }
}

fn statistics_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    fields: &[FieldId],
    controller: &mut FieldStatsController,
) {
    ui.horizontal(|ui| {
        for tab in StatsTab::ALL {
            if ui
                .selectable_label(controller.tab() == tab, tab.label())
                .clicked()
            {
                controller.set_tab(tab);
            }
        }
        if controller.is_any_updating() {
            ui.spinner();
        }
    });
    ui.separator();
    for field in fields {
        ui.strong(crate::plotting::legend::trace_label(snapshot, *field));
        let global_result;
        let result = if controller.tab() == StatsTab::Global {
            global_result = delog_core::analysis::global_field_stats(snapshot, *field);
            match &global_result {
                Ok(Some(result)) => Some(result),
                Ok(None) => {
                    ui.weak("Not numeric");
                    continue;
                }
                Err(error) => {
                    ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    continue;
                }
            }
        } else {
            if let Some(error) = controller.error_for(*field) {
                ui.colored_label(ui.visuals().error_fg_color, error);
                continue;
            }
            controller
                .result_for(*field)
                .or_else(|| controller.stale_result_for(*field))
        };
        if let Some(result) = result {
            property(ui, "Min", super::format_stat(result.min));
            property(ui, "Max", super::format_stat(result.max));
            property(ui, "Mean", super::format_stat(result.mean));
            property(ui, "Std dev", super::format_stat(result.stddev));
            property(ui, "Samples", result.count.to_string());
            property(ui, "Missing", result.missing_count.to_string());
        } else {
            ui.weak("Calculating…");
        }
        ui.separator();
    }
}

fn property(ui: &mut egui::Ui, label: &str, value: impl ToString) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace(value.to_string());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_defaults_to_summary_and_can_be_closed_without_losing_selection() {
        let mut state = InspectorState::default();
        assert_eq!(state.selection, InspectorSelection::Summary);
        state.focus_statistics(vec![FieldId(7)]);
        state.open = false;
        assert_eq!(
            state.selection,
            InspectorSelection::Statistics(vec![FieldId(7)])
        );
    }

    #[test]
    fn field_stats_context_action_also_focuses_inspector() {
        let action = inspector_action_for_stats(vec![FieldId(1), FieldId(2)]);
        assert_eq!(
            action,
            InspectorSelection::Statistics(vec![FieldId(1), FieldId(2)])
        );
    }

    #[test]
    fn inspector_width_edit_clamps_to_existing_range() {
        assert_eq!(normalize_trace_width(0.5), 1.0);
        assert_eq!(normalize_trace_width(6.0), 6.0);
        assert_eq!(normalize_trace_width(20.0), 12.0);
    }

    #[test]
    fn hovered_pane_and_time_win_over_focused_pane_and_playhead() {
        let hovered = egui_tiles::TileId::from_u64(7);
        let focused = egui_tiles::TileId::from_u64(2);
        assert_eq!(
            cursor_readout_for_frame(Some((hovered, 42)), 11, Some(focused)),
            CursorReadout::Hover {
                tile_id: hovered,
                t_us: 42,
            }
        );
        assert_eq!(
            cursor_readout_for_frame(Some((hovered, 42)), 11, Some(focused)).time_label(),
            "Cursor time"
        );
    }

    #[test]
    fn no_hover_uses_a_distinctly_labelled_playhead_fallback() {
        let focused = egui_tiles::TileId::from_u64(2);
        let readout = cursor_readout_for_frame(None, 11, Some(focused));
        assert_eq!(
            readout,
            CursorReadout::Playhead {
                tile_id: Some(focused),
                t_us: 11,
            }
        );
        assert_eq!(readout.time_label(), "Playhead time");
    }

    #[test]
    fn cursor_refresh_does_not_replace_other_inspector_selections() {
        let mut state = InspectorState::default();
        state.focus_statistics(vec![FieldId(9)]);
        let changed = state.update_cursor_readout(CursorReadout::Playhead {
            tile_id: None,
            t_us: 77,
        });
        assert!(!changed);
        assert_eq!(
            state.selection,
            InspectorSelection::Statistics(vec![FieldId(9)])
        );
    }

    #[test]
    fn cursor_refresh_reports_only_real_readout_changes() {
        let tile_id = egui_tiles::TileId::from_u64(3);
        let readout = CursorReadout::Hover { tile_id, t_us: 42 };
        let mut state = InspectorState {
            open: true,
            selection: InspectorSelection::Cursor {
                readout: CursorReadout::Playhead {
                    tile_id: Some(tile_id),
                    t_us: 11,
                },
            },
        };

        assert!(state.update_cursor_readout(readout));
        assert!(!state.update_cursor_readout(readout));
    }
}

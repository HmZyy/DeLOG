use std::sync::Arc;

use delog_core::field_view::{FieldView, SampleMode, SampleValue};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use egui_extras::{Column, TableBuilder};

use crate::shell::workspace::InspectorTrace;

pub struct InspectorState {
    pub open: bool,
}

impl Default for InspectorState {
    fn default() -> Self {
        Self { open: false }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut InspectorState,
    snapshot: &Arc<StoreSnapshot>,
    playhead_us: Option<i64>,
    marker_us: Option<i64>,
    sample_mode: SampleMode,
    traces: &[InspectorTrace],
) {
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
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        time_summary_ui(ui, snapshot, playhead_us, marker_us);
        ui.separator();
        ui.strong("Traces");
        if traces.is_empty() {
            ui.weak("No visible traces");
            return;
        }

        ui.add_space(ui.spacing().item_spacing.y);
        trace_table_ui(ui, snapshot, traces, playhead_us, marker_us, sample_mode);
    });
}

fn time_summary_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    playhead_us: Option<i64>,
    marker_us: Option<i64>,
) {
    let range = snapshot.global_time_range();
    property(
        ui,
        "Start",
        range
            .map(|range| super::format_time_us(range.min_us))
            .unwrap_or_else(|| "--".to_owned()),
    );
    property(
        ui,
        "End",
        range
            .map(|range| super::format_time_us(range.max_us))
            .unwrap_or_else(|| "--".to_owned()),
    );
    property(
        ui,
        "Duration",
        range
            .map(|range| super::format_time_us(range.max_us.saturating_sub(range.min_us)))
            .unwrap_or_else(|| "--".to_owned()),
    );
    property(
        ui,
        "Playhead time",
        playhead_us
            .map(super::format_time_us)
            .unwrap_or_else(|| "--".to_owned()),
    );
    if marker_us.is_some() {
        property(
            ui,
            "Δt",
            marker_us
                .zip(playhead_us)
                .map(|(marker, playhead)| format!("{:+.3} s", (marker - playhead) as f64 * 1e-6))
                .unwrap_or_else(|| "--".to_owned()),
        );
    }
}

fn trace_table_ui(
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    traces: &[InspectorTrace],
    playhead_us: Option<i64>,
    marker_us: Option<i64>,
    sample_mode: SampleMode,
) {
    let row_height = ui.spacing().interact_size.y;
    let mut table = TableBuilder::new(ui)
        .id_salt("inspector-traces-table")
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .auto_shrink([false, true])
        .column(Column::remainder().clip(true))
        .column(Column::auto().at_least(72.0));
    if marker_us.is_some() {
        table = table.column(Column::auto().at_least(72.0));
    }
    table
        .header(row_height, |mut header| {
            header.col(|ui| {
                ui.strong("Trace");
            });
            header.col(|ui| {
                ui.strong("Value");
            });
            if marker_us.is_some() {
                header.col(|ui| {
                    ui.strong("Δ");
                });
            }
        })
        .body(|body| {
            body.rows(row_height, traces.len(), |mut row| {
                let trace = &traces[row.index()];
                row.col(|ui| {
                    ui.horizontal(|ui| {
                        color_swatch(ui, trace.color);
                        ui.label(&trace.label);
                    });
                });
                let readout =
                    trace_readout(snapshot, trace.field, playhead_us, marker_us, sample_mode);
                row.col(|ui| {
                    ui.monospace(readout.value);
                });
                if marker_us.is_some() {
                    row.col(|ui| {
                        ui.monospace(readout.delta.unwrap_or_else(|| "--".to_owned()));
                    });
                }
            });
        });
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceReadout {
    value: String,
    delta: Option<String>,
}

fn trace_readout(
    snapshot: &StoreSnapshot,
    field: FieldId,
    playhead_us: Option<i64>,
    marker_us: Option<i64>,
    mode: SampleMode,
) -> TraceReadout {
    let value = playhead_us
        .and_then(|playhead_us| {
            FieldView::new(snapshot, field).ok().and_then(|view| {
                view.sample_at(playhead_us, mode)
                    .map(|sample| format_sample(sample.value, snapshot, field))
            })
        })
        .unwrap_or_else(|| "--".to_owned());
    let delta = marker_us
        .zip(playhead_us)
        .and_then(|(marker_us, playhead_us)| {
            crate::plotting::hover::marker_delta_for_field(
                snapshot,
                field,
                marker_us,
                playhead_us,
                mode,
            )
        });
    TraceReadout { value, delta }
}

fn format_sample(value: SampleValue<'_>, snapshot: &StoreSnapshot, field: FieldId) -> String {
    if let Some(value) = value.as_f64() {
        let meta = super::field_metadata(snapshot, field);
        let scaled = value * meta.as_ref().map_or(1.0, |meta| meta.multiplier);
        let unit = meta.and_then(|meta| meta.unit).unwrap_or_default();
        let value = super::format_stat(scaled);
        return if unit.is_empty() {
            value
        } else {
            format!("{value} {unit}")
        };
    }
    match value {
        SampleValue::Bool(value) => value.to_string(),
        SampleValue::Utf8(value) => value.to_owned(),
        SampleValue::Null | SampleValue::Int(_) | SampleValue::UInt(_) | SampleValue::Float(_) => {
            "--".to_owned()
        }
    }
}

fn color_swatch(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
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
    use crate::shell::workspace::InspectorTrace;

    #[test]
    fn inspector_starts_closed() {
        assert!(!InspectorState::default().open);
    }

    fn find_text_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_text_rect(shape, expected)),
            _ => None,
        }
    }

    fn numeric_snapshot() -> (Arc<StoreSnapshot>, FieldId) {
        use arrow::array::{ArrayRef, Float64Array, Int64Array};
        use arrow::datatypes::DataType;
        use delog_core::chunk::Chunk;
        use delog_core::schema::{FieldSchema, TopicSchema};
        use delog_core::store::TopicStore;

        let mut identity = delog_core::identity::IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "ATT").unwrap();
        let field = identity.add_field(topic, "Roll").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "ATT",
                [FieldSchema::new("Roll", DataType::Float64, Some("deg"), 0.5).unwrap()],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![2.0, 8.0]))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), columns, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot =
            Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());
        (snapshot, field)
    }

    fn inspector_frame(
        ctx: &egui::Context,
        state: &mut InspectorState,
        snapshot: &Arc<StoreSnapshot>,
        playhead_us: Option<i64>,
        marker_us: Option<i64>,
        traces: &[InspectorTrace],
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 400.0),
                )),
                ..Default::default()
            },
            |ui| {
                show(
                    ui,
                    state,
                    snapshot,
                    playhead_us,
                    marker_us,
                    SampleMode::Prev,
                    traces,
                );
            },
        )
    }

    fn painted(output: &egui::FullOutput, expected: &str) -> bool {
        output
            .shapes
            .iter()
            .any(|shape| find_text_rect(&shape.shape, expected).is_some())
    }

    #[test]
    fn single_view_omits_unused_cursor_time() {
        let snapshot = Arc::new(StoreSnapshot::empty());
        let ctx = egui::Context::default();
        let mut state = InspectorState::default();

        let output = inspector_frame(&ctx, &mut state, &snapshot, None, None, &[]);

        for required in ["Start", "End", "Duration", "Playhead time"] {
            assert!(painted(&output, required), "missing {required}");
        }
        for removed in [
            "Summary",
            "Cursor",
            "Statistics",
            "Sources",
            "Topics",
            "Fields",
            "Diagnostics",
            "Cursor time",
        ] {
            assert!(
                !painted(&output, removed),
                "obsolete inspector row remains: {removed}"
            );
        }
        assert!(painted(&output, "No visible traces"));
    }

    #[test]
    fn trace_readout_uses_playhead_value_and_marker_minus_playhead_delta() {
        let (snapshot, field) = numeric_snapshot();

        let readout = trace_readout(&snapshot, field, Some(20), Some(10), SampleMode::Prev);

        assert_eq!(readout.value, "4.0000 deg");
        assert_eq!(readout.delta.as_deref(), Some("-3.0000 deg"));
    }

    #[test]
    fn marker_renders_one_trace_table_with_delta_column() {
        let (snapshot, field) = numeric_snapshot();
        let traces = [
            InspectorTrace {
                field,
                label: "ATT.Roll".to_owned(),
                color: egui::Color32::RED,
            },
            InspectorTrace {
                field,
                label: "Bank duplicate".to_owned(),
                color: egui::Color32::BLUE,
            },
        ];
        let ctx = egui::Context::default();
        let mut state = InspectorState::default();

        let output = inspector_frame(&ctx, &mut state, &snapshot, Some(20), Some(10), &traces);

        assert!(!painted(&output, "Plot 1"));
        assert!(painted(&output, "ATT.Roll"));
        assert!(painted(&output, "Bank duplicate"));
        assert!(painted(&output, "4.0000 deg"));
        assert!(painted(&output, "-3.0000 deg"));
        assert!(painted(&output, "Δt"));
        assert!(painted(&output, "-0.000 s"));
    }
}

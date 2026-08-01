use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int32Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::chunk::Chunk;
use delog_core::diagnostics::{Diag, DiagRecord};
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicProvenance, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use super::*;

#[test]
fn active_native_loads_schedule_reactive_mode_polling() {
    let ctx = egui::Context::default();
    let (repaints, repaint_requests) = mpsc::channel();
    ctx.set_request_repaint_callback(move |request| {
        repaints.send(request.delay).unwrap();
    });

    keep_active_loads_repainting(&ctx, false);
    assert!(repaint_requests.try_recv().is_err());
    keep_active_loads_repainting(&ctx, true);

    let delay = repaint_requests
        .recv_timeout(Duration::from_secs(1))
        .expect("an active parser schedules another UI poll");
    assert!(delay <= Duration::from_millis(50));
}

#[test]
fn auto_open_diagnostics_only_fires_for_newer_seqs_when_enabled() {
    // First diagnostic ever seen opens the dock.
    assert!(should_auto_open_diagnostics(true, None, 0));
    // A strictly newer seq opens it again.
    assert!(should_auto_open_diagnostics(true, Some(3), 4));
    // The same (or older) seq does not - avoids reopening after the user closes.
    assert!(!should_auto_open_diagnostics(true, Some(4), 4));
    assert!(!should_auto_open_diagnostics(true, Some(5), 4));
    // Disabled never opens, even for a brand-new diagnostic.
    assert!(!should_auto_open_diagnostics(false, None, 0));
    assert!(!should_auto_open_diagnostics(false, Some(3), 9));
}

#[test]
fn combined_load_state_keeps_parser_label_separate_without_duplicates() {
    let state = combined_load_state(
        true,
        vec![
            "flight.bin".to_owned(),
            "running raw.py on flight.bin".to_owned(),
        ],
        Some("running raw.py on flight.bin"),
    );

    assert_eq!(state.native_labels, vec!["flight.bin"]);
    assert_eq!(
        state.parser_label.as_deref(),
        Some("running raw.py on flight.bin")
    );
    assert!(state.parser_active);
}

#[test]
fn combined_load_state_is_active_for_parser_only_work() {
    let state = combined_load_state(false, Vec::new(), Some("running raw.py on sample.dat"));

    assert!(state.active);
    assert!(state.native_labels.is_empty());
    assert_eq!(
        state.parser_label.as_deref(),
        Some("running raw.py on sample.dat")
    );
    assert!(state.parser_active);
}

#[test]
fn timeline_range_uses_empty_session_placeholder_without_data() {
    assert_eq!(
        timeline_range_for_ui(None),
        TimeRange::new(0, 10_000_000).unwrap()
    );
}

#[test]
fn fit_to_view_defaults_on_for_new_sessions() {
    assert!(DEFAULT_FIT_VIEW_ALL);
}

#[test]
fn clear_current_layout_resets_layout_and_vehicle_state() {
    let mut workspace = Workspace::new();
    let mut playback = Playback {
        speed: 2.0,
        follow_live: true,
        ..Playback::default()
    };
    let mut view = Some(ViewX::new(10, 20));
    let mut view_fitted = true;
    let mut fit_view_all = false;
    let mut marker_us = Some(42);
    let mut markers = crate::plotting::markers::Markers::new();
    markers.add_at(42);
    let mut vehicles = vec![crate::scene3d::vehicle::VehicleConfig {
        source: delog_core::identity::SourceId(0),
        label: "Vehicle".into(),
        show: true,
        pos: crate::scene3d::vehicle::PosMapping::Ned {
            north: delog_core::identity::FieldId(0),
            east: delog_core::identity::FieldId(1),
            down: delog_core::identity::FieldId(2),
            reference: None,
        },
        ori: crate::scene3d::vehicle::OriMapping::Static,
        model: crate::scene3d::vehicle::ModelKind::Cone,
        color: egui::Color32::WHITE,
        path_color: egui::Color32::WHITE,
        scale: 1.0,
    }];
    let mut vehicle_dialog = crate::session::vehicle_dialog::VehicleDialog::default();
    vehicle_dialog.open = true;
    let mut vehicle_revision = 7;
    let mut traj_dirty = false;

    DelogApp::clear_current_layout_state(
        &mut workspace,
        &mut playback,
        &mut view,
        &mut view_fitted,
        &mut fit_view_all,
        &mut marker_us,
        &mut markers,
        &mut vehicles,
        &mut vehicle_dialog,
        &mut vehicle_revision,
        &mut traj_dirty,
    );

    assert!(workspace.focused_first_field().is_none());
    assert_eq!(playback.speed, 1.0);
    assert!(!playback.follow_live);
    assert_eq!(view, None);
    assert!(!view_fitted);
    assert_eq!(fit_view_all, DEFAULT_FIT_VIEW_ALL);
    assert_eq!(marker_us, None);
    assert!(markers.as_slice().is_empty());
    assert!(vehicles.is_empty());
    assert!(!vehicle_dialog.open);
    assert_eq!(vehicle_revision, 8);
    assert!(traj_dirty);
}

#[test]
fn empty_stat_formats_as_a_dash() {
    assert_eq!(format_stat(f64::NAN), "-");
}

#[test]
fn file_menu_opens_data_export_through_resetting_api() {
    let source = include_str!("mod.rs");
    let export_action = source
        .split("if ui.button(\"Export Data\").clicked()")
        .nth(1)
        .expect("Export submenu should expose data export")
        .split("ui.separator();")
        .next()
        .expect("data export should precede the File menu separator");

    assert!(export_action.contains("self.data_export.open();"));
    assert!(!export_action.contains("self.data_export.open = true;"));
}

#[test]
fn field_stats_tabs_use_egui_dock() {
    let source = include_str!("mod.rs");
    let field_stats_window = source
        .split("fn show_field_stats_window")
        .nth(1)
        .expect("field stats window should exist")
        .split("fn stats_grid")
        .next()
        .expect("field stats window should precede stats grid");

    assert!(field_stats_window.contains("egui_dock::DockArea::new"));
    assert!(source.contains("impl egui_dock::TabViewer for FieldStatsTabViewer"));
    assert!(!field_stats_window.contains("selectable_label"));
}

#[test]
fn field_stats_window_is_a_resizable_multi_field_table() {
    let source = include_str!("mod.rs");
    let field_stats = source
        .split("fn show_field_stats_window")
        .nth(1)
        .expect("field stats window should exist")
        .split("fn field_time_range")
        .next()
        .expect("field stats helpers should precede field range helper");

    let window_constructor = ["egui::Window", "::new(\"Field stats\")"].concat();
    assert!(field_stats.contains(&window_constructor));
    assert!(field_stats.contains(".default_width(900.0)"));
    assert!(field_stats.contains(".resizable(true)"));
    assert!(field_stats.contains("fn stats_table"));
    assert!(field_stats.contains("ScrollArea::horizontal"));
    for heading in [
        "Name", "Samples", "Min", "Max", "Mean", "Std dev", "Missing", "Rate",
    ] {
        assert!(field_stats.contains(&format!("ui.strong(\"{heading}\")")));
    }
}

#[test]
fn field_stats_rows_use_topic_dot_field_labels_and_per_field_state() {
    let source = include_str!("mod.rs");
    let rows = source
        .split("fn field_stats_rows")
        .nth(1)
        .expect("field stats row builder should exist")
        .split("fn stats_table")
        .next()
        .expect("row builder should precede table renderer");

    assert!(rows.contains("crate::plotting::legend::trace_label(snapshot, field)"));
    assert!(rows.contains(".result_for(field)"));
    assert!(rows.contains(".error_for(field)"));
}

#[test]
fn diagnostics_export_doc_includes_source_labels_and_counts() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let snapshot = StoreSnapshot::from_registry(&identity, [], 7).unwrap();
    let doc = diagnostics_export_doc(
        vec![DiagRecord {
            seq: 42,
            diag: Diag::warning("ulog-dropout", "dropout")
                .with_source(source)
                .at_time(1_000_000)
                .at_byte(99),
            count: 3,
        }],
        &snapshot,
    );

    let json = serde_json::to_value(&doc).unwrap();
    let record = &json["records"][0];
    assert_eq!(json["delog_diagnostics"], 1);
    assert_eq!(record["seq"], 42);
    assert_eq!(record["count"], 3);
    assert_eq!(record["severity"], "warning");
    assert_eq!(record["code"], "ulog-dropout");
    assert_eq!(record["source_id"], source.0);
    assert_eq!(record["source_label"], "flight");
    assert_eq!(record["time_us"], 1_000_000);
    assert_eq!(record["byte_offset"], 99);
    assert_eq!(record["message"], "dropout");
}

#[test]
fn profiling_export_doc_carries_metrics_resources_and_traces() {
    let metrics = delog_core::metrics::MetricsRegistry::new();
    metrics.record("upload_bytes", 4096.0);
    metrics.add("gpu_full_uploads", 2);
    let snapshot = PerformanceSnapshot {
        metrics: metrics.snapshot(),
        resources: ResourceSummary {
            gpu_buffer_count: 3,
            gpu_bytes: 1024,
            cache_ready_count: 1,
            cache_cpu_bytes: 2048,
        },
        traces: vec![TraceSummary {
            label: "GPS.alt".into(),
            samples: Some(1000),
            visible_samples: Some(500),
            cache_cpu_bytes: 8000,
            gpu_bytes: 8000,
        }],
    };

    let doc = profiling_export_doc(&snapshot, 123);
    let json = serde_json::to_value(&doc).unwrap();

    assert_eq!(json["delog_profiling"], 1);
    assert_eq!(json["exported_at_unix_ms"], 123);
    assert_eq!(json["resources"]["gpu_buffer_count"], 3);
    assert_eq!(json["resources"]["cache_cpu_bytes"], 2048);

    // Metrics come through sorted by name (snapshot() guarantees it).
    let names: Vec<&str> = json["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["gpu_full_uploads", "upload_bytes"]);
    let upload = &json["metrics"][1];
    assert_eq!(upload["name"], "upload_bytes");
    assert_eq!(upload["last"], 4096.0);
    let full = &json["metrics"][0];
    assert_eq!(full["counter"], 2);

    assert_eq!(json["traces"][0]["label"], "GPS.alt");
    assert_eq!(json["traces"][0]["visible_samples"], 500);
}

#[test]
fn field_metadata_includes_schema_rows_and_effective_range() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    identity.set_source_offset_us(source, 250);
    let topic = identity.add_topic(source, "GPS").unwrap();
    let lat = identity.add_field(topic, "Lat").unwrap();
    identity.add_field(topic, "Alt").unwrap();

    let schema = Arc::new(
        TopicSchema::new(
            "GPS",
            [
                FieldSchema::new("Lat", DataType::Int32, Some("deg"), 1e-7)
                    .unwrap()
                    .with_description("latitude"),
                FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap(),
            ],
        )
        .unwrap()
        .with_provenance(TopicProvenance::new("flight-a", "ATT").unwrap()),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![1_000, 2_000, 3_000]),
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])) as ArrayRef,
                Arc::new(Float64Array::from(vec![10.0, 11.0, 12.0])) as ArrayRef,
            ],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 9).unwrap();

    let meta = field_metadata(&snapshot, lat).unwrap();

    assert_eq!(meta.title, "flight / GPS.Lat");
    assert_eq!(meta.source_label, "flight");
    assert_eq!(meta.topic_name, "GPS");
    assert_eq!(meta.original_source.as_deref(), Some("flight-a"));
    assert_eq!(meta.original_topic.as_deref(), Some("ATT"));
    assert_eq!(meta.field_name, "Lat");
    assert_eq!(meta.dtype, "i32");
    assert_eq!(meta.unit.as_deref(), Some("deg"));
    assert_eq!(meta.description.as_deref(), Some("latitude"));
    assert_eq!(meta.multiplier, 1e-7);
    assert_eq!(meta.rows, 3);
    assert_eq!(meta.source_offset_us, 250);
    assert_eq!(meta.range, TimeRange::new(1_250, 3_250));
}

#[test]
fn field_metadata_reports_original_sources_for_imported_topic_collisions() {
    let snapshot = crate::session::session::tests::structured_round_trip_snapshot();
    let metadata_for = |topic_name: &str| {
        let topic = snapshot
            .topics
            .iter()
            .find(|topic| topic.entry.name == topic_name)
            .unwrap();
        let roll = snapshot
            .fields
            .iter()
            .find(|field| field.topic == topic.entry.id && field.name == "Roll")
            .unwrap();
        field_metadata(&snapshot, roll.id).unwrap()
    };

    let primary = metadata_for("ATT[0]");
    assert_eq!(primary.source_label, "structured-metadata");
    assert_eq!(primary.topic_name, "ATT[0]");
    assert_eq!(primary.original_source.as_deref(), Some("flight-a"));
    assert_eq!(primary.original_topic.as_deref(), Some("ATT"));
    assert_eq!(primary.dtype, "f32");
    assert_eq!(primary.unit.as_deref(), Some("deg"));
    assert_eq!(primary.description.as_deref(), Some("roll angle"));
    assert_eq!(primary.multiplier, 0.01);
    assert_eq!(primary.rows, 2);
    assert_eq!(primary.source_offset_us, 0);
    assert_eq!(primary.range, TimeRange::new(1_100, 2_100));

    let secondary = metadata_for("ATT[1]");
    assert_eq!(secondary.source_label, "structured-metadata");
    assert_eq!(secondary.topic_name, "ATT[1]");
    assert_eq!(secondary.original_source.as_deref(), Some("flight-b"));
    assert_eq!(secondary.original_topic.as_deref(), Some("ATT"));
    assert_eq!(secondary.description.as_deref(), Some("secondary roll"));
    assert_eq!(secondary.rows, 3);
    assert_eq!(secondary.range, TimeRange::new(1_300, 3_300));
}

fn shape_contains_text(shape: &egui::epaint::Shape, expected: &str) -> bool {
    match shape {
        egui::epaint::Shape::Text(text) => text.galley.job.text == expected,
        egui::epaint::Shape::Vec(shapes) => shapes
            .iter()
            .any(|shape| shape_contains_text(shape, expected)),
        _ => false,
    }
}

#[test]
fn imported_provenance_is_rendered_in_the_existing_field_metadata_window() {
    let snapshot = crate::session::session::tests::structured_round_trip_snapshot();
    let topic = snapshot
        .topics
        .iter()
        .find(|topic| topic.entry.name == "ATT[0]")
        .unwrap();
    let roll = snapshot
        .fields
        .iter()
        .find(|field| field.topic == topic.entry.id && field.name == "Roll")
        .unwrap();
    let mut selected = Some(roll.id);
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_200.0, 800.0),
        )),
        ..Default::default()
    };

    let _ = ctx.run_ui(input.clone(), |ui| {
        show_field_metadata_window(ui.ctx(), &snapshot, &mut selected);
    });
    let output = ctx.run_ui(input, |ui| {
        show_field_metadata_window(ui.ctx(), &snapshot, &mut selected);
    });

    for expected in ["Original source", "flight-a", "Original topic", "ATT"] {
        assert!(
            output
                .shapes
                .iter()
                .any(|clipped| shape_contains_text(&clipped.shape, expected)),
            "field metadata window should render {expected:?}"
        );
    }
}

#[test]
fn provisional_visible_stats_reconstructs_absolute_minmax() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "BARO").unwrap();
    let field = identity.add_field(topic, "Alt").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "BARO",
            [FieldSchema::new("Alt", DataType::Int32, Some("cm"), 0.01).unwrap()],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![0, 1_000_000, 2_000_000]),
            vec![Arc::new(Int32Array::from(vec![10_000, 10_100, 10_200])) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
    let cache = delog_cache::TraceCache::build(
        &snapshot,
        field,
        0,
        0,
        &delog_core::metrics::MetricsRegistry::new(),
    )
    .unwrap();

    assert_eq!(
        provisional_visible_stats(&cache, ViewX::new(1_000_000, 2_000_000)),
        Some((101.0, 102.0))
    );
}

#[test]
fn source_metadata_tabs_use_egui_dock() {
    let source = include_str!("mod.rs");
    let source_metadata = source
        .split("fn show_source_metadata_window")
        .nth(1)
        .expect("source metadata window should exist")
        .split("fn show_source_metadata_tab")
        .next()
        .expect("source metadata window should precede tab renderer");

    assert!(source_metadata.contains("egui_dock::DockArea::new"));
    assert!(source.contains("impl egui_dock::TabViewer for SourceMetadataTabViewer"));
    assert!(!source_metadata.contains("selectable_value"));
}

#[test]
fn source_metadata_tables_use_resizable_table_builders() {
    let source = include_str!("mod.rs");
    let source_metadata = source
        .split("fn show_source_metadata_tab")
        .nth(1)
        .expect("source metadata tab renderer should exist")
        .split("fn show_field_stats_window")
        .next()
        .expect("source metadata should precede field stats");

    assert!(source_metadata.contains("source_metadata_summary_table"));
    assert!(source_metadata.contains("source_metadata_params_table"));
    assert!(source_metadata.contains("source_metadata_markers_table"));
    assert_eq!(source_metadata.matches("TableBuilder::new(ui)").count(), 3);
    assert_eq!(source_metadata.matches(".resizable(true)").count(), 3);
    assert!(source_metadata.matches("Column::remainder()").count() >= 3);
    assert!(!source_metadata.contains("egui::Grid::new"));
}

#[test]
fn tile_cache_repaints_on_clear_submission_and_while_action_is_pending() {
    assert!(tile_cache_needs_repaint(true, false));
    assert!(tile_cache_needs_repaint(false, true));
    assert!(!tile_cache_needs_repaint(false, false));
}

#[test]
fn keyboard_shortcuts_produce_registry_commands() {
    use crate::shell::app::commands::CommandId;

    assert_eq!(
        command_for_shortcut(egui::Key::Space, false),
        Some(CommandId::TogglePlayback)
    );
    assert_eq!(
        command_for_shortcut(egui::Key::S, true),
        Some(CommandId::SaveLayout)
    );
    assert_eq!(
        command_for_shortcut(egui::Key::L, true),
        Some(CommandId::LoadLayout)
    );
    assert_eq!(
        command_for_shortcut(egui::Key::M, false),
        Some(CommandId::AddMarker)
    );
    assert_eq!(command_for_shortcut(egui::Key::K, true), None);
}

use std::sync::Arc;

use arrow::datatypes::DataType;
use delog_core::identity::{IdentityRegistry, SourceId, SourceKind};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use super::*;

fn fixture_snapshot() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let mut stores = Vec::new();
    for label in ["a", "b", "c"] {
        let source = identity.add_source_with_kind(label, SourceKind::File);
        let topic = identity.add_topic(source, "DATA").unwrap();
        identity.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        stores.push((topic, Arc::new(TopicStore::new(schema))));
    }
    StoreSnapshot::from_registry(&identity, stores, 0).unwrap()
}

fn multi_source_fixture(series: &[(&str, &[i64], &[f64])]) -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let mut stores = Vec::new();
    for (label, times, values) in series {
        let source = identity.add_source_with_kind(*label, SourceKind::File);
        let topic = identity.add_topic(source, "DATA").unwrap();
        identity.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<arrow::array::ArrayRef> =
            vec![Arc::new(arrow::array::Float64Array::from(values.to_vec()))];
        let chunk = Arc::new(
            delog_core::chunk::Chunk::try_new(
                arrow::array::Int64Array::from(times.to_vec()),
                cols,
                &schema,
            )
            .unwrap(),
        );
        stores.push((
            topic,
            Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
        ));
    }
    StoreSnapshot::from_registry(&identity, stores, 0).unwrap()
}

fn multiplier_fixture(series: &[(&str, &[i64], &[f64], f64)]) -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let mut stores = Vec::new();
    for (label, times, values, multiplier) in series {
        let source = identity.add_source_with_kind(*label, SourceKind::File);
        let topic = identity.add_topic(source, "DATA").unwrap();
        identity.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [
                    FieldSchema::new("value", DataType::Float64, None::<String>, *multiplier)
                        .unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<arrow::array::ArrayRef> =
            vec![Arc::new(arrow::array::Float64Array::from(values.to_vec()))];
        let chunk = Arc::new(
            delog_core::chunk::Chunk::try_new(
                arrow::array::Int64Array::from(times.to_vec()),
                cols,
                &schema,
            )
            .unwrap(),
        );
        stores.push((
            topic,
            Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
        ));
    }
    StoreSnapshot::from_registry(&identity, stores, 0).unwrap()
}

fn alignment_fixture() -> StoreSnapshot {
    multi_source_fixture(&[
        ("reference", &[100, 200, 400], &[1.0, 2.0, 3.0]),
        ("target", &[10, 30, 80], &[1.0, 2.0, 3.0]),
        ("untouched", &[7, 9], &[1.0, 2.0]),
    ])
}

#[test]
fn selected_plot_time_range_unions_only_included_fields_with_drafts() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [reference, target, excluded] = sync.included_ids().try_into().unwrap();
    sync.set_draft_offset(target, 1_000).unwrap();
    sync.set_included(excluded, false).unwrap();
    let range = sync.selected_plot_time_range(&snapshot).unwrap();
    assert_eq!((range.min_us, range.max_us), (100, 1_080));
    assert_eq!(sync.reference(), reference);
}

#[test]
fn plot_fit_excludes_multiplier_overflow_rows_but_keeps_later_valid_rows() {
    let snapshot = multiplier_fixture(&[
        ("overflow", &[10, 20], &[f64::MAX, 1.0], 2.0),
        ("valid", &[30], &[3.0], 1.0),
    ]);
    let sync = SyncWindow::open(&snapshot).unwrap();
    let range = sync.selected_plot_time_range(&snapshot).unwrap();
    assert_eq!((range.min_us, range.max_us), (20, 30));
}

#[test]
fn plot_fit_handles_duplicate_and_max_boundary_samples() {
    let snapshot = multi_source_fixture(&[
        ("boundary", &[i64::MAX, i64::MAX], &[1.0, 2.0]),
        ("also-boundary", &[i64::MAX], &[3.0]),
    ]);
    let sync = SyncWindow::open(&snapshot).unwrap();
    assert_eq!(
        sync.view,
        Some(ViewX {
            min_us: i64::MAX - 1,
            max_us: i64::MAX,
        })
    );
}

#[test]
fn unrepresentable_full_domain_fit_preserves_the_current_view() {
    let snapshot = multi_source_fixture(&[
        ("full", &[i64::MIN, i64::MAX], &[1.0, 2.0]),
        ("inside", &[0], &[3.0]),
    ]);
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    sync.view = Some(ViewX::new(10, 20));
    assert!(!sync.fit_selected_plots(&snapshot));
    assert_eq!(sync.view, Some(ViewX::new(10, 20)));
}

#[test]
fn overflowing_trace_is_skipped_while_a_valid_trace_remains_fittable() {
    let snapshot = multi_source_fixture(&[
        ("overflow", &[1, 2], &[1.0, 2.0]),
        ("valid", &[20, 40], &[1.0, 2.0]),
    ]);
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [overflow, _valid] = sync.included_ids().try_into().unwrap();
    sync.source_mut(overflow).unwrap().draft_offset_us = i64::MAX;

    assert!(sync.fit_selected_plots(&snapshot));
    assert_eq!(sync.view, Some(ViewX::new(20, 40)));
}

#[test]
fn changed_reference_draft_is_included_in_plot_fit() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let reference = sync.reference();
    sync.source_mut(reference).unwrap().draft_offset_us = 1_000;

    assert!(sync.fit_selected_plots(&snapshot));
    assert_eq!(sync.view, Some(ViewX::new(7, 1_400)));
}

#[test]
fn plot_fit_skips_a_field_that_does_not_belong_to_its_source() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [first, second, third] = sync.included_ids().try_into().unwrap();
    let unrelated = sync.source(second).unwrap().field.unwrap();
    sync.source_mut(first).unwrap().field = Some(unrelated);
    sync.source_mut(first).unwrap().draft_offset_us = 10_000;
    sync.set_included(third, false).unwrap();

    assert!(sync.fit_selected_plots(&snapshot));
    assert_eq!(sync.view, Some(ViewX::new(10, 80)));
}

#[test]
fn plot_pointer_precedence_is_double_then_primary_pan_then_other_actions() {
    assert_eq!(
        plot_pointer_action(true, true),
        PlotPointerAction::DoubleClickFit
    );
    assert_eq!(
        plot_pointer_action(false, true),
        PlotPointerAction::PrimaryPan
    );
    assert_eq!(plot_pointer_action(false, false), PlotPointerAction::Other);
}

#[test]
fn accepted_view_actions_update_the_single_current_view() {
    let mut view = final_frame_views(
        ViewX::new(0, 100),
        PlotPointerAction::PrimaryPan,
        None,
        10.0,
        100.0,
    )
    .trace_projection;
    assert_eq!(view, ViewX::new(-10, 90));

    view = gpu::zoom_drag_view(view, 0.0, 100.0, 25.0, 75.0).unwrap();
    assert_eq!(view, ViewX::new(15, 65));
}

#[test]
fn accepted_navigation_supplies_one_final_view_to_trace_and_picker_paths() {
    let initial = ViewX::new(0, 100);
    let primary = final_frame_views(initial, PlotPointerAction::PrimaryPan, None, 10.0, 100.0);
    assert_eq!(primary.trace_projection, ViewX::new(-10, 90));
    assert_eq!(primary.picker_projection, primary.trace_projection);

    let fitted = ViewX::new(1_000, 2_000);
    let double = final_frame_views(
        initial,
        PlotPointerAction::DoubleClickFit,
        Some(fitted),
        0.0,
        100.0,
    );
    assert_eq!(double.trace_projection, fitted);
    assert_eq!(double.picker_projection, double.trace_projection);
}

#[test]
fn failed_plot_fit_preserves_the_current_view() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    sync.view = Some(ViewX::new(10, 20));
    assert!(!sync.fit_selected_plots(&snapshot));
    assert_eq!(sync.view, Some(ViewX::new(10, 20)));
}

fn change_fixture() -> StoreSnapshot {
    multi_source_fixture(&[
        ("reference", &[100, 200, 300], &[0.0, 0.0, 5.0]),
        ("target", &[10, 20, 40], &[8.0, 8.0, 9.0]),
        ("untouched", &[7, 9], &[1.0, 2.0]),
    ])
}

fn snapshot_with_offset(
    snapshot: &StoreSnapshot,
    source: SourceId,
    offset: i64,
) -> StoreSnapshot {
    let mut changed = snapshot.clone();
    let mut sources = changed.sources.to_vec();
    sources[source.index()].entry.offset_us = offset;
    changed.sources = Arc::from(sources);
    changed
}

#[test]
fn automatic_methods_update_only_the_active_target() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [reference, target, untouched] = sync.included_ids().try_into().unwrap();
    sync.set_active(target).unwrap();

    sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst)
        .unwrap();
    assert_eq!(sync.draft_offset(target), Some(90));
    assert_eq!(sync.draft_offset(untouched), Some(0));

    sync.align_active(&snapshot, AutoAlignMethod::LastToLast)
        .unwrap();
    assert_eq!(sync.draft_offset(target), Some(320));

    sync.align_active(&snapshot, AutoAlignMethod::BackToBack)
        .unwrap();
    assert_eq!(sync.draft_offset(target), Some(390));
    assert_eq!(sync.reference(), reference);
}

#[test]
fn automatic_methods_immediately_begin_applying_the_new_offset() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let target = sync.active.unwrap();

    let batch = sync
        .align_and_begin_apply(&snapshot, AutoAlignMethod::FirstToFirst)
        .unwrap();

    assert_eq!(batch, vec![(target, 90)]);
    assert!(sync.pending_apply.is_some());
    assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Clean));
    assert!(!sync.automatic_alignment_ready());
}

#[test]
fn picker_accepts_reference_then_target_and_aligns_only_target() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [reference, target, untouched] = sync.included_ids().try_into().unwrap();
    sync.set_active(target).unwrap();
    sync.begin_sample_pick().unwrap();
    assert_eq!(sync.pick_expected_source(), Some(reference));

    sync.accept_picked_sample(
        reference,
        SyncSample {
            row: 1,
            raw_time_us: 200,
            value: 0.0,
        },
    )
    .unwrap();
    assert_eq!(sync.pick_expected_source(), Some(target));
    sync.accept_picked_sample(
        target,
        SyncSample {
            row: 1,
            raw_time_us: 30,
            value: 0.0,
        },
    )
    .unwrap();

    assert_eq!(sync.draft_offset(target), Some(170));
    assert_eq!(sync.draft_offset(untouched), Some(0));
    assert_eq!(sync.pick_expected_source(), None);
}

#[test]
fn nearest_projected_sample_uses_radius_distance_and_row_tie_breaking() {
    let source = SourceId(0);
    let candidate = |row, x, y| ProjectedSample {
        source,
        sample: SyncSample {
            row,
            raw_time_us: row as i64,
            value: row as f64,
        },
        position: egui::pos2(x, y),
    };
    let candidates = [
        candidate(2, 10.0, 10.0),
        candidate(1, 20.0, 20.0),
        candidate(3, 30.0, 30.0),
    ];

    assert_eq!(
        nearest_projected_sample(egui::pos2(18.0, 18.0), candidates, 7.0)
            .map(|picked| picked.sample.row),
        Some(1)
    );
    assert_eq!(
        nearest_projected_sample(egui::pos2(15.0, 15.0), candidates, 7.1)
            .map(|picked| picked.sample.row),
        Some(1)
    );
    assert_eq!(
        nearest_projected_sample(egui::pos2(100.0, 100.0), candidates, 7.0),
        None
    );
}

#[test]
fn exact_interior_sample_is_a_projected_candidate_and_wins_the_click() {
    let snapshot = multi_source_fixture(&[
        ("reference", &[0, 1_000_000, 2_000_000], &[0.0, 1.0, 2.0]),
        ("target", &[0, 1_000_000, 2_000_000], &[0.0, 1.0, 2.0]),
    ]);
    let sync = SyncWindow::open(&snapshot).unwrap();
    let source = sync.reference();
    let field = sync.source(source).unwrap().field.unwrap();
    let cache = TraceCache::build(
        &snapshot,
        field,
        0,
        0,
        &delog_core::metrics::MetricsRegistry::new(),
    )
    .unwrap();

    let rows = projected_candidate_rows_in_x_range(&cache, 1.0, 1.0);
    assert_eq!(rows, vec![1], "the exact stored row must be eligible");
    let candidates = rows.into_iter().map(|row| ProjectedSample {
        source,
        sample: SyncSample {
            row,
            raw_time_us: row as i64 * 1_000_000,
            value: row as f64,
        },
        position: egui::pos2(cache.xy[row * 2], cache.xy[row * 2 + 1]),
    });
    assert_eq!(
        nearest_projected_sample(egui::pos2(1.0, 1.0), candidates, 0.1)
            .map(|picked| picked.sample.row),
        Some(1)
    );
}

#[test]
fn farther_x_sample_within_hit_radius_can_win_in_screen_space() {
    let snapshot = multi_source_fixture(&[
        ("reference", &[0, 1_000_000], &[100.0, 0.0]),
        ("target", &[0, 1_000_000], &[0.0, 0.0]),
    ]);
    let sync = SyncWindow::open(&snapshot).unwrap();
    let source = sync.reference();
    let field = sync.source(source).unwrap().field.unwrap();
    let cache = TraceCache::build(
        &snapshot,
        field,
        0,
        0,
        &delog_core::metrics::MetricsRegistry::new(),
    )
    .unwrap();

    let rows = projected_candidate_rows_in_x_range(&cache, 0.0, 1.0);
    assert_eq!(rows, vec![0, 1]);
    let candidates = [
        ProjectedSample {
            source,
            sample: SyncSample {
                row: rows[0],
                raw_time_us: 0,
                value: 100.0,
            },
            position: egui::pos2(1.0, 100.0),
        },
        ProjectedSample {
            source,
            sample: SyncSample {
                row: rows[1],
                raw_time_us: 1_000_000,
                value: 0.0,
            },
            position: egui::pos2(6.0, 0.0),
        },
    ];
    assert_eq!(
        nearest_projected_sample(egui::pos2(0.0, 0.0), candidates, 7.0)
            .map(|sample| sample.sample.row),
        Some(1),
        "the immediate-X row is vertically far, so the farther-X row must win"
    );
}

#[test]
fn stacked_picker_rejects_pointer_just_inside_adjacent_lane() {
    let expected_lane = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
    let adjacent_pointer = egui::pos2(50.0, 50.1);

    assert_eq!(
        pointer_fraction_in_lane(expected_lane, adjacent_pointer),
        None
    );
}

#[test]
fn picker_rejects_out_of_order_sources_without_mutation() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [_, target, _] = sync.included_ids().try_into().unwrap();
    sync.set_active(target).unwrap();
    sync.begin_sample_pick().unwrap();
    let before = sync.draft_offsets();
    assert_eq!(
        sync.accept_picked_sample(
            target,
            SyncSample {
                row: 0,
                raw_time_us: 10,
                value: 0.0,
            },
        ),
        Err(PickError::UnexpectedSource),
    );
    assert_eq!(sync.draft_offsets(), before);
}

#[test]
fn pair_or_field_changes_cancel_an_incomplete_pick() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [reference, target, other] = sync.included_ids().try_into().unwrap();
    sync.set_active(target).unwrap();
    sync.begin_sample_pick().unwrap();
    sync.set_active(other).unwrap();
    assert_eq!(sync.pick_expected_source(), None);

    sync.set_active(target).unwrap();
    sync.begin_sample_pick().unwrap();
    sync.set_reference(other).unwrap();
    assert_eq!(sync.pick_expected_source(), None);
    assert_ne!(reference, other);
}

#[test]
fn first_change_alignment_and_failures_preserve_the_previous_draft() {
    let snapshot = change_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let target = sync.first_movable().unwrap();
    sync.set_active(target).unwrap();
    sync.align_active(&snapshot, AutoAlignMethod::FirstChange)
        .unwrap();
    assert_eq!(sync.draft_offset(target), Some(260));

    sync.source_mut(target).unwrap().field = None;
    assert_eq!(
        sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst),
        Err(AlignmentError::FieldUnavailable)
    );
    assert_eq!(sync.draft_offset(target), Some(260));
}

#[test]
fn automatic_alignment_respects_the_reference_draft_offset() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [reference, target, _] = sync.included_ids().try_into().unwrap();
    sync.source_mut(reference).unwrap().draft_offset_us = 50;
    sync.set_active(target).unwrap();
    sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst)
        .unwrap();
    assert_eq!(sync.draft_offset(target), Some(140));
}

#[test]
fn changed_reference_remains_dirty_and_is_applied_with_dependent_target() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [original_reference, changed_reference, target] =
        sync.included_ids().try_into().unwrap();

    sync.set_draft_offset(changed_reference, 50).unwrap();
    sync.set_reference(changed_reference).unwrap();
    sync.set_active(target).unwrap();
    sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst)
        .unwrap();

    assert!(
        sync.is_dirty(),
        "the changed current reference must warn on close"
    );
    assert_eq!(
        sync.apply_request(&snapshot).unwrap(),
        vec![(changed_reference, 50), (target, 53)]
    );
    assert_eq!(sync.draft_offset(original_reference), Some(0));
}

#[test]
fn automatic_toolbar_actions_are_not_ready_during_sample_picking() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let target = sync.first_movable().unwrap();
    sync.set_active(target).unwrap();
    assert!(sync.automatic_alignment_ready());

    sync.begin_sample_pick().unwrap();

    assert!(!sync.automatic_alignment_ready());
    assert_eq!(sync.pick_expected_source(), Some(sync.reference()));
}

#[test]
fn opening_selects_the_first_source_as_reference_and_second_as_active() {
    let snapshot = fixture_snapshot();
    let sync = SyncWindow::open(&snapshot).unwrap();
    let [first, second, _] = sync.included_ids().try_into().unwrap();

    assert_eq!(sync.reference(), first);
    assert_eq!(sync.active, Some(second));
}

#[test]
fn rendered_and_legend_trace_colors_equal_the_standard_palette() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let traces = sync.rendered_sync_traces(&snapshot);
    assert!(traces.len() >= 3);

    for (index, rendered) in traces.iter().enumerate().take(3) {
        let expected = delog_render::palette::trace_color(index);
        assert_eq!(rendered.trace.color, expected.to_srgb_f32());
        assert_eq!(
            egui_trace_color(index),
            egui::Color32::from_rgba_unmultiplied(
                expected.r, expected.g, expected.b, expected.a
            )
        );
    }
}

#[test]
fn changing_reference_preserves_effective_drafts() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [a, b, c] = sync.included_ids().try_into().unwrap();
    sync.set_draft_offset(b, 500_000).unwrap();
    let before = sync.draft_offsets();
    sync.set_reference(c).unwrap();
    assert_eq!(sync.draft_offsets(), before);
    assert_eq!(sync.relative_offset(c), Some(0));
    assert_eq!(sync.reference(), c);
    assert_ne!(a, c);
}

#[test]
fn reference_rejects_direct_draft_edits_without_mutation() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let reference = sync.reference();
    let before_draft = sync.draft_offset(reference);
    let before_input = sync.input(reference).map(str::to_owned);

    assert_eq!(sync.set_draft_offset(reference, 99), Err(()));
    assert_eq!(sync.draft_offset(reference), before_draft);
    assert_eq!(sync.input(reference), before_input.as_deref());
}

#[test]
fn reference_rejects_input_edits_without_mutation() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let reference = sync.reference();
    let before_draft = sync.draft_offset(reference);
    let before_input = sync.input(reference).map(str::to_owned);

    assert_eq!(sync.set_input(reference, "99"), Err(()));
    assert_eq!(sync.draft_offset(reference), before_draft);
    assert_eq!(sync.input(reference), before_input.as_deref());
}

#[test]
fn apply_request_omits_unchanged_reference_and_detects_conflict() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let reference = sync.reference();
    let movable = sync.first_movable().unwrap();
    sync.set_draft_offset(movable, 42).unwrap();
    let request = sync.apply_request(&snapshot).unwrap();
    assert_eq!(request, vec![(movable, 42)]);
    assert!(!request.iter().any(|(id, _)| *id == reference));
    let changed = snapshot_with_offset(&snapshot, movable, 10);
    assert_eq!(sync.apply_request(&changed), Err(ApplyBlock::Conflict));
}

#[test]
fn exclusion_moves_reference_and_requires_two_sources() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [a, b, c] = sync.included_ids().try_into().unwrap();
    sync.set_included(a, false).unwrap();
    assert_eq!(sync.reference(), b);
    sync.set_included(c, false).unwrap();
    assert_eq!(
        sync.apply_request(&snapshot),
        Err(ApplyBlock::InsufficientSources)
    );
    assert_eq!(sync.set_included(b, false), Err(()));
    assert_eq!(sync.included_ids(), vec![b]);
    assert_eq!(sync.reference(), b);
}

#[test]
fn reconcile_removes_sources_clears_missing_fields_and_excludes_new_sources() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [a, b, c] = sync.included_ids().try_into().unwrap();
    let mut changed = snapshot.clone();
    let mut sources = changed.sources.to_vec();
    sources[a.index()].entry.removed = true;
    let new_id = SourceId(sources.len() as u32);
    let mut added = sources[c.index()].clone();
    added.entry.id = new_id;
    added.entry.label = "new".into();
    sources.push(added);
    changed.sources = Arc::from(sources);
    let mut fields = changed.fields.to_vec();
    fields[sync.source(b).unwrap().field.unwrap().index()].removed = true;
    changed.fields = Arc::from(fields);
    sync.reconcile(&changed);
    assert!(sync.source(a).is_none());
    assert_eq!(sync.source(b).unwrap().field, None);
    assert_eq!(sync.source(new_id).unwrap().included, false);
}

#[test]
fn file_sources_with_plottable_fields_are_the_only_offered_sources() {
    let mut identity = IdentityRegistry::new();
    let file = identity.add_source_with_kind("file", SourceKind::File);
    let live = identity.add_source_with_kind("live", SourceKind::Live);
    let text = identity.add_source_with_kind("text", SourceKind::File);
    let mut stores = Vec::new();
    for (source, dtype) in [
        (file, DataType::Float64),
        (live, DataType::Float64),
        (text, DataType::Utf8),
    ] {
        let topic = identity.add_topic(source, "DATA").unwrap();
        identity.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", dtype, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        stores.push((topic, Arc::new(TopicStore::new(schema))));
    }
    let snapshot = StoreSnapshot::from_registry(&identity, stores, 0).unwrap();
    let sync = SyncWindow::open(&snapshot).expect("both file sources remain available");
    assert_eq!(sync.included_ids(), vec![file, text]);
    assert_eq!(sync.source(text).unwrap().field, None);
    assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::InvalidInput));
}

#[test]
fn excluded_or_unavailable_sources_cannot_activate_or_move_and_active_falls_back() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [_, b, c] = sync.included_ids().try_into().unwrap();
    sync.set_active(b).unwrap();
    sync.set_included(b, false).unwrap();
    assert_eq!(sync.active, Some(c));
    assert_eq!(sync.set_active(b), Err(()));
    assert_eq!(sync.set_draft_offset(b, 99), Err(()));
    assert_eq!(sync.draft_offset(b), Some(0));

    sync.source_mut(c).unwrap().field = None;
    assert_eq!(sync.set_active(c), Err(()));
    assert_eq!(sync.set_draft_offset(c, 99), Err(()));
}

#[test]
fn preview_delta_uses_current_snapshot_offset_and_reports_overflow() {
    assert_eq!(preview_delta_us(120, 70), Ok(50));
    assert_eq!(preview_delta_us(i64::MIN, i64::MAX), Err(OffsetMathError));
}

#[test]
fn rendered_trace_mapping_omits_overflowing_preceding_source_from_lane_indices() {
    let snapshot = alignment_fixture();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [omitted, expected_first_lane, expected_second_lane] =
        sync.included_ids().try_into().unwrap();
    sync.source_mut(omitted).unwrap().draft_offset_us = i64::MIN;
    let changed = snapshot_with_offset(&snapshot, omitted, i64::MAX);

    let rendered = sync.rendered_sync_traces(&changed);

    assert_eq!(
        rendered
            .iter()
            .map(|trace| trace.source)
            .collect::<Vec<_>>(),
        vec![expected_first_lane, expected_second_lane]
    );
    assert_eq!(
        rendered[0].trace.field,
        sync.source(expected_first_lane).unwrap().field.unwrap()
    );
}

#[test]
fn rejected_pending_apply_clears_only_on_a_later_epoch() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let movable = sync.first_movable().unwrap();
    sync.set_draft_offset(movable, 42).unwrap();
    sync.begin_apply(vec![(movable, 42)], snapshot.epoch);
    sync.reconcile(&snapshot);
    assert!(
        sync.pending_apply.is_some(),
        "dispatch epoch cannot acknowledge"
    );

    let mut later = snapshot.clone();
    later.epoch += 1;
    sync.reconcile(&later);
    assert!(sync.pending_apply.is_none());
    assert_eq!(sync.apply_block(&later), Some(ApplyBlock::Conflict));
}

#[test]
fn failed_apply_dispatch_is_immediately_reloadable() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let movable = sync.first_movable().unwrap();
    sync.set_draft_offset(movable, 42).unwrap();
    sync.begin_apply(vec![(movable, 42)], snapshot.epoch);
    sync.apply_dispatch_failed();
    assert!(sync.pending_apply.is_none());
    assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Conflict));
    sync.reload_offsets(&snapshot);
    assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Clean));
}

#[test]
fn checked_drag_overflow_preserves_draft_and_marks_source_invalid() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let movable = sync.first_movable().unwrap();
    sync.set_draft_offset(movable, i64::MAX).unwrap();
    assert_eq!(
        sync.apply_drag_delta(movable, i64::MAX, 1),
        Err(OffsetMathError)
    );
    assert_eq!(sync.draft_offset(movable), Some(i64::MAX));
    assert!(!sync.source(movable).unwrap().input.valid);
}

#[test]
fn removed_field_does_not_shift_schema_alignment() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source_with_kind("file", SourceKind::File);
    let topic = identity.add_topic(source, "DATA").unwrap();
    let removed = identity.add_field(topic, "text").unwrap();
    let numeric = identity.add_field(topic, "value").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "DATA",
            [
                FieldSchema::new("text", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let mut snapshot = StoreSnapshot::from_registry(
        &identity,
        [(topic, Arc::new(TopicStore::new(schema)))],
        0,
    )
    .unwrap();
    let mut fields = snapshot.fields.to_vec();
    fields[removed.index()].removed = true;
    snapshot.fields = Arc::from(fields);
    assert_eq!(
        first_plottable_field_in_topic(&snapshot, source, topic),
        Some(numeric)
    );
}

#[test]
fn topic_selection_scopes_fields_and_selects_the_first_plottable_field() {
    let mut identity = IdentityRegistry::new();
    let first_source = identity.add_source_with_kind("first", SourceKind::File);
    let second_source = identity.add_source_with_kind("second", SourceKind::File);
    let primary = identity.add_topic(first_source, "PRIMARY").unwrap();
    let primary_field = identity.add_field(primary, "value").unwrap();
    let secondary = identity.add_topic(first_source, "SECONDARY").unwrap();
    let secondary_field = identity.add_field(secondary, "other").unwrap();
    let peer = identity.add_topic(second_source, "PEER").unwrap();
    identity.add_field(peer, "value").unwrap();

    let schema = |name: &str, field: &str| {
        Arc::new(
            TopicSchema::new(
                name,
                [FieldSchema::new(field, DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        )
    };
    let snapshot = StoreSnapshot::from_registry(
        &identity,
        [
            (
                primary,
                Arc::new(TopicStore::new(schema("PRIMARY", "value"))),
            ),
            (
                secondary,
                Arc::new(TopicStore::new(schema("SECONDARY", "other"))),
            ),
            (peer, Arc::new(TopicStore::new(schema("PEER", "value")))),
        ],
        0,
    )
    .unwrap();
    let mut sync = SyncWindow::open(&snapshot).unwrap();

    assert_eq!(sync.source(first_source).unwrap().topic, Some(primary));
    assert_eq!(
        sync.source(first_source).unwrap().field,
        Some(primary_field)
    );
    sync.set_topic(&snapshot, first_source, secondary).unwrap();
    assert_eq!(sync.source(first_source).unwrap().topic, Some(secondary));
    assert_eq!(
        sync.source(first_source).unwrap().field,
        Some(secondary_field)
    );
    assert_eq!(
        plottable_fields(&snapshot, first_source, secondary),
        vec![secondary_field]
    );
    assert_eq!(sync.set_topic(&snapshot, first_source, peer), Err(()));
}

#[test]
fn fuzzy_result_selects_topic_and_field_together() {
    let mut identity = IdentityRegistry::new();
    let first = identity.add_source_with_kind("first", SourceKind::File);
    let second = identity.add_source_with_kind("second", SourceKind::File);
    let attitude = identity.add_topic(first, "ATTITUDE").unwrap();
    identity.add_field(attitude, "roll").unwrap();
    let gps = identity.add_topic(first, "GPS").unwrap();
    let latitude = identity.add_field(gps, "latitude").unwrap();
    let peer = identity.add_topic(second, "PEER").unwrap();
    identity.add_field(peer, "value").unwrap();
    let schema = |name: &str, field: &str| {
        Arc::new(
            TopicSchema::new(
                name,
                [FieldSchema::new(field, DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        )
    };
    let snapshot = StoreSnapshot::from_registry(
        &identity,
        [
            (
                attitude,
                Arc::new(TopicStore::new(schema("ATTITUDE", "roll"))),
            ),
            (gps, Arc::new(TopicStore::new(schema("GPS", "latitude")))),
            (peer, Arc::new(TopicStore::new(schema("PEER", "value")))),
        ],
        0,
    )
    .unwrap();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let result = field_search_results(&snapshot, first, "gp lat")
        .into_iter()
        .next()
        .unwrap();

    sync.select_search_result(first, result).unwrap();
    assert_eq!(sync.source(first).unwrap().topic, Some(gps));
    assert_eq!(sync.source(first).unwrap().field, Some(latitude));
}

#[test]
fn reset_and_apply_lifecycle_tracks_clean_dirty_and_input_validity() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let [_, b, c] = sync.included_ids().try_into().unwrap();
    assert_eq!(sync.apply_request(&snapshot), Err(ApplyBlock::Clean));
    sync.set_draft_offset(b, 5).unwrap();
    sync.set_draft_offset(c, 7).unwrap();
    assert!(sync.is_dirty());
    sync.reset_one(b).unwrap();
    assert_eq!(sync.draft_offset(b), Some(0));
    sync.reset_all();
    assert!(!sync.is_dirty());
    sync.set_input(b, "bad").unwrap();
    assert_eq!(sync.apply_request(&snapshot), Err(ApplyBlock::InvalidInput));
    sync.set_input(b, "12").unwrap();
    assert_eq!(sync.draft_offset(b), Some(12));
    let applied = snapshot_with_offset(&snapshot, b, 12);
    sync.mark_applied(&applied);
    assert!(!sync.is_dirty());
    assert_eq!(sync.input(b), Some("12"));
}

#[test]
fn reference_controls_are_disabled_and_apply_tracks_policy() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    assert!(!sync.controls(sync.reference()).movable);
    assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Clean));
    let movable = sync.first_movable().unwrap();
    sync.set_draft_offset(movable, 1).unwrap();
    assert_eq!(sync.apply_block(&snapshot), None);
}

#[test]
fn view_toggle_preserves_alignment_state() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let movable = sync.first_movable().unwrap();
    sync.set_draft_offset(movable, 77).unwrap();
    sync.set_mode(CompareMode::Stacked);
    assert_eq!(sync.draft_offset(movable), Some(77));
}

#[test]
fn overlay_geometry_uses_one_padded_union() {
    let raw = [
        Some(PreparedYRange::new(0.0, 0.0, 10.0).unwrap()),
        Some(PreparedYRange::new(100.0, 0.0, 100.0).unwrap()),
    ];
    assert_eq!(
        prepared_y_ranges(CompareMode::Overlay, &raw),
        vec![
            PreparedYRange::new(0.0, -10.0, 210.0).unwrap(),
            PreparedYRange::new(0.0, -10.0, 210.0).unwrap(),
        ]
    );
}

#[test]
fn stacked_geometry_pads_each_lane_independently() {
    let raw = [
        Some(PreparedYRange::new(0.0, 0.0, 10.0).unwrap()),
        Some(PreparedYRange::new(100.0, 0.0, 100.0).unwrap()),
    ];
    assert_eq!(
        prepared_y_ranges(CompareMode::Stacked, &raw),
        vec![
            PreparedYRange::new(0.0, -0.5, 10.5).unwrap(),
            PreparedYRange::new(100.0, -5.0, 105.0).unwrap(),
        ]
    );
}

#[test]
fn stacked_geometry_keeps_ready_traces_without_visible_samples() {
    let raw = [Some(PreparedYRange::new(10.0, 2.0, 4.0).unwrap()), None];
    assert_eq!(
        prepared_y_ranges(CompareMode::Stacked, &raw),
        vec![
            PreparedYRange::new(10.0, 1.9, 4.1).unwrap(),
            PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
        ]
    );
}

#[test]
fn overlay_geometry_uses_finite_union_and_keeps_ready_empty_traces() {
    let raw = [None, Some(PreparedYRange::new(100.0, 2.0, 4.0).unwrap())];
    assert_eq!(
        prepared_y_ranges(CompareMode::Overlay, &raw),
        vec![
            PreparedYRange::new(100.0, 1.9, 4.1).unwrap(),
            PreparedYRange::new(100.0, 1.9, 4.1).unwrap(),
        ]
    );

    let all_empty = [None, None];
    assert_eq!(
        prepared_y_ranges(CompareMode::Overlay, &all_empty),
        vec![
            PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
            PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
        ]
    );
}

#[test]
fn preparation_repaint_stops_after_relevant_cache_build_finishes() {
    assert!(preparation_needs_repaint([true, false]));
    assert!(!preparation_needs_repaint([false, false]));
}

#[test]
fn overlay_flat_padding_survives_large_origin() {
    let raw = [Some(PreparedYRange::new(1.0e20, 0.0, 0.0).unwrap())];
    let ranges = prepared_y_ranges(CompareMode::Overlay, &raw);
    assert_eq!(ranges[0].span(), 2.0);
}

#[test]
fn stacked_flat_padding_survives_large_origin() {
    let raw = [Some(PreparedYRange::new(1.0e20, 0.0, 0.0).unwrap())];
    let ranges = prepared_y_ranges(CompareMode::Stacked, &raw);
    assert_eq!(ranges[0].span(), 2.0);
}

#[test]
fn overlay_and_stacked_keep_single_trace_large_origin_geometry_in_parity() {
    let raw = [Some(PreparedYRange::new(1.0e20, -4.0, 6.0).unwrap())];
    assert_eq!(
        prepared_y_ranges(CompareMode::Overlay, &raw),
        prepared_y_ranges(CompareMode::Stacked, &raw),
    );
    assert_eq!(
        prepared_y_ranges(CompareMode::Overlay, &raw)[0].span(),
        11.0
    );
}

#[test]
fn invalid_exact_edit_counts_as_dirty_for_close_policy() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let movable = sync.first_movable().unwrap();
    sync.set_input(movable, "bad").unwrap();
    assert!(sync.is_dirty());
}

#[test]
fn newly_opened_sync_window_defaults_to_stacked() {
    let snapshot = fixture_snapshot();
    let sync = SyncWindow::open(&snapshot).unwrap();
    assert_eq!(sync.mode, CompareMode::Stacked);
}

#[test]
fn stacked_toggle_is_offered_before_overlay() {
    let source = include_str!("mod.rs");
    let stacked = source
        .find("CompareMode::Stacked, \"Stacked\"")
        .expect("stacked toggle should exist");
    let overlay = source
        .find("CompareMode::Overlay, \"Overlay\"")
        .expect("overlay toggle should exist");
    assert!(
        stacked < overlay,
        "Stacked should be the first compare-mode toggle"
    );
}

#[test]
fn conflict_reload_captures_current_offsets_and_preserves_presentation() {
    let snapshot = fixture_snapshot();
    let mut sync = SyncWindow::open(&snapshot).unwrap();
    let reference = sync.reference();
    let movable = sync.first_movable().unwrap();
    let field = sync.source(movable).unwrap().field;
    sync.set_mode(CompareMode::Stacked);
    sync.set_draft_offset(movable, 77).unwrap();
    let changed = snapshot_with_offset(&snapshot, movable, 42);
    assert_eq!(sync.apply_block(&changed), Some(ApplyBlock::Conflict));

    sync.reload_offsets(&changed);

    assert_eq!(sync.apply_block(&changed), Some(ApplyBlock::Clean));
    assert!(!sync.is_dirty());
    assert_eq!(sync.draft_offset(movable), Some(42));
    assert_eq!(sync.reference(), reference);
    assert_eq!(sync.source(movable).unwrap().field, field);
    assert_eq!(sync.mode, CompareMode::Stacked);
}

#[test]
fn conflict_footer_exposes_reload_current_offsets_control() {
    let source = include_str!("mod.rs");
    assert!(source.contains("Reload current offsets"));
}

#[test]
fn overlay_hit_test_selects_nearest_trace_and_misses_outside_threshold() {
    let traces = [
        OverlayHitSegment::new(0, egui::pos2(0.0, 10.0), egui::pos2(100.0, 10.0)),
        OverlayHitSegment::new(1, egui::pos2(0.0, 30.0), egui::pos2(100.0, 30.0)),
    ];
    assert_eq!(
        nearest_overlay_trace(egui::pos2(50.0, 11.0), &traces, 6.0),
        Some(0)
    );
    assert_eq!(
        nearest_overlay_trace(egui::pos2(50.0, 28.0), &traces, 6.0),
        Some(1)
    );
    assert_eq!(
        nearest_overlay_trace(egui::pos2(50.0, 50.0), &traces, 6.0),
        None
    );
}

#[test]
fn overlay_hit_test_breaks_equal_distance_ties_by_trace_order() {
    let traces = [
        OverlayHitSegment::new(0, egui::pos2(0.0, 10.0), egui::pos2(100.0, 10.0)),
        OverlayHitSegment::new(1, egui::pos2(0.0, 20.0), egui::pos2(100.0, 20.0)),
    ];
    assert_eq!(
        nearest_overlay_trace(egui::pos2(50.0, 15.0), &traces, 6.0),
        Some(0)
    );
}

#[test]
fn tiny_lane_rejects_pointer_projection() {
    let lane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0e-7, 100.0));
    assert_eq!(pointer_fraction_in_lane(lane, egui::pos2(0.0, 50.0)), None);
}

#[test]
fn exact_offset_parser_supports_required_units() {
    assert_eq!(parse_offset_us("500 us"), Ok(500));
    assert_eq!(parse_offset_us("-250 ms"), Ok(-250_000));
    assert_eq!(parse_offset_us("1.2 s"), Ok(1_200_000));
    assert_eq!(
        parse_offset_us("0.1 us"),
        Err(OffsetParseError::FractionalMicrosecond)
    );
    assert_eq!(parse_offset_us("1 minute"), Err(OffsetParseError::Unit));
}

#[test]
fn exact_offset_parser_rejects_invalid_and_out_of_range_values() {
    assert_eq!(parse_offset_us("1"), Err(OffsetParseError::Unit));
    assert_eq!(parse_offset_us("1 ms extra"), Err(OffsetParseError::Syntax));
    assert_eq!(parse_offset_us("NaN s"), Err(OffsetParseError::NonFinite));
    assert_eq!(parse_offset_us("1e30 s"), Err(OffsetParseError::Overflow));
}

#[test]
fn offset_formatter_uses_exact_largest_unit_and_round_trips() {
    for (value, formatted) in [
        (2_000_000, "2 s"),
        (-250_000, "-250 ms"),
        (501, "501 us"),
        (0, "0 s"),
        (i64::MAX, "9223372036854775807 us"),
        (i64::MIN, "-9223372036854775808 us"),
    ] {
        assert_eq!(format_offset_us(value), formatted);
        assert_eq!(parse_offset_us(formatted), Ok(value));
    }
}

#[test]
fn drag_follows_visible_span() {
    assert_eq!(drag_delta_us(25.0, 100.0, 1_000_000), Some(250_000));
}

#[test]
fn drag_rejects_invalid_or_overflowing_inputs() {
    assert_eq!(drag_delta_us(f32::NAN, 100.0, 1_000_000), None);
    assert_eq!(drag_delta_us(25.0, 0.0, 1_000_000), None);
    assert_eq!(drag_delta_us(1.0, 1.0e-7, 1_000_000), None);
    assert_eq!(drag_delta_us(25.0, 100.0, 0), None);
    assert_eq!(drag_delta_us(f32::MAX, 1.0, i64::MAX), None);
}

#[test]
fn plot_height_reserves_footer_and_never_grows_with_unbounded_space() {
    assert_eq!(sync_plot_height(800.0), 360.0);
    assert_eq!(sync_plot_height(220.0), 184.0);
    assert_eq!(sync_plot_height(20.0), 1.0);
}

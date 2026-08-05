use arrow::array::{ArrayRef, Int32Array, Int64Array};
use arrow::datatypes::DataType;

#[test]
fn y_padding_matches_normal_plot_policy() {
    assert_eq!(padded_y_range(10.0, 20.0), (9.5, 20.5));
    assert_eq!(padded_y_range(7.0, 7.0), (6.0, 8.0));
    assert_eq!(padded_y_range(f64::NAN, 1.0), (-1.0, 1.0));
}

#[test]
fn both_padding_paths_fallback_when_padding_overflows() {
    let min = f64::MAX / 2.0;
    let max = f64::MAX;
    assert_eq!(padded_y_range(min, max), (-1.0, 1.0));
    assert_eq!(
        PreparedYRange::new(0.0, min, max).unwrap().padded(),
        PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
    );
}

#[test]
fn prepared_padding_failure_resets_the_semantic_origin_with_fallback() {
    let range = PreparedYRange::new(1.0e20, f64::MAX / 2.0, f64::MAX).unwrap();
    assert_eq!(range.padded(), PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),);
}

#[test]
fn sync_y_axis_uses_the_prepared_relative_range() {
    let range = PreparedYRange::new(1000.0, 0.0, 10.0).unwrap();
    assert_eq!(sync_y_axis(range, 997.0), Some((0.2, 3.0)));
}

#[test]
fn sync_y_span_survives_large_distant_cache_origin() {
    let range = PreparedYRange::new(1.0e12, 0.0, 8.0).unwrap();
    let (scale, lower) = sync_y_axis(range, -1.0e12).unwrap();
    assert_eq!(scale, 0.25);
    assert!(lower.is_finite());
}

#[test]
fn flat_padding_survives_large_absolute_origin() {
    let range = PreparedYRange::new(1.0e20, 0.0, 0.0).unwrap().padded();
    assert_eq!(range.span(), 2.0);
}
use delog_core::chunk::Chunk;
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use super::*;

#[test]
fn sync_stacked_lanes_are_equal_and_cover_the_plot() {
    let rect = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 80.0));
    let traces: Vec<_> = sync_lane_fractions(3, CompareMode::Stacked)
        .into_iter()
        .map(|lane| SyncTrace {
            field: FieldId(0),
            preview_delta_us: 0,
            color: [0.0; 4],
            y_range: PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
            lane: Some(lane),
        })
        .collect();
    let lanes = sync_lane_rects(rect, &traces);
    assert_eq!(lanes.len(), 3);
    assert_eq!(
        lanes[0],
        egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 40.0))
    );
    assert_eq!(
        lanes[2],
        egui::Rect::from_min_max(egui::pos2(10.0, 60.0), egui::pos2(110.0, 80.0))
    );
}

#[test]
fn sync_traces_share_absolute_x_bounds_across_cache_origins() {
    let view = ViewX::new(2_000_000, 5_000_000);
    assert_eq!(sync_x_bounds(view, 0), (2.0, 5.0));
    assert_eq!(sync_x_bounds(view, 1_000_000), (1.0, 4.0));
}

#[test]
fn sync_active_trace_resolves_the_lane_under_the_pointer() {
    let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 90.0));
    let traces: Vec<_> = sync_lane_fractions(3, CompareMode::Stacked)
        .into_iter()
        .map(|lane| SyncTrace {
            field: FieldId(0),
            preview_delta_us: 0,
            color: [0.0; 4],
            y_range: PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
            lane: Some(lane),
        })
        .collect();
    let lanes = sync_lane_rects(rect, &traces);
    assert_eq!(
        sync_active_trace_at(&lanes, egui::pos2(50.0, 45.0)),
        Some(1)
    );
    assert_eq!(sync_active_trace_at(&lanes, egui::pos2(150.0, 45.0)), None);
}

#[test]
fn sync_active_trace_ignores_tiny_lanes() {
    let too_narrow = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0e-7, 100.0));
    let too_short = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 1.0e-7));

    assert_eq!(
        sync_active_trace_at(&[too_narrow], egui::pos2(0.0, 50.0)),
        None
    );
    assert_eq!(
        sync_active_trace_at(&[too_short], egui::pos2(50.0, 0.0)),
        None
    );
}

#[test]
fn visible_y_range_merges_distinct_trace_origins_as_absolute_values() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "DATA").unwrap();
    let low = identity.add_field(topic, "Low").unwrap();
    let high = identity.add_field(topic, "High").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "DATA",
            [
                FieldSchema::new("Low", DataType::Int32, None::<String>, 0.01).unwrap(),
                FieldSchema::new("High", DataType::Int32, None::<String>, 0.01).unwrap(),
            ],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![0, 1_000_000]),
            vec![
                Arc::new(Int32Array::from(vec![10_000, 10_100])) as ArrayRef,
                Arc::new(Int32Array::from(vec![100_000, 100_200])) as ArrayRef,
            ],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot =
        Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());
    let mut caches = CacheManager::new();
    caches.request(low, &snapshot);
    caches.request(high, &snapshot);
    for _ in 0..2_000 {
        caches.poll_builds();
        if caches.is_ready(low) && caches.is_ready(high) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(caches.is_ready(low) && caches.is_ready(high));

    let mut pane = PlotPane::default();
    pane.add_trace(low);
    pane.add_trace(high);
    let (min, max) = visible_y_range(&mut caches, &pane, 0.0, 1.0, RenderTuning::default());
    assert!((min - 54.9).abs() < 1e-9, "min was {min}");
    assert!((max - 1_047.1).abs() < 1e-9, "max was {max}");
}

#[test]
fn visible_y_range_threads_tuning_and_trace_mode_to_cache_geometry() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "DATA").unwrap();
    let field = identity.add_field(topic, "Value").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "DATA",
            [FieldSchema::new("Value", DataType::Int32, None::<String>, 0.01).unwrap()],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![0, 1_000_000, 10_000_000, 11_000_000]),
            vec![Arc::new(Int32Array::from(vec![0, 100, 10_000, 10_100])) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot =
        Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());
    let mut caches = CacheManager::new();
    caches.request(field, &snapshot);
    for _ in 0..2_000 {
        caches.poll_builds();
        if caches.is_ready(field) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(caches.is_ready(field));

    let mut pane = PlotPane::default();
    pane.add_trace(field);
    let tuning = RenderTuning {
        gap_mode: GapMode::Cut,
        gap_factor: 2.0,
        ..RenderTuning::default()
    };

    let (min, max) = visible_y_range(&mut caches, &pane, 0.0, 5.0, tuning);

    assert!((min - -0.05).abs() < 1e-9, "min was {min}");
    assert!((max - 1.05).abs() < 1e-9, "max was {max}");

    pane.traces[0].mode = TraceMode::Step;
    let tuning = RenderTuning {
        gap_mode: GapMode::Connect,
        ..tuning
    };
    let (min, max) = visible_y_range(&mut caches, &pane, 5.0, 11.0, tuning);
    assert!((min - -4.0).abs() < 1e-9, "min was {min}");
    assert!((max - 106.0).abs() < 1e-9, "max was {max}");
}

#[test]
fn visible_y_range_line_connect_singleton_uses_empty_fallback() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "DATA").unwrap();
    let field = identity.add_field(topic, "Value").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "DATA",
            [FieldSchema::new("Value", DataType::Int32, None::<String>, 0.01).unwrap()],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![1_000_000]),
            vec![Arc::new(Int32Array::from(vec![4_200])) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot =
        Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());
    let mut caches = CacheManager::new();
    caches.request(field, &snapshot);
    for _ in 0..2_000 {
        caches.poll_builds();
        if caches.is_ready(field) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(caches.is_ready(field));

    let mut pane = PlotPane::default();
    pane.add_trace(field);
    let tuning = RenderTuning {
        gap_mode: GapMode::Connect,
        ..RenderTuning::default()
    };

    assert_eq!(
        visible_y_range(&mut caches, &pane, 0.0, 2.0, tuning),
        (-1.0, 1.0)
    );
}

#[test]
fn active_scope_is_first_only_when_scope_count_exceeds_capacity() {
    let mut saturated: Vec<_> = (0..=MAP_TILE_CAPACITY as u64).map(MapScopeId).collect();
    prioritize_active_scope(&mut saturated, Some(MapScopeId(128)));
    assert_eq!(saturated[0], MapScopeId(128));

    let mut normal = vec![MapScopeId(1), MapScopeId(2)];
    prioritize_active_scope(&mut normal, Some(MapScopeId(2)));
    assert_eq!(normal, vec![MapScopeId(1), MapScopeId(2)]);
}

fn live_zoom(zoom: u8) -> Vec<(TileId, i32)> {
    (0..4)
        .flat_map(|y| (0..256).map(move |x| (TileId { zoom, x, y }, x as i32)))
        .collect()
}

#[test]
fn line_window_always_strips_non_finite_points() {
    let xy = [
        0.0,
        10.0, //
        1.0,
        f32::NAN, //
        2.0,
        12.0, //
        3.0,
        f32::INFINITY, //
        4.0,
        14.0,
    ];
    let line_xy = line_window_xy(&xy, 0, 5);
    assert_eq!(line_xy, vec![0.0, 10.0, 2.0, 12.0, 4.0, 14.0]);
}

#[test]
fn isolated_points_are_the_samples_with_gaps_on_both_sides() {
    // Regular 1s samples, one lone sample at t=10, more at t=20,21.
    let xy = [
        0.0, 1.0, //
        1.0, 2.0, //
        2.0, 3.0, //
        10.0, 4.0, //
        20.0, 5.0, //
        21.0, 6.0,
    ];
    let iso = isolated_points_xy(&xy, 5.0);
    assert_eq!(iso, vec![10.0, 4.0]);
}

#[test]
fn isolated_points_include_lone_edge_samples_and_need_a_threshold() {
    // A single sample is isolated by definition.
    assert_eq!(isolated_points_xy(&[3.0, 7.0], 1.0), vec![3.0, 7.0]);
    // Every sample farther than threshold from both neighbours is isolated.
    let sparse = [0.0, 1.0, 10.0, 2.0, 20.0, 3.0];
    assert_eq!(isolated_points_xy(&sparse, 5.0), sparse.to_vec());
    // Threshold 0 = delta detection off: nothing is isolated.
    assert!(isolated_points_xy(&sparse, 0.0).is_empty());
    assert!(isolated_points_xy(&[], 5.0).is_empty());
}

#[test]
fn point_draws_use_the_scatter_pipeline() {
    assert_eq!(
        DrawKind::Points { samples: 3 }.pipeline(),
        PipelineKind::Scatter
    );
    assert!(DrawKind::Points { samples: 1 }.is_drawable());
    assert!(!DrawKind::Points { samples: 0 }.is_drawable());
}

#[test]
fn bridge_draws_batch_with_line_pipeline_runs() {
    use PipelineKind::{Columns, Line};
    let kinds = [
        DrawKind::Columns { count: 100 },
        DrawKind::Bridge { samples: 5 },
        DrawKind::Line { samples: 10 },
        DrawKind::Bridge { samples: 5 },
    ];
    let runs = pipeline_runs(kinds.iter().map(|k| k.pipeline()));
    assert_eq!(runs, vec![(Columns, 1), (Line, 3)]);
    assert!(!DrawKind::Bridge { samples: 1 }.is_drawable());
    assert!(DrawKind::Bridge { samples: 2 }.is_drawable());
}

#[test]
fn batching_groups_consecutive_items_into_one_bind_per_pipeline_run() {
    use PipelineKind::{Columns, Line, Scatter};
    let kinds = [
        DrawKind::Line { samples: 10 },
        DrawKind::Line { samples: 20 },
        DrawKind::Scatter { samples: 5 },
        DrawKind::Line { samples: 7 },
        DrawKind::Columns { count: 100 },
    ];
    let runs = pipeline_runs(kinds.iter().map(|k| k.pipeline()));
    // Draw order is preserved; each run = exactly one set_pipeline call.
    assert_eq!(runs, vec![(Line, 2), (Scatter, 1), (Line, 1), (Columns, 1)]);
    assert_eq!(pipeline_runs([].into_iter()), vec![]);
}

#[test]
fn scissor_is_viewport_clip_intersection_clamped_to_screen() {
    assert_eq!(
        intersect_scissor_rect((10, 20, 100, 80), (50, 0, 70, 50), [200, 200]),
        Some((50, 20, 60, 30))
    );
    assert_eq!(
        intersect_scissor_rect((-10, -10, 20, 20), (-5, -5, 20, 20), [100, 100]),
        Some((0, 0, 10, 10))
    );
    assert_eq!(
        intersect_scissor_rect((0, 0, 10, 10), (20, 20, 5, 5), [100, 100]),
        None
    );
}

#[test]
fn pan_maps_pixels_to_time_and_follows_the_pointer() {
    let mut view = ViewX::new(0, 1000);
    // Drag right by half the width → window shifts left by half the span.
    apply_pan(&mut view, 50.0, 100.0);
    assert_eq!((view.min_us, view.max_us), (-500, 500));
}

#[test]
fn zoom_in_shrinks_the_span_about_the_cursor() {
    let mut view = ViewX::new(0, 1000);
    apply_zoom(&mut view, 0.5, 200.0);
    assert!(view.span_us() < 1000);
    // Centre stays roughly fixed.
    let centre = (view.min_us + view.max_us) / 2;
    assert!((centre - 500).abs() < 50);
}

#[test]
fn zoom_drag_left_to_right_selects_window() {
    let view = ViewX::new(0, 1000);
    // rect: left=100, width=100. Drag from 25% to 75% of the rect.
    let out = zoom_drag_view(view, 100.0, 100.0, 125.0, 175.0).unwrap();
    assert_eq!(out.min_us, 250);
    assert_eq!(out.max_us, 750);
}

#[test]
fn zoom_drag_is_symmetric() {
    let view = ViewX::new(0, 1000);
    let fwd = zoom_drag_view(view, 100.0, 100.0, 125.0, 175.0).unwrap();
    let rev = zoom_drag_view(view, 100.0, 100.0, 175.0, 125.0).unwrap();
    assert_eq!(fwd.min_us, rev.min_us);
    assert_eq!(fwd.max_us, rev.max_us);
}

#[test]
fn zoom_drag_below_threshold_is_noop() {
    let view = ViewX::new(0, 1000);
    assert!(zoom_drag_view(view, 100.0, 100.0, 150.0, 152.0).is_none());
}

#[test]
fn zoom_drag_clamps_past_rect_edges() {
    let view = ViewX::new(0, 1000);
    // x well outside the rect on both sides clamps to full 0..1000.
    let out = zoom_drag_view(view, 100.0, 100.0, -50.0, 500.0).unwrap();
    assert_eq!(out.min_us, 0);
    assert_eq!(out.max_us, 1000);
}

#[test]
fn zoom_drag_zero_width_rect_is_noop() {
    let view = ViewX::new(0, 1000);
    assert!(zoom_drag_view(view, 0.0, 0.0, 5.0, 50.0).is_none());
}

#[test]
fn mixed_ready_batch_classifies_current_and_stale_independent_of_order() {
    let selection = MapTileSelection {
        scope: MapScopeId(4),
        epoch: 3,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 9,
        current_tiles: live_zoom(12),
        enabled: true,
    };
    let tile = |zoom, generation| ReadyTile {
        scope: MapScopeId(4),
        epoch: 3,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        id: crate::map::provider::TileId { zoom, x: 0, y: 0 },
        generation,
        rgba: Vec::new(),
        corners: [[0.0; 3]; 4],
    };
    let mixed = [tile(11, 9), tile(12, 9)];
    assert!(mixed.iter().all(|tile| map_tile_matches(&selection, tile)));
    assert!(map_tile_is_current(&selection, &mixed[1]));
    assert!(!map_tile_is_current(&selection, &mixed[0]));
    assert!(!map_tile_matches(&selection, &tile(12, 8)));
}

#[test]
fn prepare_map_tiles_exposes_sorted_fallback_then_current_draw_groups() {
    let Some(ctx) = RenderContext::headless() else {
        eprintln!("no wgpu adapter - skipping map zoom grouping test");
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = MapTileSelection {
        scope: MapScopeId(44),
        epoch: 2,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 7,
        current_tiles: live_zoom(8),
        enabled: true,
    };
    let tile = |zoom, x| ReadyTile {
        scope: selection.scope,
        epoch: selection.epoch,
        provider: selection.provider,
        id: crate::map::provider::TileId { zoom, x, y: 3 },
        generation: selection.generation,
        rgba: [x as u8, zoom, 0, 255].repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let tiles = [tile(8, 5), tile(7, 2), tile(8, 1), tile(7, 6)];
    let mut expected_fallback = vec![map_tile_key(&tiles[1]), map_tile_key(&tiles[3])];
    let mut expected_current = vec![map_tile_key(&tiles[0]), map_tile_key(&tiles[2])];
    expected_fallback.sort_unstable();
    expected_current.sort_unstable();
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();

    let first = resources.prepare_map_tiles(identity, &selection, &tiles);
    let reversed = resources.prepare_map_tiles(
        identity,
        &selection,
        &tiles.iter().rev().cloned().collect::<Vec<_>>(),
    );
    assert_eq!(first.fallback, expected_fallback);
    assert_eq!(first.current, expected_current);
    assert_eq!(reversed, first, "ready insertion order cannot affect draws");
    assert!(resources.selection_has_current_imagery(&selection));

    let mut other_pane = selection.clone();
    other_pane.scope = MapScopeId(45);

    let mut switched_generation = selection.clone();
    switched_generation.generation += 1;
}

#[test]
fn cache_epoch_change_purges_cpu_and_gpu_tiles_on_empty_poll() {
    let Some(ctx) = RenderContext::headless() else {
        eprintln!("no wgpu adapter - skipping map clear residency test");
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = MapTileSelection {
        scope: MapScopeId(7),
        epoch: 0,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: vec![(
            crate::map::provider::TileId {
                zoom: 3,
                x: 1,
                y: 2,
            },
            0,
        )],
        enabled: true,
    };
    let tile = ReadyTile {
        scope: selection.scope,
        epoch: 0,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        id: crate::map::provider::TileId {
            zoom: 3,
            x: 1,
            y: 2,
        },
        generation: 1,
        rgba: [40, 80, 120, 255].repeat(256 * 256),
        corners: [[0.0, 0.0, 0.0]; 4],
    };
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection, &[tile]);
    assert_eq!(resources.map_tile_cache[&selection.scope].len(), 1);
    assert_eq!(resources.map_tiles.resident_tile_count(), 1);

    resources.prepare_map_tiles(
        identity,
        &MapTileSelection {
            epoch: 1,
            ..selection.clone()
        },
        &[],
    );
    assert_eq!(resources.map_tile_cache[&selection.scope].len(), 0);
    assert_eq!(resources.map_tiles.resident_tile_count(), 0);
}

#[test]
fn map_tile_prepare_only_uploads_and_allocates_changed_residency() {
    let Some(ctx) = RenderContext::headless() else {
        eprintln!("no wgpu adapter - skipping map residency instrumentation test");
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = MapTileSelection {
        scope: MapScopeId(8),
        epoch: 2,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 5,
        current_tiles: live_zoom(4),
        enabled: true,
    };
    let tile = |zoom, x, color: [u8; 4]| ReadyTile {
        scope: selection.scope,
        epoch: selection.epoch,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        id: crate::map::provider::TileId { zoom, x, y: 1 },
        generation: selection.generation,
        rgba: color.repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection, &[tile(4, 1, [1, 2, 3, 255])]);
    assert_eq!(resources.map_tiles.upload_count(), 1);
    assert_eq!(resources.map_tiles.allocation_count(), 1);

    resources.prepare_map_tiles(identity, &selection, &[]);
    assert_eq!(
        resources.map_tiles.upload_count(),
        1,
        "static frame uploads zero"
    );
    assert_eq!(
        resources.map_tiles.allocation_count(),
        1,
        "static frame allocates zero"
    );

    let zoomed = MapTileSelection {
        current_tiles: live_zoom(5),
        ..selection
    };
    resources.prepare_map_tiles(identity, &zoomed, &[tile(5, 2, [4, 5, 6, 255])]);
    assert_eq!(resources.map_tiles.resident_tile_count(), 2);
    assert_eq!(
        resources.map_tiles.upload_count(),
        2,
        "only the new zoom uploads"
    );
    assert_eq!(
        resources.map_tiles.allocation_count(),
        2,
        "only the new zoom allocates"
    );

    resources.prepare_map_tiles(identity, &zoomed, &[tile(5, 2, [7, 8, 9, 255])]);
    assert_eq!(
        resources.map_tiles.upload_count(),
        3,
        "changed content uploads"
    );
}

#[test]
fn alternating_map_scopes_keep_union_resident_without_cross_pane_draws() {
    let Some(ctx) = RenderContext::headless() else {
        eprintln!("no wgpu adapter - skipping multi-pane map residency test");
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = |scope| MapTileSelection {
        scope: MapScopeId(scope),
        epoch: 4,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: live_zoom(6),
        enabled: true,
    };
    let tile = |scope, x, color: [u8; 4]| ReadyTile {
        scope: MapScopeId(scope),
        epoch: 4,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        id: crate::map::provider::TileId { zoom: 6, x, y: 2 },
        generation: 1,
        rgba: color.repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    let tile_a = tile(10, 1, [10, 20, 30, 255]);
    let tile_b = tile(20, 2, [40, 50, 60, 255]);
    let key_a = map_tile_key(&tile_a);
    let key_b = map_tile_key(&tile_b);

    let draw_a = resources.prepare_map_tiles(identity, &selection(10), &[tile_a]);
    assert_eq!(draw_a.current, vec![key_a], "pane A draws only A");
    assert!(draw_a.fallback.is_empty());
    let draw_b = resources.prepare_map_tiles(identity, &selection(20), &[tile_b]);
    assert_eq!(draw_b.current, vec![key_b], "pane B draws only B");
    assert!(draw_b.fallback.is_empty());
    let draw_a_again = resources.prepare_map_tiles(identity, &selection(10), &[]);
    assert_eq!(
        draw_a_again.current,
        vec![key_a],
        "pane A still draws only A"
    );
    assert!(draw_a_again.fallback.is_empty());
    assert_eq!(resources.map_tiles.resident_tile_count(), 2);
    assert_eq!(
        resources.map_tiles.upload_count(),
        2,
        "each scope uploads once"
    );

    let disabled_b = MapTileSelection {
        current_tiles: Vec::new(),
        enabled: false,
        ..selection(20)
    };
    let draw_disabled_b = resources.prepare_map_tiles(identity, &disabled_b, &[]);
    assert!(draw_disabled_b.is_empty());
    assert!(!resources.map_tile_cache.contains_key(&MapScopeId(20)));
    assert_eq!(resources.map_tiles.resident_tile_count(), 1);
    assert!(resources.map_tiles.contains(key_a), "disabling B retains A");

    let draw_a_after_b_disabled = resources.prepare_map_tiles(identity, &selection(10), &[]);
    assert_eq!(draw_a_after_b_disabled.current, vec![key_a]);
    assert!(draw_a_after_b_disabled.fallback.is_empty());
    assert_eq!(resources.map_tiles.upload_count(), 2, "A is not reuploaded");

    let draw_after_epoch = resources.prepare_map_tiles(
        identity,
        &MapTileSelection {
            epoch: 5,
            ..selection(10)
        },
        &[],
    );
    assert!(draw_after_epoch.is_empty());
    assert!(resources.map_tile_cache.values().all(HashMap::is_empty));
    assert_eq!(resources.map_tiles.resident_tile_count(), 0);
}

fn transition_selection(current: Vec<(TileId, i32)>) -> MapTileSelection {
    MapTileSelection {
        scope: MapScopeId(31),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: current,
        enabled: true,
    }
}

fn transition_tile(selection: &MapTileSelection, id: TileId) -> ReadyTile {
    ReadyTile {
        scope: selection.scope,
        epoch: selection.epoch,
        provider: selection.provider,
        id,
        generation: selection.generation,
        rgba: [id.x as u8, id.zoom, id.y as u8, 255].repeat(256 * 256),
        corners: [[id.x as f32, id.y as f32, 0.0]; 4],
    }
}

#[test]
fn stale_resident_tiles_fall_back_while_replacements_load() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let id = |x| TileId { zoom: 8, x, y: 4 };
    let shown = transition_selection(vec![(id(1), 0)]);
    let tile = transition_tile(&shown, id(1));
    let stale_key = map_tile_key(&tile);
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &shown, &[tile]);

    let moved = transition_selection(vec![(id(2), 0)]);
    let draw = resources.prepare_map_tiles(identity, &moved, &[]);
    assert!(draw.current.is_empty());
    assert_eq!(draw.fallback, vec![stale_key]);
    assert!(resources.map_tiles.contains(stale_key));

    let replacement = transition_tile(&moved, id(2));
    let replaced = resources.prepare_map_tiles(identity, &moved, &[replacement.clone()]);
    assert_eq!(replaced.current, vec![map_tile_key(&replacement)]);
    assert_eq!(replaced.fallback, vec![stale_key]);
}

#[test]
fn stale_fallback_prefers_recent_and_skips_overlapping_older_tiles() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let parent = TileId {
        zoom: 7,
        x: 3,
        y: 5,
    };
    let child = TileId {
        zoom: 8,
        x: 6,
        y: 10,
    };
    let far = TileId {
        zoom: 8,
        x: 40,
        y: 4,
    };
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();

    let first = transition_selection(vec![(parent, 0)]);
    resources.prepare_map_tiles(identity, &first, &[transition_tile(&first, parent)]);
    let second = transition_selection(vec![(child, 0)]);
    let child_tile = transition_tile(&second, child);
    resources.prepare_map_tiles(identity, &second, &[child_tile.clone()]);

    let third = transition_selection(vec![(far, 0)]);
    let draw = resources.prepare_map_tiles(identity, &third, &[]);
    assert!(draw.current.is_empty());
    assert_eq!(
        draw.fallback,
        vec![map_tile_key(&child_tile)],
        "the newer child covers its area; the stale parent must not z-fight it"
    );
}

#[test]
fn stale_cache_is_bounded_per_scope() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    for round in 0..3_u32 {
        let ids: Vec<_> = (0..128)
            .map(|x| TileId {
                zoom: 8,
                x: round * 128 + x,
                y: 4,
            })
            .collect();
        let selection = transition_selection(
            ids.iter()
                .enumerate()
                .map(|(p, id)| (*id, p as i32))
                .collect(),
        );
        let ready: Vec<_> = ids
            .iter()
            .map(|id| transition_tile(&selection, *id))
            .collect();
        resources.prepare_map_tiles(identity, &selection, &ready);
    }
    let cached = resources.map_tile_cache[&MapScopeId(31)].len();
    assert!(cached <= 2 * MAP_TILE_CAPACITY, "cache holds {cached}");
    assert_eq!(resources.map_tiles.resident_tile_count(), MAP_TILE_CAPACITY);
}

#[test]
fn ready_current_tile_preempts_saturated_fallback() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let parent_ids: Vec<_> = (0..128).map(|x| TileId { zoom: 7, x, y: 5 }).collect();
    let child_ids = [
        TileId {
            zoom: 8,
            x: 6,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 6,
            y: 11,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 11,
        },
    ];
    let selection = transition_selection(
        child_ids
            .iter()
            .enumerate()
            .map(|(p, id)| (*id, p as i32))
            .collect(),
    );
    let fallback: Vec<_> = parent_ids
        .iter()
        .copied()
        .map(|id| transition_tile(&selection, id))
        .collect();
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    let first = resources.prepare_map_tiles(identity, &selection, &fallback);
    assert_eq!(first.fallback.len(), 128);

    let child = transition_tile(&selection, child_ids[0]);
    let child_key = map_tile_key(&child);
    let draw = resources.prepare_map_tiles(identity, &selection, &[child]);
    assert!(draw.current.contains(&child_key));
    assert!(resources.map_tiles.contains(child_key));
    assert_eq!(draw.fallback.len(), 127);
    assert_eq!(resources.map_tiles.resident_tile_count(), 128);
}

#[test]
fn partial_child_uses_spare_slot_over_retained_parent() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let parent_ids: Vec<_> = (0..127).map(|x| TileId { zoom: 7, x, y: 5 }).collect();
    let child_ids = [
        TileId {
            zoom: 8,
            x: 6,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 6,
            y: 11,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 11,
        },
    ];
    let selection = transition_selection(
        child_ids
            .iter()
            .enumerate()
            .map(|(p, id)| (*id, p as i32))
            .collect(),
    );
    let previous: Vec<_> = parent_ids
        .iter()
        .copied()
        .map(|id| transition_tile(&selection, id))
        .collect();
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection, &previous);
    let child = transition_tile(&selection, child_ids[0]);
    let draw = resources.prepare_map_tiles(identity, &selection, &[child.clone()]);
    assert_eq!(draw.fallback.len(), 127);
    assert!(draw.current.contains(&map_tile_key(&child)));
    assert!(
        resources
            .map_tiles
            .contains(map_tile_key(&transition_tile(&selection, parent_ids[3])))
    );
}

#[test]
fn ready_children_and_fallback_share_quota_deterministically() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let parent_ids: Vec<_> = (0..125).map(|x| TileId { zoom: 7, x, y: 5 }).collect();
    let children = [
        TileId {
            zoom: 8,
            x: 6,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 6,
            y: 11,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 11,
        },
    ];
    let selection = transition_selection(
        children
            .iter()
            .enumerate()
            .map(|(priority, id)| (*id, priority as i32))
            .collect(),
    );
    let fallback: Vec<_> = parent_ids
        .iter()
        .copied()
        .map(|id| transition_tile(&selection, id))
        .collect();
    let ready_children: Vec<_> = children
        .iter()
        .map(|id| transition_tile(&selection, *id))
        .collect();
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection, &fallback);
    let draw = resources.prepare_map_tiles(identity, &selection, &ready_children);
    assert_eq!(draw.current.len(), 4);
    assert_eq!(draw.fallback.len(), 124);
    assert!(
        resources
            .map_tiles
            .contains(map_tile_key(&transition_tile(&selection, parent_ids[3])))
    );
    let reversed = resources.prepare_map_tiles(
        identity,
        &selection,
        &ready_children.iter().rev().cloned().collect::<Vec<_>>(),
    );
    assert_eq!(
        reversed, draw,
        "ready arrival order cannot affect admission"
    );
}

#[test]
fn zoom_out_parent_draws_over_retained_fallback_children() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let parent = TileId {
        zoom: 7,
        x: 3,
        y: 5,
    };
    let children = vec![
        TileId {
            zoom: 8,
            x: 6,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 10,
        },
        TileId {
            zoom: 8,
            x: 6,
            y: 11,
        },
        TileId {
            zoom: 8,
            x: 7,
            y: 11,
        },
    ];
    let selection = transition_selection(vec![(parent, 0)]);
    let fallback: Vec<_> = children
        .iter()
        .copied()
        .map(|id| transition_tile(&selection, id))
        .collect();
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection, &fallback);
    let current = transition_tile(&selection, parent);
    let draw = resources.prepare_map_tiles(identity, &selection, &[current.clone()]);
    assert_eq!(draw.current, vec![map_tile_key(&current)]);
    assert_eq!(draw.fallback.len(), 4);
}

#[test]
fn saturated_scope_draws_only_deterministic_first_128_candidates() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = MapTileSelection {
        scope: MapScopeId(32),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: live_zoom(8),
        enabled: true,
    };
    let tile = |x| ReadyTile {
        scope: selection.scope,
        epoch: 1,
        provider: selection.provider,
        id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
        generation: 1,
        rgba: [x as u8, 8, 0, 255].repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let tiles: Vec<_> = (0..140).rev().map(tile).collect();
    let expected: std::collections::HashSet<_> =
        (0..128).map(|x| map_tile_key(&tile(x))).collect();
    let draw = resources.prepare_map_tiles(
        glam::Mat4::IDENTITY.to_cols_array_2d(),
        &selection,
        &tiles,
    );
    assert_eq!(draw.current.len(), 128);
    assert_eq!(
        draw.current
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        expected
    );
    assert!(
        draw.current
            .iter()
            .all(|key| resources.map_tiles.contains(*key))
    );
    assert_eq!(
        resources.map_tile_cache[&selection.scope].len(),
        140,
        "CPU cache retains overflow"
    );
}

#[test]
fn saturated_same_zoom_pan_prefers_current_and_keeps_bounded_stale() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let mut selection = MapTileSelection {
        scope: MapScopeId(33),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: live_zoom(8),
        enabled: true,
    };
    let tile = |x| ReadyTile {
        scope: selection.scope,
        epoch: selection.epoch,
        provider: selection.provider,
        id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
        generation: selection.generation,
        rgba: [x as u8, 8, 0, 255].repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let old: Vec<_> = (0..128).map(tile).collect();
    let new: Vec<_> = (128..256).map(tile).collect();
    let expected: std::collections::HashSet<_> = new.iter().map(map_tile_key).collect();
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    selection.current_tiles = old
        .iter()
        .map(|tile| (tile.id, (tile.id.x % 128) as i32))
        .collect();
    resources.prepare_map_tiles(identity, &selection, &old);
    selection.current_tiles = new
        .iter()
        .map(|tile| (tile.id, (tile.id.x % 128) as i32))
        .collect();
    let draw = resources.prepare_map_tiles(identity, &selection, &new);

    assert_eq!(
        draw.current
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        expected
    );
    assert!(
        draw.fallback.is_empty(),
        "saturated current leaves no stale slots"
    );
    assert_eq!(
        resources.map_tile_cache[&selection.scope].len(),
        256,
        "displaced tiles stay cached as bounded stale fallback"
    );
    assert_eq!(resources.map_tiles.resident_tile_count(), 128);
}

#[test]
fn same_zoom_one_tile_pan_retains_old_nonoverlap_with_exact_current_group() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let id = |x| crate::map::provider::TileId { zoom: 8, x, y: 2 };
    let mut selection = MapTileSelection {
        scope: MapScopeId(34),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: vec![(id(1), 0), (id(2), 1)],
        enabled: true,
    };
    let tile = |x| ReadyTile {
        scope: selection.scope,
        epoch: selection.epoch,
        provider: selection.provider,
        id: id(x),
        generation: selection.generation,
        rgba: [x as u8, 8, 0, 255].repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let old = vec![tile(1), tile(2)];
    let old_key = map_tile_key(&old[0]);
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection, &old);

    selection.current_tiles = vec![(id(2), 0), (id(3), 1)];
    let new = tile(3);
    let expected_current = [map_tile_key(&old[1]), map_tile_key(&new)]
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let draw = resources.prepare_map_tiles(identity, &selection, &[new]);

    assert_eq!(draw.fallback, vec![old_key]);
    assert_eq!(
        draw.current
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        expected_current
    );
    assert!(resources.map_tiles.contains(old_key));
}

#[test]
fn three_saturated_scopes_have_stable_sorted_quotas_and_uploads() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = |scope| MapTileSelection {
        scope: MapScopeId(scope),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: (0..128)
            .map(|x| (crate::map::provider::TileId { zoom: 8, x, y: 0 }, x as i32))
            .collect(),
        enabled: true,
    };
    let tiles = |scope| {
        (0..128)
            .map(|x| ReadyTile {
                scope: MapScopeId(scope),
                epoch: 1,
                provider: crate::map::provider::MapProviderId::BingSatellite,
                id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
                generation: 1,
                rgba: [scope as u8, x as u8, 0, 255].repeat(256 * 256),
                corners: [[x as f32, 0.0, 0.0]; 4],
            })
            .collect::<Vec<_>>()
    };
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    for scope in [30, 10, 20] {
        resources.prepare_map_tiles(identity, &selection(scope), &tiles(scope));
    }
    let warm = resources.map_tiles.upload_count();
    for _ in 0..3 {
        for scope in [30, 10, 20] {
            resources.prepare_map_tiles(identity, &selection(scope), &[]);
        }
    }
    let resident = |scope| {
        resources.map_tile_cache[&MapScopeId(scope)]
            .keys()
            .filter(|key| resources.map_tiles.contains(**key))
            .count()
    };
    assert_eq!((resident(10), resident(20), resident(30)), (43, 43, 42));
    assert_eq!(resources.map_tiles.upload_count(), warm);
}

#[test]
fn saturated_other_scope_cannot_starve_active_scope() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = |scope| MapTileSelection {
        scope: MapScopeId(scope),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: live_zoom(8),
        enabled: true,
    };
    let tile = |scope, x| ReadyTile {
        scope: MapScopeId(scope),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
        generation: 1,
        rgba: [scope as u8, x as u8, 0, 255].repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let a: Vec<_> = (0..128).map(|x| tile(41, x)).collect();
    let b = tile(42, 0);
    let b_key = map_tile_key(&b);
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    resources.prepare_map_tiles(identity, &selection(41), &a);
    let draw_b = resources.prepare_map_tiles(identity, &selection(42), &[b]);
    assert_eq!(draw_b.current, vec![b_key]);
    assert!(resources.map_tiles.contains(b_key));
    assert!(
        draw_b
            .current
            .iter()
            .all(|key| resources.map_tiles.contains(*key))
    );
    let draw_a = resources.prepare_map_tiles(identity, &selection(41), &[]);
    assert_eq!(draw_a.current.len(), 64);
    assert!(
        resources.map_tiles.contains(b_key),
        "alternating panes stabilizes without evicting B"
    );
    let draw_b_again = resources.prepare_map_tiles(identity, &selection(42), &[]);
    assert_eq!(draw_b_again.current, vec![b_key]);
    assert_eq!(resources.map_tiles.resident_tile_count(), 65);
}

#[test]
fn retaining_live_map_scopes_reclaims_closed_scope_quota_and_cache() {
    let Some(ctx) = RenderContext::headless() else {
        return;
    };
    let mut resources = SceneResources::new(ctx);
    let selection = |scope| MapTileSelection {
        scope: MapScopeId(scope),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        generation: 1,
        current_tiles: live_zoom(8),
        enabled: true,
    };
    let tile = |scope, x| ReadyTile {
        scope: MapScopeId(scope),
        epoch: 1,
        provider: crate::map::provider::MapProviderId::BingSatellite,
        id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
        generation: 1,
        rgba: [scope as u8, x as u8, 0, 255].repeat(256 * 256),
        corners: [[x as f32, 0.0, 0.0]; 4],
    };
    let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
    let a: Vec<_> = (0..128).map(|x| tile(51, x)).collect();
    let b: Vec<_> = (0..128).map(|x| tile(52, x)).collect();
    resources.prepare_map_tiles(identity, &selection(51), &a);
    resources.prepare_map_tiles(identity, &selection(52), &b);

    resources.retain_map_scopes(&[MapScopeId(52)]);

    assert!(!resources.map_tile_cache.contains_key(&MapScopeId(51)));
    assert!(!resources.map_tile_selections.contains_key(&MapScopeId(51)));
    assert_eq!(resources.map_tiles.resident_tile_count(), 128);
    let draw_b = resources.prepare_map_tiles(identity, &selection(52), &[]);
    assert_eq!(draw_b.current.len(), 128, "B gets the closed pane's quota");

    resources.retain_map_scopes(&[]);
    assert!(resources.map_tile_cache.is_empty());
    assert!(resources.map_tile_selections.is_empty());
    assert!(resources.map_tile_resident_signatures.is_empty());
    assert_eq!(resources.map_tiles.resident_tile_count(), 0);

    for scope in 60..64 {
        let tiles: Vec<_> = (0..128).map(|x| tile(scope, x)).collect();
        resources.prepare_map_tiles(identity, &selection(scope), &tiles);
        resources.retain_map_scopes(&[]);
        assert!(resources.map_tile_cache.is_empty());
        assert!(resources.map_tile_selections.is_empty());
        assert!(resources.map_tile_resident_signatures.is_empty());
        assert_eq!(resources.map_tiles.resident_tile_count(), 0);
    }

    let disabled_scope = MapScopeId(70);
    resources.prepare_map_tiles(
        identity,
        &MapTileSelection {
            current_tiles: Vec::new(),
            enabled: false,
            ..selection(disabled_scope.0)
        },
        &[],
    );
    resources.retain_map_scopes(&[disabled_scope]);
    assert!(resources.map_tile_cache.is_empty());
    assert!(resources.map_tile_selections.is_empty());
    assert_eq!(resources.map_tiles.resident_tile_count(), 0);
}

#[test]
fn scene_pass_encodes_tiles_before_grid_before_vehicle_overlays() {
    let source = include_str!("mod.rs");
    let pass = source
        .split("let mut pass = res.target.begin_pass")
        .nth(1)
        .expect("scene pass");
    let tiles = pass.find("res.map_tiles.draw").expect("tile draw");
    let grid = pass.find("res.grid.draw").expect("grid draw");
    let vehicles = pass.find("res.draw_vehicles").expect("vehicle draw");
    assert!(tiles < grid && grid < vehicles);
}

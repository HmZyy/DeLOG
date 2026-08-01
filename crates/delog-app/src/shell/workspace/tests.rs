use super::*;

#[test]
fn workspace_starts_with_one_plot_pane() {
    let workspace = Workspace::new();
    assert_eq!(workspace.plot_panes().count(), 1);
    assert!(workspace.fields().next().is_none());
}

#[test]
fn focused_fields_preserve_the_focused_plot_trace_order() {
    let mut workspace = Workspace::new();
    let pane = workspace.tree.root().unwrap();
    workspace.add_trace_to_first_plot(FieldId(7));
    workspace.add_trace_to_first_plot(FieldId(3));
    workspace.focused = Some(pane);

    assert_eq!(workspace.focused_fields(), vec![FieldId(7), FieldId(3)]);
}

#[test]
fn hovered_pane_fields_include_only_visible_traces() {
    let mut workspace = Workspace::new();
    let pane = workspace.tree.root().unwrap();
    workspace.add_trace_to_first_plot(FieldId(7));
    workspace.add_trace_to_first_plot(FieldId(3));
    let Pane::Plot(plot) = workspace
        .tree
        .tiles
        .get_mut(pane)
        .and_then(|tile| match tile {
            egui_tiles::Tile::Pane(pane) => Some(pane),
            _ => None,
        })
        .unwrap()
    else {
        panic!("root should be a plot")
    };
    plot.traces[1].visible = false;

    assert_eq!(workspace.visible_fields(pane), vec![FieldId(7)]);
}

fn render_plot_hover(pane: &mut PlotPane, gpu_available: bool) -> Option<PlotHover> {
    let ctx = egui::Context::default();
    let frame = eframe::Frame::_new_kittest();
    let snapshot = Arc::new(StoreSnapshot::empty());
    let metrics = Arc::new(delog_core::metrics::MetricsRegistry::new());
    let mut gpu = gpu::test_bridge(gpu_available);
    let mut caches = CacheManager::new();
    let mut view = Some(ViewX::new(0, 1_000_000));
    let mut hover_mode = delog_core::field_view::SampleMode::Prev;
    let mut snap_playhead = false;
    let mut marker_us = None;
    let markers = Vec::new();
    let tile_id = egui_tiles::TileId::from_u64(7);
    let mut hovered = None;

    let mut frame_with = |events| {
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                let services = PlotServices {
                    frame: &frame,
                    snapshot: &snapshot,
                    metrics: &metrics,
                    gpu: &mut gpu,
                    tile_manager: None,
                    tile_manager_error: None,
                    caches: &mut caches,
                    view: &mut view,
                    origin_us: 0,
                    hover_mode: &mut hover_mode,
                    snap_playhead: &mut snap_playhead,
                    marker_us: &mut marker_us,
                    render_tuning: crate::config::settings::RenderTuning::default(),
                    scene3d: crate::config::settings::Scene3dSettings::default(),
                    playhead_us: None,
                    playing: false,
                    vehicles: &[],
                    trajectories: &[],
                    traj_generation: 0,
                    shared_y_gutter: 0.0,
                    plot_display: crate::config::settings::PlotDisplay::default(),
                    markers: &markers,
                };
                let mut behavior = Behavior::new(services);
                let _ = behavior.plot_body(ui, tile_id, pane);
                hovered = behavior.into_actions().hovered_cursor;
            },
        );
    };
    frame_with(Vec::new());
    frame_with(vec![egui::Event::PointerMoved(egui::pos2(250.0, 120.0))]);

    hovered
}

#[test]
fn empty_plot_publishes_hover_before_its_rendering_only_return() {
    let mut pane = PlotPane::default();
    pane.show_tooltip = false;

    let hovered = render_plot_hover(&mut pane, false).expect("empty plot hover should publish");

    assert_eq!(hovered.tile_id, egui_tiles::TileId::from_u64(7));
}

#[test]
fn gpu_unavailable_plot_publishes_hover_before_its_rendering_only_return() {
    let mut pane = PlotPane::default();
    pane.show_tooltip = false;
    pane.show_legend = false;
    assert!(pane.add_trace(FieldId(0)));

    let hovered =
        render_plot_hover(&mut pane, false).expect("GPU-unavailable plot hover should publish");

    assert_eq!(hovered.tile_id, egui_tiles::TileId::from_u64(7));
}

#[test]
fn fully_rendered_plot_keeps_publishing_normal_hover() {
    let mut pane = PlotPane::default();
    pane.show_tooltip = false;
    pane.show_legend = false;
    assert!(pane.add_trace(FieldId(0)));

    let hovered = render_plot_hover(&mut pane, true).expect("normal plot hover should publish");

    assert_eq!(hovered.tile_id, egui_tiles::TileId::from_u64(7));
}

#[test]
fn plot_context_menu_keeps_every_existing_action() {
    let source = include_str!("mod.rs");
    for label in [
        "Clear all traces",
        "Remove trace",
        "Field stats",
        "Edit trace",
        "Copy Image",
        "Export PNG...",
        "Split horizontally",
        "Split vertically",
        "Show legend",
        "Show tooltip",
        "Plot Info",
        "Close",
    ] {
        assert!(source.contains(label), "missing plot action: {label}");
    }
}

#[test]
fn data_browser_and_legend_keep_contextual_actions() {
    let browser = include_str!("../../plotting/browser.rs");
    for label in [
        "Source metadata",
        "Remove source",
        "Set exact offset (us)",
        "Field metadata",
        "Field stats",
        "Generate markers",
    ] {
        assert!(browser.contains(label), "missing browser action: {label}");
    }
    let legend = include_str!("../../plotting/legend.rs");
    for label in ["Mode", "Rename", "Remove"] {
        assert!(legend.contains(label), "missing legend action: {label}");
    }
}

#[test]
fn inspector_trace_edits_reuse_workspace_trace_mutation() {
    let mut workspace = Workspace::new();
    let pane = workspace.tree.root().unwrap();
    let field = FieldId(5);
    workspace.add_trace_to_first_plot(field);

    assert!(workspace.set_trace_width(pane, field, 20.0));
    assert!(workspace.set_trace_mode(pane, field, TraceMode::Scatter));
    assert!(workspace.set_trace_label(pane, field, Some("Altitude".into())));
    assert!(workspace.set_trace_color(pane, field, egui::Color32::RED));

    let trace = workspace.trace_ref(pane, field).unwrap();
    assert_eq!(trace.width_px, 12.0);
    assert_eq!(trace.mode, TraceMode::Scatter);
    assert_eq!(trace.label_override.as_deref(), Some("Altitude"));
    assert_eq!(trace.color32(), egui::Color32::RED);
}

#[test]
fn workspace_image_actions_carry_plot_rects() {
    let rect = egui::Rect::from_min_size(egui::pos2(4.0, 8.0), egui::vec2(120.0, 80.0));

    let copy = WorkspaceImageAction::CopyPlot { rect };
    let export = WorkspaceImageAction::ExportPlot { rect };

    assert_eq!(copy.rect(), rect);
    assert_eq!(export.rect(), rect);
}

#[test]
fn prune_removed_fields_drops_traces_for_removed_sources() {
    let mut identity = delog_core::identity::IdentityRegistry::new();
    let keep_source = identity.add_source("keep");
    let drop_source = identity.add_source("drop");
    let keep_topic = identity.add_topic(keep_source, "POS").unwrap();
    let drop_topic = identity.add_topic(drop_source, "POS").unwrap();
    let keep_field = identity.add_field(keep_topic, "Alt").unwrap();
    let drop_field = identity.add_field(drop_topic, "Alt").unwrap();
    identity.remove_source(drop_source);
    let snapshot = StoreSnapshot::from_registry(&identity, [], 1).unwrap();

    let mut workspace = Workspace::new();
    assert!(workspace.add_trace_to_first_plot(keep_field));
    assert!(workspace.add_trace_to_first_plot(drop_field));

    let removed = workspace.prune_removed_fields(&snapshot);

    assert_eq!(removed, vec![drop_field]);
    assert_eq!(workspace.fields().collect::<Vec<_>>(), vec![keep_field]);
}

#[test]
fn prune_removed_fields_rebinds_script_traces_to_recreated_fields() {
    let mut identity = delog_core::identity::IdentityRegistry::new();
    let old_source = identity.add_source("script:calc");
    let old_topic = identity.add_topic(old_source, "Derived").unwrap();
    let old_field = identity.add_field(old_topic, "value").unwrap();
    identity.remove_source(old_source);
    let new_source = identity.add_source("script:calc");
    let new_topic = identity.add_topic(new_source, "Derived").unwrap();
    let new_field = identity.add_field(new_topic, "value").unwrap();
    let snapshot = StoreSnapshot::from_registry(&identity, [], 1).unwrap();

    let mut workspace = Workspace::new();
    assert!(workspace.add_trace_to_first_plot(old_field));
    {
        let pane = workspace.plot_panes_mut().next().unwrap();
        let trace = pane.trace_mut(old_field).unwrap();
        trace.color = [0.1, 0.2, 0.3, 0.4];
        trace.width_px = 3.0;
        trace.mode = TraceMode::Step;
        trace.visible = false;
        pane.text_filters.insert(old_field, "armed".into());
        pane.text_offsets.insert((old_field, 42), 0.75);
    }

    let removed = workspace.prune_removed_fields(&snapshot);

    assert!(removed.is_empty());
    assert_eq!(workspace.fields().collect::<Vec<_>>(), vec![new_field]);
    let pane = workspace.plot_panes().next().unwrap();
    assert_eq!(
        pane.traces[0],
        TraceRef {
            field: new_field,
            color: [0.1, 0.2, 0.3, 0.4],
            width_px: 3.0,
            mode: TraceMode::Step,
            visible: false,
            label_override: None,
        }
    );
    assert_eq!(pane.text_filters.get(&new_field).unwrap(), "armed");
    assert_eq!(pane.text_offsets.get(&(new_field, 42)), Some(&0.75));
    assert!(!pane.text_filters.contains_key(&old_field));
    assert!(
        !pane
            .text_offsets
            .keys()
            .any(|(field, _time)| *field == old_field)
    );
}

#[test]
fn prune_removed_fields_keeps_script_trace_until_recreated_field_appears() {
    let mut identity = delog_core::identity::IdentityRegistry::new();
    let old_source = identity.add_source("script:calc");
    let old_topic = identity.add_topic(old_source, "Derived").unwrap();
    let old_field = identity.add_field(old_topic, "value").unwrap();

    let mut workspace = Workspace::new();
    assert!(workspace.add_trace_to_first_plot(old_field));
    {
        let pane = workspace.plot_panes_mut().next().unwrap();
        let trace = pane.trace_mut(old_field).unwrap();
        trace.color = [0.4, 0.3, 0.2, 0.1];
        trace.width_px = 4.0;
        trace.mode = TraceMode::Scatter;
        trace.visible = false;
        pane.text_filters.insert(old_field, "armed".into());
        pane.text_offsets.insert((old_field, 42), 0.75);
    }

    identity.remove_source(old_source);
    let removed_snapshot = StoreSnapshot::from_registry(&identity, [], 1).unwrap();
    let removed = workspace.prune_removed_fields(&removed_snapshot);

    assert_eq!(removed, vec![old_field]);
    assert!(workspace.fields().next().is_none());
    {
        let pane = workspace.plot_panes().next().unwrap();
        assert_eq!(pane.ghosts.len(), 1);
        assert_eq!(pane.ghosts[0].source.as_deref(), Some("script:calc"));
        assert_eq!(pane.ghosts[0].topic, "Derived");
        assert_eq!(pane.ghosts[0].field, "value");
        assert_eq!(pane.ghosts[0].color, [0.4, 0.3, 0.2, 0.1]);
        assert_eq!(pane.ghosts[0].width_px, 4.0);
        assert_eq!(pane.ghosts[0].mode, TraceMode::Scatter);
        assert!(!pane.ghosts[0].visible);
        assert_eq!(pane.ghosts[0].text_filter.as_deref(), Some("armed"));
        assert_eq!(pane.ghosts[0].text_offsets, vec![(42, 0.75)]);
    }

    let new_source = identity.add_source("script:calc");
    let new_topic = identity.add_topic(new_source, "Derived").unwrap();
    let new_field = identity.add_field(new_topic, "value").unwrap();
    let recreated_snapshot = StoreSnapshot::from_registry(&identity, [], 2).unwrap();

    assert_eq!(workspace.resolve_ghosts(&recreated_snapshot), 1);

    let pane = workspace.plot_panes().next().unwrap();
    assert!(pane.ghosts.is_empty());
    assert_eq!(pane.traces.len(), 1);
    assert_eq!(
        pane.traces[0],
        TraceRef {
            field: new_field,
            color: [0.4, 0.3, 0.2, 0.1],
            width_px: 4.0,
            mode: TraceMode::Scatter,
            visible: false,
            label_override: None,
        }
    );
    assert_eq!(pane.text_filters.get(&new_field).unwrap(), "armed");
    assert_eq!(pane.text_offsets.get(&(new_field, 42)), Some(&0.75));
}

#[test]
fn scene_pane_toggles_a_single_instance_on_and_off() {
    fn scene_count(w: &Workspace) -> usize {
        w.tree
            .tiles
            .tiles()
            .filter(|t| matches!(t, egui_tiles::Tile::Pane(Pane::Scene3D(_))))
            .count()
    }

    let mut workspace = Workspace::new();
    assert!(workspace.scene_pane_id().is_none());

    workspace.toggle_scene_pane();
    let id = workspace.scene_pane_id().expect("scene pane should exist");
    assert_eq!(scene_count(&workspace), 1);
    assert_eq!(workspace.plot_panes().count(), 1);

    workspace.toggle_scene_pane();
    assert!(workspace.scene_pane_id().is_none());
    assert_eq!(workspace.plot_panes().count(), 1);

    // A fresh show reuses the single-instance path (never two) with a new id.
    workspace.toggle_scene_pane();
    assert_eq!(scene_count(&workspace), 1);
    assert_ne!(workspace.scene_pane_id(), Some(id));
}

#[test]
fn closing_the_focused_plot_reassigns_focus_to_the_surviving_plot() {
    let mut workspace = Workspace::new();
    let closing = workspace.tree.root().unwrap();
    workspace.split_plot(closing, SplitDirection::Horizontal);
    let surviving = workspace
        .tree
        .tiles
        .iter()
        .find_map(|(id, tile)| {
            (*id != closing && matches!(tile, egui_tiles::Tile::Pane(Pane::Plot(_))))
                .then_some(*id)
        })
        .expect("split should create a surviving plot");
    workspace.focused = Some(closing);

    workspace.close_plot(closing);

    assert_eq!(workspace.focused, Some(surviving));
}

#[test]
fn focus_repair_chooses_the_lowest_surviving_plot_id() {
    let mut workspace = Workspace::new();
    let closing = workspace.tree.root().unwrap();
    workspace.split_plot(closing, SplitDirection::Horizontal);
    workspace.split_plot(closing, SplitDirection::Horizontal);
    let expected = workspace
        .tree
        .tiles
        .iter()
        .filter_map(|(id, tile)| {
            (*id != closing && matches!(tile, egui_tiles::Tile::Pane(Pane::Plot(_))))
                .then_some(*id)
        })
        .min_by_key(|id| id.0)
        .expect("two plots should survive");
    workspace.focused = Some(closing);
    workspace.tree.remove_recursively(closing);
    assert_eq!(workspace.deterministic_plot_fallback(), Some(expected));

    workspace.repair_focus();

    assert_eq!(workspace.focused, Some(expected));
}

#[test]
fn closing_the_focused_scene_reassigns_focus_to_the_surviving_plot() {
    let mut workspace = Workspace::new();
    let plot = workspace.tree.root().unwrap();
    workspace.toggle_scene_pane();
    let scene = workspace.scene_pane_id().expect("scene should be open");
    assert_eq!(workspace.focused, Some(scene));

    workspace.toggle_scene_pane();

    assert_eq!(workspace.focused, Some(plot));
}

#[test]
fn inspector_fallback_accepts_only_an_existing_focused_plot() {
    let mut workspace = Workspace::new();
    let plot = workspace.tree.root().unwrap();
    workspace.focused = Some(plot);
    assert_eq!(workspace.focused_plot_id(), Some(plot));

    workspace.toggle_scene_pane();
    assert!(workspace.focused_plot_id().is_none());

    workspace.focused = Some(egui_tiles::TileId::from_u64(u64::MAX));
    assert!(workspace.focused_plot_id().is_none());
}

#[test]
fn scene_splits_at_root_not_inside_the_focused_pane() {
    let mut workspace = Workspace::new();
    let pane1 = workspace.tree.root().unwrap();
    workspace.split_plot(pane1, SplitDirection::Vertical);
    let inner = workspace.tree.root().unwrap();

    // Focus the nested plot: the buggy path split here instead of globally.
    workspace.focused = Some(pane1);
    workspace.toggle_scene_pane();

    let scene = workspace.scene_pane_id().expect("scene pane should exist");
    let root = workspace.tree.root().unwrap();
    assert_eq!(
        workspace.tree.tiles.parent_of(scene),
        Some(root),
        "scene must sit directly under the root, beside the whole layout",
    );
    let Some(egui_tiles::Tile::Container(root_container)) = workspace.tree.tiles.get(root)
    else {
        panic!("root should be a container wrapping the layout and the scene");
    };
    assert_eq!(root_container.num_children(), 2);
    // The previous layout stays intact as a single sibling of the scene.
    assert_eq!(workspace.tree.tiles.parent_of(pane1), Some(inner));
}

#[test]
fn first_visible_vehicle_skips_missing_poses() {
    let poses = [
        None,
        Some(vehicle::Pose {
            pos: glam::Vec3::X,
            rot: glam::Mat3::IDENTITY,
        }),
        Some(vehicle::Pose {
            pos: glam::Vec3::Y,
            rot: glam::Mat3::IDENTITY,
        }),
    ];

    assert_eq!(first_visible_vehicle(&poses), Some(1));
    assert_eq!(first_visible_vehicle(&[None, None]), None);
}

#[test]
fn scene_map_overlay_only_reports_actionable_states() {
    assert_eq!(
        scene_map_overlay(false, None, None, false).as_deref(),
        Some("Map unavailable: no georeference")
    );
    assert_eq!(
        scene_map_overlay(true, None, Some(TileFailureClass::Cache), true).as_deref(),
        Some("Map cache error")
    );
    assert_eq!(
        scene_map_overlay(true, None, Some(TileFailureClass::NetworkTransient), true)
            .as_deref(),
        Some("Map tiles offline - showing cached imagery")
    );
    assert_eq!(scene_map_overlay(true, None, None, true), None);
    assert_eq!(
        scene_map_overlay(true, Some("permission denied"), None, false).as_deref(),
        Some("Map cache error")
    );
    assert_eq!(
        scene_map_overlay(true, None, Some(TileFailureClass::NetworkTransient), false),
        None
    );
}

#[test]
fn scene_map_tracked_vehicle_switch_changes_generation() {
    let mut pane = Scene3dPane::default();
    let first = pane.update_map_selection(Some((0, MapProviderId::BingSatellite, [0; 3])));
    assert_eq!(
        pane.update_map_selection(Some((0, MapProviderId::BingSatellite, [0; 3]))),
        first
    );
    assert!(pane.update_map_selection(Some((1, MapProviderId::BingSatellite, [0; 3]))) > first);
}

#[test]
fn scene_map_selection_change_clears_current_tiles() {
    let tile = |zoom, x| crate::map::provider::TileId { zoom, x, y: 4 };
    let selection = Some((0, MapProviderId::BingSatellite, [0; 3]));
    let mut pane = Scene3dPane::default();
    let generation = pane.update_map_selection(selection);
    pane.update_visible_map_tiles(vec![tile(8, 1)]);
    pane.update_visible_map_tiles(vec![tile(9, 2)]);

    assert_eq!(pane.update_map_selection(selection), generation);
    assert_eq!(pane.map_tiles, vec![(tile(9, 2), 0)]);

    assert!(
        pane.update_map_selection(Some((1, MapProviderId::BingSatellite, [1; 3]))) > generation
    );
    assert!(pane.map_tiles.is_empty());
}

#[test]
fn visible_map_tiles_are_stored_in_priority_order() {
    let tile = |x| crate::map::provider::TileId { zoom: 8, x, y: 4 };
    let mut pane = Scene3dPane::default();
    pane.update_visible_map_tiles(vec![tile(3), tile(1), tile(2)]);
    assert_eq!(
        pane.map_tiles,
        vec![(tile(3), 0), (tile(1), 1), (tile(2), 2)]
    );
}

#[test]
fn scene_map_none_provider_or_reference_produces_no_selection() {
    let mut pane = Scene3dPane::default();
    pane.update_visible_map_tiles(vec![crate::map::provider::TileId {
        zoom: 8,
        x: 1,
        y: 1,
    }]);
    assert_eq!(pane.update_map_selection(None), 0);
    assert!(pane.map_selection.is_none());
    assert!(pane.map_tiles.is_empty());
}

#[test]
fn ghost_trace_resolves_when_matching_field_loads() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should be a plot");
    };
    pane.add_ghost(crate::plotting::plot::GhostTrace {
        source: None,
        topic: "ATT".into(),
        field: "Roll".into(),
        color: [1.0, 0.0, 0.0, 1.0],
        width_px: 2.0,
        mode: TraceMode::Step,
        visible: false,
        text_filter: None,
        text_offsets: Vec::new(),
    });

    let mut ids = delog_core::identity::IdentityRegistry::new();
    let source = ids.add_source("flight");
    let topic = ids.add_topic(source, "ATT").unwrap();
    let field = ids.add_field(topic, "Roll").unwrap();
    let snapshot = StoreSnapshot::from_registry(&ids, [], 0).unwrap();

    assert_eq!(workspace.resolve_ghosts(&snapshot), 1);
    let pane = match workspace.tree.tiles.get(root).unwrap() {
        egui_tiles::Tile::Pane(Pane::Plot(pane)) => pane,
        _ => panic!("root should remain a plot"),
    };
    assert!(pane.ghosts.is_empty());
    assert_eq!(pane.traces.len(), 1);
    assert_eq!(pane.traces[0].field, field);
    assert_eq!(pane.traces[0].mode, TraceMode::Step);
    assert!(!pane.traces[0].visible);
}

#[test]
fn ghost_trace_stays_missing_when_field_is_ambiguous() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should be a plot");
    };
    pane.add_ghost(crate::plotting::plot::GhostTrace {
        source: None,
        topic: "ATT".into(),
        field: "Roll".into(),
        color: [0.0, 1.0, 0.0, 1.0],
        width_px: 1.0,
        mode: TraceMode::Line,
        visible: true,
        text_filter: None,
        text_offsets: Vec::new(),
    });

    let mut ids = delog_core::identity::IdentityRegistry::new();
    for source_name in ["left", "right"] {
        let source = ids.add_source(source_name);
        let topic = ids.add_topic(source, "ATT").unwrap();
        ids.add_field(topic, "Roll").unwrap();
    }
    let snapshot = StoreSnapshot::from_registry(&ids, [], 0).unwrap();

    assert_eq!(workspace.resolve_ghosts(&snapshot), 0);
    let pane = match workspace.tree.tiles.get(root).unwrap() {
        egui_tiles::Tile::Pane(Pane::Plot(pane)) => pane,
        _ => panic!("root should remain a plot"),
    };
    assert!(pane.traces.is_empty());
    assert_eq!(pane.ghosts.len(), 1);
    assert_eq!(pane.ghosts[0].topic, "ATT");
    assert_eq!(pane.ghosts[0].field, "Roll");
}

#[test]
fn split_root_adds_a_second_plot_pane_under_linear_root() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();

    workspace.split_plot(root, SplitDirection::Horizontal);

    assert_eq!(workspace.plot_panes().count(), 2);
    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Container(container)) = workspace.tree.tiles.get(root) else {
        panic!("root should be a container after split");
    };
    assert_eq!(container.kind(), egui_tiles::ContainerKind::Horizontal);
    assert_eq!(container.num_children(), 2);
}

#[test]
fn split_child_with_new_direction_wraps_the_pane() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Horizontal);
    let first_pane = workspace
        .tree
        .tiles
        .iter()
        .find_map(|(id, tile)| matches!(tile, egui_tiles::Tile::Pane(_)).then_some(*id))
        .unwrap();

    workspace.split_plot(first_pane, SplitDirection::Vertical);

    assert_eq!(workspace.plot_panes().count(), 3);
    assert!(workspace.tree.tiles.tiles().any(|tile| matches!(
        tile,
        egui_tiles::Tile::Container(container)
            if container.kind() == egui_tiles::ContainerKind::Vertical
                && container.num_children() == 2
    )));
}

#[test]
fn cross_direction_split_keeps_the_wrapped_pane_in_its_slot() {
    // Root vertical: pane 1 on top, pane 2 on the bottom.
    let mut workspace = Workspace::new();
    let pane1 = workspace.tree.root().unwrap();
    workspace.split_plot(pane1, SplitDirection::Vertical);

    let root = workspace.tree.root().unwrap();
    let top_children = match workspace.tree.tiles.get(root) {
        Some(egui_tiles::Tile::Container(c)) => c.children_vec(),
        _ => panic!("root should be a vertical container"),
    };
    assert_eq!(top_children[0], pane1, "pane 1 starts on top");

    // Split the TOP pane horizontally: the new horizontal wrapper must
    // stay in the top slot, not get appended to the bottom.
    workspace.split_plot(pane1, SplitDirection::Horizontal);

    let children = match workspace.tree.tiles.get(root) {
        Some(egui_tiles::Tile::Container(c)) => c.children_vec(),
        _ => panic!("root should still be a vertical container"),
    };
    assert_eq!(children.len(), 2);
    let Some(egui_tiles::Tile::Container(wrapper)) = workspace.tree.tiles.get(children[0])
    else {
        panic!("the top slot should hold the new horizontal wrapper");
    };
    assert_eq!(wrapper.kind(), egui_tiles::ContainerKind::Horizontal);
    assert!(wrapper.has_child(pane1), "pane 1 stays inside its wrapper");
    assert_eq!(children[1], top_children[1], "pane 2 stays on the bottom");
}

#[test]
fn edge_drop_splits_root_and_adds_all_dropped_traces_to_new_pane() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();

    let added =
        workspace.split_plot_with_traces(root, DropEdge::Left, &[FieldId(7), FieldId(9)]);
    assert_eq!(added, vec![FieldId(7), FieldId(9)]);

    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Container(container)) = workspace.tree.tiles.get(root) else {
        panic!("root should be a container after edge split");
    };
    assert_eq!(container.kind(), egui_tiles::ContainerKind::Horizontal);
    let children = container.children_vec();
    let new_pane = children[0];
    let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get(new_pane)
    else {
        panic!("left child should be the new plot pane");
    };
    assert_eq!(
        pane.fields().collect::<Vec<_>>(),
        vec![FieldId(7), FieldId(9)]
    );

    let before = workspace.plot_panes().count();
    assert!(
        workspace
            .split_plot_with_traces(new_pane, DropEdge::Right, &[])
            .is_empty()
    );
    assert_eq!(workspace.plot_panes().count(), before);
}

#[test]
fn set_all_plot_legends_updates_existing_and_future_plots() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Horizontal);

    workspace.set_all_plot_legends(false);
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Vertical);
    assert!(workspace.plot_panes().all(|pane| !pane.show_legend));

    workspace.set_all_plot_legends(true);
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Vertical);
    assert!(workspace.plot_panes().all(|pane| pane.show_legend));
}

#[test]
fn all_plot_legends_visible_requires_every_plot() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Horizontal);
    assert!(workspace.all_plot_legends_visible());

    workspace.plot_panes_mut().next().unwrap().show_legend = false;

    assert!(!workspace.all_plot_legends_visible());
}

#[test]
fn equalize_plot_heights_resets_vertical_split_shares() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Vertical);
    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
        workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should be a linear split");
    };
    assert_eq!(linear.dir, egui_tiles::LinearDir::Vertical);
    let children = linear.children.clone();
    linear.shares.set_share(children[0], 4.0);
    linear.shares.set_share(children[1], 1.0);

    workspace.equalize_plot_heights();

    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
        workspace.tree.tiles.get(root)
    else {
        panic!("root should remain a linear split");
    };
    assert_eq!(linear.shares[children[0]], 1.0);
    assert_eq!(linear.shares[children[1]], 1.0);
}

#[test]
fn equalize_plot_heights_resets_horizontal_split_shares() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    workspace.split_plot(root, SplitDirection::Horizontal);
    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
        workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should be a linear split");
    };
    assert_eq!(linear.dir, egui_tiles::LinearDir::Horizontal);
    let children = linear.children.clone();
    linear.shares.set_share(children[0], 1.0);
    linear.shares.set_share(children[1], 6.0);

    workspace.equalize_plot_heights();

    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
        workspace.tree.tiles.get(root)
    else {
        panic!("root should remain a linear split");
    };
    assert_eq!(linear.shares[children[0]], 1.0);
    assert_eq!(linear.shares[children[1]], 1.0);
}

#[test]
fn equalize_plot_heights_resets_grid_row_shares() {
    let mut tiles = egui_tiles::Tiles::default();
    let panes = (0..4)
        .map(|_| tiles.insert_pane(Pane::Plot(PlotPane::default())))
        .collect::<Vec<_>>();
    let root = tiles.insert_container(egui_tiles::Container::new(
        egui_tiles::ContainerKind::Grid,
        panes,
    ));
    let mut workspace = Workspace {
        tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
        focused: None,
        shared_y_gutter: 0.0,
        default_show_legend: true,
    };
    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
        workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should be a grid");
    };
    grid.row_shares = vec![3.0, 1.0];

    workspace.equalize_plot_heights();

    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
        workspace.tree.tiles.get(root)
    else {
        panic!("root should remain a grid");
    };
    assert_eq!(grid.row_shares, vec![1.0, 1.0]);
}

#[test]
fn equalize_plot_heights_resets_grid_column_shares() {
    let mut tiles = egui_tiles::Tiles::default();
    let panes = (0..4)
        .map(|_| tiles.insert_pane(Pane::Plot(PlotPane::default())))
        .collect::<Vec<_>>();
    let root = tiles.insert_container(egui_tiles::Container::new(
        egui_tiles::ContainerKind::Grid,
        panes,
    ));
    let mut workspace = Workspace {
        tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
        focused: None,
        shared_y_gutter: 0.0,
        default_show_legend: true,
    };
    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
        workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should be a grid");
    };
    grid.col_shares = vec![1.0, 4.0];

    workspace.equalize_plot_heights();

    let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
        workspace.tree.tiles.get(root)
    else {
        panic!("root should remain a grid");
    };
    assert_eq!(grid.col_shares, vec![1.0, 1.0]);
}

#[test]
fn equalize_plot_heights_descends_into_multiple_columns() {
    let mut tiles = egui_tiles::Tiles::default();
    let left_a = tiles.insert_pane(Pane::Plot(PlotPane::default()));
    let left_b = tiles.insert_pane(Pane::Plot(PlotPane::default()));
    let right_a = tiles.insert_pane(Pane::Plot(PlotPane::default()));
    let right_b = tiles.insert_pane(Pane::Plot(PlotPane::default()));
    let left = tiles.insert_container(egui_tiles::Container::new(
        egui_tiles::ContainerKind::Vertical,
        vec![left_a, left_b],
    ));
    let right = tiles.insert_container(egui_tiles::Container::new(
        egui_tiles::ContainerKind::Vertical,
        vec![right_a, right_b],
    ));
    let root = tiles.insert_container(egui_tiles::Container::new(
        egui_tiles::ContainerKind::Horizontal,
        vec![left, right],
    ));
    let mut workspace = Workspace {
        tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
        focused: None,
        shared_y_gutter: 0.0,
        default_show_legend: true,
    };
    for (id, first, second) in [(left, left_a, left_b), (right, right_a, right_b)] {
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
            workspace.tree.tiles.get_mut(id)
        else {
            panic!("column should be a linear split");
        };
        linear.shares.set_share(first, 2.0);
        linear.shares.set_share(second, 5.0);
    }

    workspace.equalize_plot_heights();

    for (id, first, second) in [(left, left_a, left_b), (right, right_a, right_b)] {
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
            workspace.tree.tiles.get(id)
        else {
            panic!("column should remain a linear split");
        };
        assert_eq!(linear.shares[first], 1.0);
        assert_eq!(linear.shares[second], 1.0);
    }
}

#[test]
fn drop_edge_prefers_the_nearest_edge_inside_the_threshold() {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 100.0));
    assert_eq!(
        DropEdge::from_pos(rect, egui::pos2(3.0, 50.0)),
        Some(DropEdge::Left)
    );
    assert_eq!(
        DropEdge::from_pos(rect, egui::pos2(197.0, 50.0)),
        Some(DropEdge::Right)
    );
    assert_eq!(DropEdge::from_pos(rect, rect.center()), None);
}

#[test]
fn close_plot_removes_its_fields_and_keeps_a_workspace_alive() {
    let mut workspace = Workspace::new();
    let root = workspace.tree.root().unwrap();
    let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get_mut(root)
    else {
        panic!("root should start as a pane");
    };
    pane.add_trace(FieldId(42));

    let removed = workspace.close_plot(root);

    assert_eq!(removed, vec![FieldId(42)]);
    assert_eq!(workspace.plot_panes().count(), 1);
    assert!(workspace.fields().next().is_none());
}

fn plot_tile_ids(ws: &Workspace) -> Vec<egui_tiles::TileId> {
    ws.tree
        .tiles
        .iter()
        .filter_map(|(id, tile)| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(_)) => Some(*id),
            _ => None,
        })
        .collect()
}

fn seed_trace(ws: &mut Workspace, tile: egui_tiles::TileId, trace: TraceRef) {
    if let Some(egui_tiles::Tile::Pane(Pane::Plot(p))) = ws.tree.tiles.get_mut(tile) {
        p.add_trace_ref(trace);
    } else {
        panic!("tile is not a plot pane");
    }
}

fn trace_of(ws: &Workspace, tile: egui_tiles::TileId, field: FieldId) -> Option<TraceRef> {
    match ws.tree.tiles.get(tile) {
        Some(egui_tiles::Tile::Pane(Pane::Plot(p))) => {
            p.traces.iter().find(|t| t.field == field).cloned()
        }
        _ => None,
    }
}

#[test]
fn legend_move_edge_recolors_and_removes_emptied_source() {
    let mut ws = Workspace::new();
    let source = ws.tree.root().unwrap();
    seed_trace(
        &mut ws,
        source,
        TraceRef {
            field: FieldId(3),
            color: [0.2, 0.4, 0.6, 1.0],
            width_px: 5.0,
            mode: TraceMode::Step,
            visible: false,
            label_override: Some("renamed".to_string()),
        },
    );
    let trace = trace_of(&ws, source, FieldId(3)).unwrap();

    let returned = ws.apply_legend_move(LegendMove {
        source,
        target: source,
        edge: Some(DropEdge::Right),
        trace,
    });
    assert_eq!(returned, FieldId(3));

    // The emptied source pane is dropped.
    assert!(!plot_tile_ids(&ws).contains(&source));
    let holders: Vec<_> = plot_tile_ids(&ws)
        .into_iter()
        .filter(|id| trace_of(&ws, *id, FieldId(3)).is_some())
        .collect();
    assert_eq!(holders.len(), 1);
    let moved = trace_of(&ws, holders[0], FieldId(3)).unwrap();
    // width/mode/name travel; color is reassigned to the new pane's slot 0.
    assert_eq!(moved.width_px, 5.0);
    assert_eq!(moved.mode, TraceMode::Step);
    assert!(!moved.visible);
    assert_eq!(moved.label_override.as_deref(), Some("renamed"));
    assert_eq!(
        moved.color,
        delog_render::palette::trace_color(0).to_srgb_f32()
    );
}

#[test]
fn legend_move_center_recolors_to_target_palette_slot() {
    let mut ws = Workspace::new();
    let source = ws.tree.root().unwrap();
    seed_trace(
        &mut ws,
        source,
        TraceRef {
            field: FieldId(5),
            color: [0.9, 0.1, 0.1, 1.0],
            width_px: 3.0,
            mode: TraceMode::Step,
            visible: true,
            label_override: Some("keep".to_string()),
        },
    );
    ws.split_plot(source, SplitDirection::Horizontal);
    let target = plot_tile_ids(&ws)
        .into_iter()
        .find(|id| *id != source)
        .unwrap();
    for field in [FieldId(1), FieldId(2)] {
        seed_trace(
            &mut ws,
            target,
            TraceRef {
                field,
                color: [0.0, 0.0, 0.0, 1.0],
                width_px: 1.0,
                mode: TraceMode::Line,
                visible: true,
                label_override: None,
            },
        );
    }
    let trace = trace_of(&ws, source, FieldId(5)).unwrap();

    ws.apply_legend_move(LegendMove {
        source,
        target,
        edge: None,
        trace,
    });

    let moved = trace_of(&ws, target, FieldId(5)).unwrap();
    // Target already held 2 traces, so the moved one takes palette slot 2.
    assert_eq!(
        moved.color,
        delog_render::palette::trace_color(2).to_srgb_f32()
    );
    assert_eq!(moved.width_px, 3.0);
    assert_eq!(moved.mode, TraceMode::Step);
    assert_eq!(moved.label_override.as_deref(), Some("keep"));
}

#[test]
fn legend_move_keeps_source_when_traces_remain() {
    let mut ws = Workspace::new();
    let source = ws.tree.root().unwrap();
    for field in [FieldId(1), FieldId(2)] {
        seed_trace(
            &mut ws,
            source,
            TraceRef {
                field,
                color: [0.1, 0.1, 0.1, 1.0],
                width_px: 1.0,
                mode: TraceMode::Line,
                visible: true,
                label_override: None,
            },
        );
    }
    ws.split_plot(source, SplitDirection::Horizontal);
    let target = plot_tile_ids(&ws)
        .into_iter()
        .find(|id| *id != source)
        .unwrap();
    let trace = trace_of(&ws, source, FieldId(1)).unwrap();

    ws.apply_legend_move(LegendMove {
        source,
        target,
        edge: None,
        trace,
    });

    // Source still holds its other trace and stays alive.
    assert!(plot_tile_ids(&ws).contains(&source));
    assert!(trace_of(&ws, source, FieldId(1)).is_none());
    assert!(trace_of(&ws, source, FieldId(2)).is_some());
    assert!(trace_of(&ws, target, FieldId(1)).is_some());
}

#[test]
fn legend_move_center_into_pane_with_same_field_dedups_and_keeps_target() {
    let mut ws = Workspace::new();
    let source = ws.tree.root().unwrap();
    seed_trace(
        &mut ws,
        source,
        TraceRef {
            field: FieldId(1),
            color: [1.0, 0.0, 0.0, 1.0],
            width_px: 2.0,
            mode: TraceMode::Line,
            visible: true,
            label_override: Some("A".to_string()),
        },
    );
    ws.split_plot(source, SplitDirection::Horizontal);
    let target = plot_tile_ids(&ws)
        .into_iter()
        .find(|id| *id != source)
        .unwrap();
    seed_trace(
        &mut ws,
        target,
        TraceRef {
            field: FieldId(1),
            color: [0.0, 1.0, 0.0, 1.0],
            width_px: 8.0,
            mode: TraceMode::Scatter,
            visible: true,
            label_override: Some("B".to_string()),
        },
    );
    let moved = trace_of(&ws, source, FieldId(1)).unwrap();

    ws.apply_legend_move(LegendMove {
        source,
        target,
        edge: None,
        trace: moved,
    });

    assert!(trace_of(&ws, source, FieldId(1)).is_none());
    let kept = trace_of(&ws, target, FieldId(1)).unwrap();
    assert_eq!(kept.label_override.as_deref(), Some("B"));
    assert_eq!(kept.width_px, 8.0);
}

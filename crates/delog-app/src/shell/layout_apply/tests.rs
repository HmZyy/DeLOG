use super::*;
use delog_core::identity::{FieldId, IdentityRegistry};

#[test]
fn externally_owned_window_and_plot_round_trip_by_name() {
    use crate::shell::windows::{ExtendedWindow, WindowId};
    use delog_api::control::ResourceOwner;

    let owner = ResourceOwner {
        name: "flight-diagnosis".into(),
        generation: 7,
    };
    let mut window = ExtendedWindow::new(WindowId(1));
    window.owner = Some(owner.clone());
    window.workspace.plot_panes_mut().next().unwrap().owner = Some(owner);
    let workspace = Workspace::new();
    let snapshot = StoreSnapshot::empty();
    let doc = current_doc(CurrentLayout {
        name: "owned".into(),
        workspace: &workspace,
        windows: &[window],
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });
    assert_eq!(doc.windows[0].owner.as_deref(), Some("flight-diagnosis"));
    let LayoutNode::Plot { owner, .. } = &doc.windows[0].root else {
        panic!("plot");
    };
    assert_eq!(owner.as_deref(), Some("flight-diagnosis"));
    let doc = crate::config::layout::doc::decode_doc(
        &crate::config::layout::doc::doc_json(&doc).unwrap(),
    )
    .unwrap();
    let LoadOutcome::Applied(restored) = load_doc(doc, &snapshot).unwrap() else {
        panic!("applied");
    };
    let expected = ResourceOwner {
        name: "flight-diagnosis".into(),
        generation: 0,
    };
    assert_eq!(restored.windows[0].owner.as_ref(), Some(&expected));
    assert_eq!(
        restored.windows[0]
            .workspace
            .plot_panes()
            .next()
            .unwrap()
            .owner
            .as_ref(),
        Some(&expected)
    );
    assert_eq!(restored.workspace.plot_panes().next().unwrap().owner, None);
}

#[test]
fn owned_traces_survive_layout_restore_before_and_after_schema_arrives() {
    use delog_api::control::ResourceOwner;
    let snapshot = snapshot_with_topics(&[("flight", "ATT", &["roll"])]);
    let mut workspace = Workspace::new();
    let pane = workspace.plot_panes_mut().next().unwrap();
    pane.add_trace(FieldId(0));
    pane.traces[0].owner = Some(ResourceOwner {
        name: "analysis".into(),
        generation: 9,
    });
    let doc = current_doc(CurrentLayout {
        name: "owned".into(),
        workspace: &workspace,
        windows: &[],
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });
    let LayoutNode::Plot { traces, .. } = &doc.workspace.root else {
        panic!("plot");
    };
    assert_eq!(traces[0].owner.as_deref(), Some("analysis"));
    let expected = ResourceOwner {
        name: "analysis".into(),
        generation: 0,
    };
    let LoadOutcome::Applied(restored) = load_doc(doc.clone(), &snapshot).unwrap() else {
        panic!("applied");
    };
    assert_eq!(
        restored.workspace.plot_panes().next().unwrap().traces[0]
            .owner
            .as_ref(),
        Some(&expected)
    );
    let LoadOutcome::Applied(mut restored) = load_doc(doc, &StoreSnapshot::empty()).unwrap() else {
        panic!("applied");
    };
    assert_eq!(
        restored.workspace.plot_panes().next().unwrap().ghosts[0]
            .owner
            .as_ref(),
        Some(&expected)
    );
    let saved_ghost = current_doc(CurrentLayout {
        name: "ghost".into(),
        workspace: &restored.workspace,
        windows: &[],
        snapshot: &StoreSnapshot::empty(),
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });
    let LayoutNode::Plot { traces, .. } = &saved_ghost.workspace.root else {
        panic!("plot");
    };
    assert_eq!(traces[0].owner.as_deref(), Some("analysis"));
    assert_eq!(restored.workspace.resolve_ghosts(&snapshot), 1);
    assert_eq!(
        restored.workspace.plot_panes().next().unwrap().traces[0]
            .owner
            .as_ref(),
        Some(&expected)
    );
}

#[test]
fn legacy_transient_state_is_ignored_and_not_reserialized() {
    let doc = crate::config::layout::doc::decode_doc(
        r#"{
            "delog_layout": 1,
            "name": "legacy-transient",
            "view": {"mode": "window", "min_us": 10, "max_us": 20},
            "playback": {"speed": 2.0, "follow_live": false},
            "workspace": {
                "root": {
                    "plot": {
                        "traces": [],
                        "show_legend": true,
                        "show_tooltip": true,
                        "marker_us": 15,
                        "text_offsets": [{"field": {"topic": "MSG", "field": "Text"}, "t_us": 15, "y_frac": 0.4}],
                        "text_filters": [{"field": {"topic": "MSG", "field": "Text"}, "filter": "armed"}]
                    }
                }
            },
            "vehicles": [],
            "marker_us": 15,
            "markers": [{"t_us": 15, "label": "M", "color": [1.0, 0.0, 0.0, 1.0], "note": ""}],
            "favorites": [],
            "docks": {}
        }"#,
    )
    .expect("legacy transient fields should be accepted");

    let json = crate::config::layout::doc::doc_json(&doc).expect("reserialize");
    for removed in [
        "\"view\"",
        "\"marker_us\"",
        "\"markers\"",
        "\"favorites\"",
        "\"docks\"",
        "\"text_offsets\"",
        "\"text_filters\"",
    ] {
        assert!(!json.contains(removed), "layout still writes {removed}");
    }

    let LoadOutcome::Applied(layout) =
        load_doc(doc, &StoreSnapshot::empty()).expect("legacy layout should apply")
    else {
        panic!("no fields should require mapping");
    };
    assert!(layout.fit_all);
    let pane = layout
        .workspace
        .tree
        .tiles
        .tiles()
        .find_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            _ => None,
        })
        .expect("plot pane");
    assert!(pane.text_offsets.is_empty());
    assert!(pane.text_filters.is_empty());
}

#[test]
fn invalid_version_is_rejected() {
    let mut doc = empty_doc("bad");
    doc.delog_layout = 99;
    match load_doc(doc, &StoreSnapshot::empty()) {
        Err(LayoutError::UnsupportedVersion(99)) => {}
        Ok(_) => panic!("expected unsupported version, got successful load"),
        Err(err) => panic!("expected unsupported version, got {err}"),
    }
}

#[test]
fn frozen_v1_fixture_decodes_and_applies_cross_log() {
    let doc = crate::config::layout::doc::decode_doc(include_str!(
        "../../../../../fixtures/layouts/v1_basic.json"
    ))
    .expect("fixture should decode");
    assert_eq!(doc.delog_layout, LAYOUT_VERSION);
    assert_eq!(doc.name, "v1-basic");

    let snapshot = snapshot_with_topics(&[
        ("different_log", "ATT", &["Roll", "Pitch", "Yaw"]),
        ("different_log", "POS", &["Lat", "Lng", "Alt"]),
    ]);
    let outcome = load_doc(doc, &snapshot).expect("fixture should load");
    let LoadOutcome::Applied(layout) = outcome else {
        panic!("single-source fixture should not need mapping");
    };

    assert_eq!(layout.vehicles.len(), 1);
    assert_eq!(layout.vehicles[0].label, "Vehicle");
    assert_eq!(layout.diagnostics.len(), 0);
}

#[test]
fn same_layout_populates_after_loading_before_log_schema() {
    let doc = crate::config::layout::doc::decode_doc(include_str!(
        "../../../../../fixtures/layouts/v1_basic.json"
    ))
    .expect("fixture should decode");
    let LoadOutcome::Applied(empty_layout) =
        load_doc(doc.clone(), &StoreSnapshot::empty()).expect("empty load should apply")
    else {
        panic!("empty store should not need mapping");
    };
    assert_eq!(empty_layout.vehicles.len(), 0);
    let (traces, ghosts) = plot_trace_counts(&empty_layout.workspace);
    assert_eq!(traces, 0);
    assert_eq!(ghosts, 2);

    let snapshot = snapshot_with_topics(&[
        ("later_log", "ATT", &["Roll", "Pitch", "Yaw"]),
        ("later_log", "POS", &["Lat", "Lng", "Alt"]),
    ]);
    let LoadOutcome::Applied(populated) =
        load_doc(doc, &snapshot).expect("schema load should apply")
    else {
        panic!("single source should not need mapping");
    };
    assert_eq!(populated.vehicles.len(), 1);
    let (traces, ghosts) = plot_trace_counts(&populated.workspace);
    assert_eq!(traces, 2);
    assert_eq!(ghosts, 0);
}

#[test]
fn load_report_tracks_ambiguous_unresolved_and_non_resolution_warnings_structurally() {
    let snapshot = snapshot_with_topics(&[
        ("flight-b", "ATT", &["roll"]),
        ("flight-a", "ATT", &["roll"]),
    ]);
    let mut doc = empty_doc("report");
    doc.workspace.root = LayoutNode::Plot {
        owner: None,
        traces: vec![
            TraceLayout {
                owner: None,
                field: FieldRef {
                    topic: "ATT".into(),
                    field: "roll".into(),
                },
                color: [1.0; 4],
                width_px: 1.5,
                mode: TraceModeLayout::Line,
                visible: true,
            },
            TraceLayout {
                owner: None,
                field: FieldRef {
                    topic: "GPS".into(),
                    field: "alt".into(),
                },
                color: [1.0; 4],
                width_px: 1.5,
                mode: TraceModeLayout::Line,
                visible: true,
            },
            TraceLayout {
                owner: None,
                field: FieldRef {
                    topic: "GPS".into(),
                    field: "alt".into(),
                },
                color: [1.0; 4],
                width_px: 1.5,
                mode: TraceModeLayout::Line,
                visible: true,
            },
        ],
        show_legend: true,
        show_tooltip: true,
        annotations: vec![AnnotationLayout {
            kind: "segment".into(),
            points: vec![[1.0, 2.0]],
            y: None,
            label: "broken".into(),
            color: [1.0; 4],
            stroke_px: 1.0,
            fill_opacity: 0.0,
            font_px: 12.0,
            arrow: false,
            owner: None,
        }],
    };

    let LoadOutcome::NeedsMapping(pending) = load_doc(doc, &snapshot).unwrap() else {
        panic!("duplicate fields should require source mapping")
    };
    assert_eq!(pending.ambiguities().len(), 1);
    let applied = pending.apply_skipping(&snapshot);
    assert_eq!(applied.report.ambiguous.len(), 1);
    assert_eq!(applied.report.ambiguous[0].field.topic, "ATT");
    assert_eq!(applied.report.ambiguous[0].field.field, "roll");
    assert_eq!(applied.report.unresolved.len(), 1);
    assert_eq!(applied.report.unresolved[0].topic, "GPS");
    assert_eq!(applied.report.unresolved[0].field, "alt");
    assert_eq!(applied.report.warnings.len(), 1);
    assert!(applied.report.warnings[0].contains("broken"));
}

#[test]
fn extended_windows_survive_a_save_and_load_round_trip() {
    let snapshot = test_snapshot();
    let mut window = ExtendedWindow::new(WindowId(1));
    window.size = [1600.0, 900.0];
    window
        .workspace
        .add_trace_to_first_plot(first_field(&snapshot));
    let windows = vec![window];

    let doc = current_doc(CurrentLayout {
        name: "round-trip".to_owned(),
        workspace: &Workspace::new(),
        windows: &windows,
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });

    assert_eq!(doc.windows.len(), 1);

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("a document with no ambiguity should apply directly");
    };

    assert_eq!(applied.windows.len(), 1);
    assert_eq!(applied.windows[0].size, [1600.0, 900.0]);
    assert_eq!(applied.windows[0].workspace.fields().count(), 1);
}

#[test]
fn annotations_survive_a_save_and_load_round_trip_in_every_window() {
    use crate::plotting::annotations::{DataPos, Geometry, Kind, Style};

    let snapshot = test_snapshot();
    let main_style = Style {
        color: [0.25, 0.5, 0.75, 1.0],
        stroke_px: 3.5,
        fill_opacity: 0.4,
        font_px: 14.0,
        arrow: true,
    };
    let mut main = Workspace::new();
    let main_geom;
    {
        let pane = main.plot_panes_mut().next().unwrap();
        let id = pane.annotations.add(
            Kind::Rect,
            DataPos {
                t_us: 1_000,
                y: 2.0,
            },
            2_000_000,
            4.0,
        );
        let annotation = pane.annotations.get_mut(id).unwrap();
        annotation.label = "main burst".to_owned();
        annotation.style = main_style;
        #[cfg(feature = "scripting")]
        {
            annotation.owner = Some(crate::plotting::annotations::AnnotationOwner {
                name: "flight.py".to_owned(),
                generation: 5,
            });
        }
        main_geom = annotation.geom;
    }
    let mut window = ExtendedWindow::new(WindowId(1));
    {
        let pane = window.workspace.plot_panes_mut().next().unwrap();
        let id = pane
            .annotations
            .add(Kind::HLine, DataPos { t_us: 0, y: 3.5 }, 2_000_000, 4.0);
        pane.annotations.get_mut(id).unwrap().label = "window limit".to_owned();
    }
    let windows = vec![window];

    let doc = current_doc(CurrentLayout {
        name: "annotated-round-trip".to_owned(),
        workspace: &main,
        windows: &windows,
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("a document with no ambiguity should apply directly");
    };

    let main_pane = applied
        .workspace
        .tree
        .tiles
        .tiles()
        .find_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            _ => None,
        })
        .expect("plot pane in the main workspace");
    assert_eq!(main_pane.annotations.items().len(), 1);
    let restored = &main_pane.annotations.items()[0];
    assert_eq!(restored.label, "main burst");
    match (restored.geom, main_geom) {
        (
            Geometry::Rect { a, b },
            Geometry::Rect {
                a: orig_a,
                b: orig_b,
            },
        ) => {
            assert_eq!(a.t_us, orig_a.t_us);
            assert_eq!(a.y, orig_a.y);
            assert_eq!(b.t_us, orig_b.t_us);
            assert_eq!(b.y, orig_b.y);
        }
        other => panic!("expected a restored rect matching the original, got {other:?}"),
    }
    assert_eq!(restored.style, main_style);
    #[cfg(feature = "scripting")]
    {
        assert_eq!(
            restored.owner.as_ref().map(|owner| owner.name.as_str()),
            Some("flight.py")
        );
    }

    let window_pane = applied.windows[0]
        .workspace
        .tree
        .tiles
        .tiles()
        .find_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            _ => None,
        })
        .expect("plot pane in the extended window");
    assert_eq!(window_pane.annotations.items().len(), 1);
    assert_eq!(window_pane.annotations.items()[0].label, "window limit");
}

#[cfg(not(feature = "scripting"))]
#[test]
fn vehicle_owner_survives_a_layout_round_trip_without_scripting() {
    let snapshot = test_snapshot();
    let mut doc = empty_doc("vehicle-owner-round-trip");
    doc.vehicles
        .push(crate::config::layout::doc::VehicleLayout {
            label: "Vehicle".to_owned(),
            show: true,
            show_path: true,
            model: crate::config::layout::doc::ModelLayout::Cone,
            color: [255, 255, 255, 255],
            path_color: [255, 255, 255, 255],
            scale: 1.0,
            position: crate::config::layout::doc::PosLayout::Gps {
                lat: crate::config::layout::doc::FieldRef {
                    topic: "GLOBAL_POSITION_INT".to_owned(),
                    field: "lat".to_owned(),
                },
                lon: crate::config::layout::doc::FieldRef {
                    topic: "GLOBAL_POSITION_INT".to_owned(),
                    field: "lon".to_owned(),
                },
                alt: crate::config::layout::doc::FieldRef {
                    topic: "GLOBAL_POSITION_INT".to_owned(),
                    field: "alt".to_owned(),
                },
                lat_lon_dege7: true,
                alt_mm: true,
                alt_offset_m: 0.0,
            },
            orientation: crate::config::layout::doc::OriLayout::Static,
            owner: Some("flight.py".to_owned()),
        });

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).unwrap() else {
        panic!("the document should apply");
    };
    let saved = current_doc(CurrentLayout {
        name: "vehicle-owner-round-trip".to_owned(),
        workspace: &applied.workspace,
        windows: &applied.windows,
        snapshot: &snapshot,
        speed: applied.speed,
        follow_live: applied.follow_live,
        vehicles: &applied.vehicles,
    });
    let json = crate::config::layout::doc::doc_json(&saved).unwrap();
    let decoded = crate::config::layout::doc::decode_doc(&json).unwrap();

    assert_eq!(decoded.vehicles[0].owner.as_deref(), Some("flight.py"));
}

#[cfg(not(feature = "scripting"))]
#[test]
fn annotation_owner_survives_a_layout_round_trip_without_scripting() {
    let snapshot = test_snapshot();
    let mut doc = empty_doc("owner-round-trip");
    let LayoutNode::Plot { annotations, .. } = &mut doc.workspace.root else {
        panic!("expected a plot root");
    };
    annotations.push(crate::config::layout::doc::AnnotationLayout {
        kind: "hline".to_owned(),
        points: Vec::new(),
        y: Some(9.81),
        label: "1g".to_owned(),
        color: [1.0, 0.0, 0.0, 1.0],
        stroke_px: 1.5,
        fill_opacity: 0.0,
        font_px: 11.0,
        arrow: false,
        owner: Some("flight.py".to_owned()),
    });

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("a document with no ambiguity should apply directly");
    };
    let saved = current_doc(CurrentLayout {
        name: "owner-round-trip".to_owned(),
        workspace: &applied.workspace,
        windows: &applied.windows,
        snapshot: &snapshot,
        speed: applied.speed,
        follow_live: applied.follow_live,
        vehicles: &applied.vehicles,
    });
    let json = crate::config::layout::doc::doc_json(&saved).expect("the document should encode");
    let decoded =
        crate::config::layout::doc::decode_doc(&json).expect("the document should decode");
    let LayoutNode::Plot { annotations, .. } = decoded.workspace.root else {
        panic!("expected a plot root");
    };

    assert_eq!(annotations[0].owner.as_deref(), Some("flight.py"));
}

#[test]
fn an_annotation_whose_points_do_not_match_its_kind_is_skipped_and_reported() {
    let snapshot = test_snapshot();
    let doc = LayoutDoc {
        delog_layout: LAYOUT_VERSION,
        name: "malformed-annotation".to_owned(),
        playback: PlaybackLayout {
            speed: 1.0,
            follow_live: false,
        },
        workspace: WorkspaceLayout {
            root: LayoutNode::Plot {
                owner: None,
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
                annotations: vec![crate::config::layout::doc::AnnotationLayout {
                    kind: "rect".to_owned(),
                    points: vec![[0.0, 0.0]],
                    y: None,
                    label: "lopsided".to_owned(),
                    color: [1.0, 0.0, 0.0, 1.0],
                    stroke_px: 1.5,
                    fill_opacity: 0.0,
                    font_px: 11.0,
                    arrow: false,
                    owner: None,
                }],
            },
        },
        windows: Vec::new(),
        vehicles: Vec::new(),
    };

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("no ambiguity is possible in this document");
    };

    let pane = applied
        .workspace
        .tree
        .tiles
        .tiles()
        .find_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            _ => None,
        })
        .expect("plot pane");
    assert!(pane.annotations.is_empty());
    assert!(
        applied
            .diagnostics
            .iter()
            .any(|diag| diag.message.contains("lopsided"))
    );
}

#[test]
fn a_window_restored_from_a_layout_opens_with_its_data_browser_collapsed() {
    let snapshot = test_snapshot();
    let mut window = ExtendedWindow::new(WindowId(1));
    window
        .workspace
        .add_trace_to_first_plot(first_field(&snapshot));
    assert!(
        !window.browser.collapsed,
        "a window opened by the user starts with its browser showing"
    );

    let doc = current_doc(CurrentLayout {
        name: "collapsed".to_owned(),
        workspace: &Workspace::new(),
        windows: &[window],
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("a document with no ambiguity should apply directly");
    };

    assert!(applied.windows[0].browser.collapsed);
}

#[test]
fn a_saved_window_keeps_its_id_so_a_later_window_cannot_reuse_its_title() {
    let snapshot = test_snapshot();
    let mut window = ExtendedWindow::new(WindowId(2));
    window
        .workspace
        .add_trace_to_first_plot(first_field(&snapshot));
    let windows = vec![window];

    let doc = current_doc(CurrentLayout {
        name: "reopened".to_owned(),
        workspace: &Workspace::new(),
        windows: &windows,
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });

    assert_eq!(doc.windows[0].id, Some(2));

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("a document with no ambiguity should apply directly");
    };

    assert_eq!(applied.windows[0].id, WindowId(2));
    assert_eq!(applied.windows[0].title, "DeLOG · Window 2");

    let mut windows = Vec::new();
    let mut fresh = 1;
    crate::shell::windows::install_restored_windows(&mut windows, &mut fresh, applied.windows)
        .unwrap();
    assert_eq!(windows[0].id, WindowId(3));
    assert_eq!(fresh, 4, "the next window must not reuse a restored id");
    assert_ne!(ExtendedWindow::new(WindowId(fresh)).title, windows[0].title);
}

#[test]
fn window_ids_fall_back_to_position_and_never_collide() {
    fn layout(id: Option<u64>) -> WindowLayout {
        WindowLayout {
            owner: None,
            id,
            title: String::new(),
            size: MIN_WINDOW_SIZE,
            root: LayoutNode::Plot {
                owner: None,
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
                annotations: Vec::new(),
            },
        }
    }

    assert_eq!(
        window_ids(&[layout(None), layout(None)]),
        vec![WindowId(1), WindowId(2)],
        "a v2 document written before ids were saved keeps its positional ids"
    );
    assert_eq!(
        window_ids(&[layout(Some(4)), layout(Some(4))]),
        vec![WindowId(4), WindowId(5)],
        "a hand-edited duplicate must not shadow another window's viewport"
    );
    assert_eq!(
        window_ids(&[layout(Some(0))]),
        vec![WindowId(1)],
        "zero is the main window, never an extended one"
    );
}

#[test]
fn a_scene_node_in_an_extended_window_layout_becomes_a_plot() {
    let snapshot = test_snapshot();
    let doc = LayoutDoc {
        delog_layout: LAYOUT_VERSION,
        name: "scene".to_owned(),
        playback: PlaybackLayout {
            speed: 1.0,
            follow_live: false,
        },
        workspace: WorkspaceLayout {
            root: LayoutNode::Plot {
                owner: None,
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
                annotations: Vec::new(),
            },
        },
        windows: vec![WindowLayout {
            owner: None,
            id: None,
            title: "DeLOG · Window 1".to_owned(),
            size: [1280.0, 800.0],
            root: LayoutNode::Scene3d(SceneLayout {
                camera: CameraLayout {
                    yaw: 0.0,
                    pitch: 0.0,
                    distance: 1.0,
                },
                tracked_vehicle: None,
                trail_mode: TrailModeLayout::ToPlayhead,
            }),
        }],
        vehicles: Vec::new(),
    };

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("it should load") else {
        panic!("no ambiguity is possible in this document");
    };

    assert!(applied.windows[0].workspace.map_scopes().is_empty());
}

#[test]
fn a_field_ambiguous_only_inside_an_extended_window_is_flagged_for_mapping() {
    let snapshot = snapshot_with_topics(&[
        ("flight_a", "ATT", &["Roll"]),
        ("flight_b", "ATT", &["Roll"]),
    ]);
    let doc = LayoutDoc {
        delog_layout: LAYOUT_VERSION,
        name: "ambiguous-window".to_owned(),
        playback: PlaybackLayout {
            speed: 1.0,
            follow_live: false,
        },
        workspace: WorkspaceLayout {
            root: LayoutNode::Plot {
                owner: None,
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
                annotations: Vec::new(),
            },
        },
        windows: vec![WindowLayout {
            owner: None,
            id: None,
            title: "DeLOG · Window 1".to_owned(),
            size: [1280.0, 800.0],
            root: LayoutNode::Plot {
                owner: None,
                traces: vec![TraceLayout {
                    owner: None,
                    field: FieldRef {
                        topic: "ATT".to_owned(),
                        field: "Roll".to_owned(),
                    },
                    color: [1.0, 1.0, 1.0, 1.0],
                    width_px: 1.0,
                    mode: TraceModeLayout::Line,
                    visible: true,
                }],
                show_legend: true,
                show_tooltip: true,
                annotations: Vec::new(),
            },
        }],
        vehicles: Vec::new(),
    };

    match load_doc(doc, &snapshot).expect("document should decode") {
        LoadOutcome::NeedsMapping(pending) => assert_eq!(pending.ambiguity_count(), 1),
        LoadOutcome::Applied(_) => {
            panic!("an ambiguous field plotted only in an extended window must surface for mapping")
        }
    }
}

fn test_snapshot() -> StoreSnapshot {
    snapshot_with_topics(&[("flight.bin", "GLOBAL_POSITION_INT", &["lat", "lon", "alt"])])
}

fn first_field(snapshot: &StoreSnapshot) -> FieldId {
    snapshot.fields[0].id
}

fn snapshot_with_topics(entries: &[(&str, &str, &[&str])]) -> StoreSnapshot {
    let mut ids = IdentityRegistry::new();
    let mut sources = HashMap::new();
    for (source, topic, fields) in entries {
        let source_id = *sources
            .entry(*source)
            .or_insert_with(|| ids.add_source(*source));
        let topic = ids.add_topic(source_id, *topic).unwrap();
        for field in *fields {
            ids.add_field(topic, *field).unwrap();
        }
    }
    StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")
}

fn empty_doc(name: &str) -> LayoutDoc {
    LayoutDoc {
        delog_layout: LAYOUT_VERSION,
        name: name.into(),
        playback: PlaybackLayout {
            speed: 1.0,
            follow_live: false,
        },
        workspace: WorkspaceLayout {
            root: LayoutNode::Plot {
                owner: None,
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
                annotations: Vec::new(),
            },
        },
        windows: Vec::new(),
        vehicles: Vec::new(),
    }
}

fn plot_trace_counts(workspace: &Workspace) -> (usize, usize) {
    workspace
        .tree
        .tiles
        .tiles()
        .filter_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => {
                Some((pane.traces.len(), pane.ghosts.len()))
            }
            _ => None,
        })
        .fold((0, 0), |(traces, ghosts), (t, g)| (traces + t, ghosts + g))
}

#[test]
fn a_ui_layout_load_never_reuses_the_id_of_a_closed_window() {
    let snapshot = StoreSnapshot::empty();
    let mut open = Vec::new();
    let mut next_window_id = 1;
    crate::shell::windows::open_window(&mut open, &mut next_window_id, None);
    crate::shell::windows::open_window(&mut open, &mut next_window_id, None);
    crate::shell::windows::open_window(&mut open, &mut next_window_id, None);
    let doc = current_doc(CurrentLayout {
        name: "saved".to_owned(),
        workspace: &Workspace::new(),
        windows: &open[2..],
        snapshot: &snapshot,
        speed: 1.0,
        follow_live: false,
        vehicles: &[],
    });
    open.clear();

    let LoadOutcome::Applied(applied) = load_doc(doc, &snapshot).expect("the document should load")
    else {
        panic!("a document with no ambiguity should apply directly");
    };
    assert_eq!(applied.windows[0].id, WindowId(3));
    crate::shell::windows::install_restored_windows(
        &mut open,
        &mut next_window_id,
        applied.windows,
    )
    .unwrap();

    assert_eq!(open[0].id, WindowId(4));
    assert_eq!(next_window_id, 5);
}

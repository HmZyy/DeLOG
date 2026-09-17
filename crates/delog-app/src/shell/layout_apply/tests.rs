use super::*;
use delog_core::identity::{FieldId, IdentityRegistry};

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

    let fresh = crate::shell::windows::next_window_id(&applied.windows);
    assert_eq!(fresh, 3, "the next window must not reuse a restored id");
    assert_ne!(
        ExtendedWindow::new(WindowId(fresh)).title,
        applied.windows[0].title
    );
}

#[test]
fn window_ids_fall_back_to_position_and_never_collide() {
    fn layout(id: Option<u64>) -> WindowLayout {
        WindowLayout {
            id,
            title: String::new(),
            size: MIN_WINDOW_SIZE,
            root: LayoutNode::Plot {
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
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
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
            },
        },
        windows: vec![WindowLayout {
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
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
            },
        },
        windows: vec![WindowLayout {
            id: None,
            title: "DeLOG · Window 1".to_owned(),
            size: [1280.0, 800.0],
            root: LayoutNode::Plot {
                traces: vec![TraceLayout {
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
                traces: Vec::new(),
                show_legend: true,
                show_tooltip: true,
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

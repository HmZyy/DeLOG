use super::*;
use delog_core::identity::IdentityRegistry;

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

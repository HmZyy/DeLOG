use super::*;
use delog_core::identity::IdentityRegistry;

#[test]
fn app_settings_round_trip_through_settings_json() {
    let path = std::env::temp_dir().join(format!(
        "delog-settings-rt-{}-{}.json",
        std::process::id(),
        std::thread::current().name().unwrap_or("settings")
    ));
    let mut settings = AppSettings::default();
    settings.show_fps = true;
    settings.render_mode = crate::config::settings::RenderMode::Continuous;
    settings.theme = crate::ui::theme::ThemeChoice::Light;
    settings.scene3d.map_provider = crate::map::provider::MapProviderId::BingSatellite;
    settings.scene3d.tile_cache_limit_bytes = 8 * 1024 * 1024 * 1024;

    save_app_settings_at(&path, &settings).expect("save settings");
    let loaded = load_app_settings_at(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(loaded, settings);
}

#[test]
fn load_app_settings_defaults_when_file_missing() {
    let missing = std::env::temp_dir().join(format!(
        "delog-settings-missing-{}-{}.json",
        std::process::id(),
        std::thread::current().name().unwrap_or("settings")
    ));
    let _ = fs::remove_file(&missing);
    assert_eq!(load_app_settings_at(&missing), AppSettings::default());
}

#[test]
fn sanitize_layout_name_blocks_paths() {
    assert_eq!(sanitize_name("../bad/name"), "bad_name");
    assert_eq!(sanitize_name(""), "default");
    assert_eq!(sanitize_name("ap-attitude_1"), "ap-attitude_1");
}

#[test]
fn plot_field_ref_has_no_source_in_json() {
    let trace = TraceLayout {
        field: FieldRef {
            topic: "ATT".into(),
            field: "Roll".into(),
        },
        color: [1.0, 0.0, 0.0, 1.0],
        width_px: 1.5,
        mode: TraceModeLayout::Line,
        visible: true,
    };
    let json = serde_json::to_string(&trace).unwrap();
    assert!(json.contains("\"topic\":\"ATT\""));
    assert!(!json.contains("source"));
}

#[test]
fn export_import_doc_round_trips_through_json_file() {
    let path = std::env::temp_dir().join(format!(
        "delog-layout-test-{}-{}.json",
        std::process::id(),
        std::thread::current().name().unwrap_or("layout")
    ));
    let doc = empty_doc("portable");

    export_doc(&path, &doc).expect("export should write JSON");
    let imported = import_doc(&path).expect("import should read JSON");
    let _ = fs::remove_file(&path);

    assert_eq!(imported.delog_layout, LAYOUT_VERSION);
    assert_eq!(imported.name, "portable");
    let json = serde_json::to_string(&imported).unwrap();
    assert!(!json.contains("\"source\""));
}

#[test]
fn legacy_layout_with_settings_key_still_decodes_ignoring_it() {
    let doc = decode_doc(
        r#"{
            "delog_layout": 1,
            "name": "legacy",
            "playback": {"speed": 1.0, "follow_live": false},
            "workspace": {
                "root": {
                    "plot": {"traces": [], "show_legend": true, "show_tooltip": true}
                }
            },
            "vehicles": [],
            "settings": {"theme": "light", "show_fps": true, "render_mode": "continuous"}
        }"#,
    )
    .expect("legacy layout with settings key should decode");
    assert_eq!(doc.name, "legacy");
}

#[test]
fn missing_version_is_rejected_by_decoder() {
    match decode_doc(r#"{"name":"missing"}"#) {
        Err(LayoutError::MissingVersion) => {}
        Ok(_) => panic!("expected missing version, got successful decode"),
        Err(err) => panic!("expected missing version, got {err}"),
    }
}

#[test]
fn vehicle_layout_helpers_round_trip_static_ned_vehicle() {
    let snapshot = snapshot_with_topics(&[("log", "LOCAL_POSITION_NED", &["x", "y", "z"])]);
    let source = snapshot
        .sources
        .iter()
        .find(|s| !s.entry.removed)
        .unwrap()
        .entry
        .id;
    let mut fields = snapshot
        .fields
        .iter()
        .filter(|f| !f.removed)
        .map(|f| (f.name.as_str(), f.id))
        .collect::<std::collections::HashMap<_, _>>();
    let north = fields.remove("x").unwrap();
    let east = fields.remove("y").unwrap();
    let down = fields.remove("z").unwrap();

    let cfg = VehicleConfig {
        source,
        label: "Vehicle".to_owned(),
        show: true,
        pos: PosMapping::Ned {
            north,
            east,
            down,
            reference: None,
        },
        ori: OriMapping::Static,
        model: ModelKind::Cone,
        color: Color32::from_rgb(1, 2, 3),
        path_color: Color32::from_rgb(4, 5, 6),
        scale: 1.5,
    };

    let layout = vehicle_config_to_layout(&cfg, &snapshot).expect("vehicle should serialize");
    let resolved =
        vehicle_config_from_layout(&layout, &snapshot).expect("vehicle should resolve");

    assert_eq!(resolved, cfg);
}

#[test]
fn vehicle_config_from_layout_for_source_resolves_duplicate_topic_fields() {
    let snapshot = snapshot_with_topics(&[
        ("flight_a", "LOCAL_POSITION_NED", &["x", "y", "z"]),
        ("flight_b", "LOCAL_POSITION_NED", &["x", "y", "z"]),
    ]);
    let second_source = snapshot
        .sources
        .iter()
        .find(|source| source.entry.label == "flight_b")
        .map(|source| source.entry.id)
        .expect("second source should exist");
    let layout = VehicleLayout {
        label: "Rover".to_owned(),
        show: true,
        model: ModelLayout::Cone,
        color: [255, 255, 255, 255],
        path_color: [0, 0, 0, 255],
        scale: 2.0,
        position: PosLayout::Ned {
            north: FieldRef {
                topic: "LOCAL_POSITION_NED".to_owned(),
                field: "x".to_owned(),
            },
            east: FieldRef {
                topic: "LOCAL_POSITION_NED".to_owned(),
                field: "y".to_owned(),
            },
            down: FieldRef {
                topic: "LOCAL_POSITION_NED".to_owned(),
                field: "z".to_owned(),
            },
            reference: None,
        },
        orientation: OriLayout::Static,
    };

    let cfg = vehicle_config_from_layout_for_source(&layout, &snapshot, second_source)
        .expect("vehicle should resolve for selected source");

    assert_eq!(cfg.source, second_source);
    let PosMapping::Ned {
        north, east, down, ..
    } = cfg.pos
    else {
        panic!("expected NED mapping");
    };
    for field in [north, east, down] {
        let topic = snapshot
            .fields
            .get(field.index())
            .and_then(|field| snapshot.topic(field.topic))
            .expect("field topic should exist");
        assert_eq!(topic.entry.source, second_source);
    }
}

#[test]
fn one_loaded_source_resolves_topic_field_without_source() {
    let (snapshot, field) = snapshot_with_sources(&[("flight_a", "ATT", "Roll")]);
    let mut resolver = Resolver {
        snapshot: &snapshot,
        choices: &HashMap::new(),
        diagnostics: Vec::new(),
        ambiguities: BTreeMap::new(),
        collect_ambiguities: true,
    };

    let got = resolver.resolve(&FieldRef {
        topic: "ATT".into(),
        field: "Roll".into(),
    });

    assert_eq!(got, Some(field[0]));
    assert!(resolver.ambiguities.is_empty());
}

#[test]
fn duplicate_topic_field_across_sources_is_ambiguous() {
    let (snapshot, _) =
        snapshot_with_sources(&[("flight_a", "ATT", "Roll"), ("flight_b", "ATT", "Roll")]);
    let mut resolver = Resolver {
        snapshot: &snapshot,
        choices: &HashMap::new(),
        diagnostics: Vec::new(),
        ambiguities: BTreeMap::new(),
        collect_ambiguities: true,
    };

    let got = resolver.resolve(&FieldRef {
        topic: "ATT".into(),
        field: "Roll".into(),
    });

    assert_eq!(got, None);
    let ambiguity = resolver.ambiguities.values().next().unwrap();
    assert_eq!(ambiguity.candidates.len(), 2);
    assert_eq!(ambiguity.candidates[0].label, "flight_a");
    assert_eq!(ambiguity.candidates[1].label, "flight_b");
}

fn snapshot_with_sources(entries: &[(&str, &str, &str)]) -> (StoreSnapshot, Vec<FieldId>) {
    let mut ids = IdentityRegistry::new();
    let mut fields = Vec::new();
    for (source, topic, field) in entries {
        let source = ids.add_source(*source);
        let topic = ids.add_topic(source, *topic).unwrap();
        fields.push(ids.add_field(topic, *field).unwrap());
    }
    (
        StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"),
        fields,
    )
}

fn snapshot_with_topics(entries: &[(&str, &str, &[&str])]) -> StoreSnapshot {
    let mut ids = IdentityRegistry::new();
    let mut sources = std::collections::HashMap::new();
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

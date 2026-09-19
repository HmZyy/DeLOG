#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::{
    APP, COMMAND_PALETTE, CONTEXT_HEADER, DATAFLOW_WINDOW, PALETTE, PARSERS, SCRIPTS,
    SEQUENCES_WINDOW, SYNC_WINDOW, VEHICLE_DIALOG, WORKSPACE,
};

const SCANNED: &[(&str, &str)] = &[
    ("shell/app", APP),
    ("shell/app/command_palette.rs", COMMAND_PALETTE),
    ("shell/app/context_header.rs", CONTEXT_HEADER),
    ("dataflow/window.rs", DATAFLOW_WINDOW),
    ("ingest/parsers.rs", PARSERS),
    ("scripting/scripts.rs", SCRIPTS),
    ("sequences/window.rs", SEQUENCES_WINDOW),
    ("session/vehicle_dialog", VEHICLE_DIALOG),
    ("shell/workspace/mod.rs", WORKSPACE),
    ("sync/sync_window/mod.rs", SYNC_WINDOW),
    ("ui/palette.rs", PALETTE),
];

fn assert_nouns(expected: &[(&str, &str, &[&str])], builder: &str) {
    for (name, source, plurals) in expected {
        for plural in *plurals {
            let call = format!("{builder}(\"{plural}\")");
            assert!(
                source.contains(&call),
                "{name} should build its empty state with {call}"
            );
        }
    }
}

#[test]
fn no_source_hand_writes_an_empty_state_string() {
    for (name, source) in SCANNED {
        for needle in [
            "\"No saved",
            "\"No matching",
            "\"No vehicles",
            "\"No topics",
            "\"No fields",
            "\"No sources",
        ] {
            assert!(
                !source.contains(needle),
                "{name} hand-writes {needle}...; build it with crate::ui::empty instead"
            );
        }
    }
}

#[test]
fn every_dropdown_source_builds_its_empty_state_with_the_helper() {
    for (name, source) in SCANNED {
        if *name == "ui/palette.rs" {
            continue;
        }
        assert!(
            source.contains("ui::empty::"),
            "{name} should build its empty states with crate::ui::empty"
        );
    }
}

#[test]
fn every_saved_library_dropdown_names_its_own_type() {
    assert_nouns(
        &[
            ("shell/app", APP, &["layouts", "scripts"]),
            (
                "shell/app/context_header.rs",
                CONTEXT_HEADER,
                &["scripts", "parsers", "layouts", "sequences"],
            ),
            ("dataflow/window.rs", DATAFLOW_WINDOW, &["dataflows"]),
            ("ingest/parsers.rs", PARSERS, &["parsers"]),
            ("scripting/scripts.rs", SCRIPTS, &["scripts"]),
            ("sequences/window.rs", SEQUENCES_WINDOW, &["sequences"]),
            ("session/vehicle_dialog", VEHICLE_DIALOG, &["profiles"]),
        ],
        "no_saved",
    );
}

#[test]
fn the_run_palette_names_every_kind_it_can_run() {
    for plural in ["scripts", "parsers", "dataflows", "sequences"] {
        assert!(
            APP.contains(&format!("=> \"{plural}\"")),
            "RunKind::plural should name {plural} so the run palette can say it has none saved"
        );
    }
}

#[test]
fn every_derived_dropdown_falls_back_to_a_plain_count() {
    assert_nouns(
        &[
            (
                "session/vehicle_dialog",
                VEHICLE_DIALOG,
                &["vehicles", "sources", "fields"],
            ),
            ("shell/workspace/mod.rs", WORKSPACE, &["vehicles"]),
            (
                "sync/sync_window/mod.rs",
                SYNC_WINDOW,
                &["topics", "fields"],
            ),
            (
                "shell/app/command_palette.rs",
                COMMAND_PALETTE,
                &["commands"],
            ),
        ],
        "no_items",
    );
}

#[test]
fn a_searchable_picker_is_told_the_noun_it_lists() {
    assert!(
        VEHICLE_DIALOG.contains("topics, \"topics\")"),
        "searchable_combo call sites should name the type they list"
    );
}

#[test]
fn a_filtered_list_is_distinguished_from_an_empty_one() {
    assert!(
        PALETTE.contains("if items.is_empty()"),
        "the palette should pick its text from the library size, not the ranked size"
    );
    assert_nouns(
        &[
            ("shell/app", APP, &["layouts", "scripts", "actions"]),
            (
                "shell/app/command_palette.rs",
                COMMAND_PALETTE,
                &["commands"],
            ),
            ("sync/sync_window/mod.rs", SYNC_WINDOW, &["fields"]),
        ],
        "no_matching",
    );
}

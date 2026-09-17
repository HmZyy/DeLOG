use super::*;
use crate::shell::workspace::Workspace;
use delog_core::identity::FieldId;

fn workspace_with(fields: &[u32]) -> Workspace {
    let mut workspace = Workspace::new();
    for &field in fields {
        let _ = workspace.add_trace_to_first_plot(FieldId(field));
    }
    workspace
}

fn window_with(id: u64, fields: &[u32]) -> ExtendedWindow {
    let mut window = ExtendedWindow::new(WindowId(id));
    window.workspace = workspace_with(fields);
    window
}

#[test]
fn the_main_window_is_window_zero() {
    assert_eq!(WindowId::MAIN, WindowId(0));
}

#[test]
fn each_window_gets_its_own_viewport_id() {
    assert_ne!(WindowId(1).viewport_id(), WindowId(2).viewport_id());
    assert_ne!(WindowId(1).viewport_id(), egui::ViewportId::ROOT);
}

#[test]
fn window_titles_carry_the_id_so_compositor_rules_can_match_them() {
    assert_eq!(WindowId(2).title(), "DeLOG · Window 2");
    assert_eq!(WindowId(7).title(), "DeLOG · Window 7");
}

#[test]
fn each_window_profiles_its_tree_under_its_own_name() {
    assert_eq!(tree_scope(WindowId::MAIN), "workspace_tree");
    assert_ne!(tree_scope(WindowId(1)), tree_scope(WindowId::MAIN));
    assert_ne!(tree_scope(WindowId(1)), tree_scope(WindowId(2)));
    assert_eq!(
        tree_scope(WindowId(64)),
        tree_scope(WindowId(65)),
        "past the interned names windows share one bucket rather than leaking a name per frame"
    );
}

#[test]
fn the_next_id_clears_every_open_window() {
    assert_eq!(next_window_id(&[]), 1);
    assert_eq!(next_window_id(&[window_with(2, &[])]), 3);
    assert_eq!(
        next_window_id(&[window_with(5, &[]), window_with(1, &[])]),
        6
    );
}

#[test]
fn union_fields_spans_every_window_without_duplicates() {
    let main = workspace_with(&[1, 2]);
    let windows = vec![window_with(1, &[2, 3]), window_with(2, &[4])];

    let mut union = union_fields(&main, &windows);
    union.sort_by_key(|f| f.0);

    assert_eq!(union, vec![FieldId(1), FieldId(2), FieldId(3), FieldId(4)]);
}

#[test]
fn union_fields_dedupes_a_trace_plotted_in_two_panes() {
    let mut workspace = Workspace::new();
    let first = workspace.tree.root().unwrap();
    workspace.add_trace_to_first_plot(FieldId(7));
    workspace.add_trace_to_first_plot(FieldId(3));
    workspace.split_plot(first, crate::shell::workspace::SplitDirection::Horizontal);

    let second = workspace
        .tree
        .tiles
        .iter()
        .filter(|(id, tile)| {
            **id != first
                && matches!(
                    tile,
                    egui_tiles::Tile::Pane(crate::shell::workspace::Pane::Plot(_))
                )
        })
        .map(|(id, _)| *id)
        .next()
        .expect("the split should have produced a second plot");
    let Some(egui_tiles::Tile::Pane(crate::shell::workspace::Pane::Plot(pane))) =
        workspace.tree.tiles.get_mut(second)
    else {
        panic!("expected a plot pane");
    };
    pane.add_trace(FieldId(3));
    pane.add_trace(FieldId(9));

    let unique = union_fields(&workspace, &[]);
    let mut sorted = unique.clone();
    sorted.sort_by_key(|field| field.0);
    assert_eq!(
        sorted,
        vec![FieldId(3), FieldId(7), FieldId(9)],
        "every plotted trace should be present"
    );
    assert_eq!(
        unique.len(),
        3,
        "a trace plotted in two panes should appear once, got {unique:?}"
    );
}

#[test]
fn closing_a_window_releases_only_fields_no_one_else_plots() {
    let main = workspace_with(&[1, 2]);
    let closing = window_with(1, &[2, 3]);
    let others = vec![window_with(2, &[3, 4])];

    let released = fields_only_in(&closing, &main, &others);

    assert_eq!(released, vec![]);

    let others = vec![window_with(2, &[4])];
    let released = fields_only_in(&closing, &main, &others);

    assert_eq!(released, vec![FieldId(3)]);
}

fn seed_annotation(
    workspace: &mut Workspace,
    tile: egui_tiles::TileId,
    kind: crate::plotting::annotations::Kind,
) -> u64 {
    use crate::plotting::annotations::DataPos;
    let Some(egui_tiles::Tile::Pane(crate::shell::workspace::Pane::Plot(pane))) =
        workspace.tree.tiles.get_mut(tile)
    else {
        panic!("expected a plot pane");
    };
    pane.annotations
        .add(kind, DataPos { t_us: 0, y: 0.0 }, 1_000_000, 10.0)
}

fn annotation_count(workspace: &Workspace, tile: egui_tiles::TileId) -> usize {
    let Some(egui_tiles::Tile::Pane(crate::shell::workspace::Pane::Plot(pane))) =
        workspace.tree.tiles.get(tile)
    else {
        panic!("expected a plot pane");
    };
    pane.annotations.items().len()
}

fn editing_id(workspace: &Workspace, tile: egui_tiles::TileId) -> Option<u64> {
    let Some(egui_tiles::Tile::Pane(crate::shell::workspace::Pane::Plot(pane))) =
        workspace.tree.tiles.get(tile)
    else {
        panic!("expected a plot pane");
    };
    pane.annotations.editing
}

#[test]
fn annotation_rows_span_every_window_with_disambiguated_labels() {
    use crate::plotting::annotations::Kind;

    let mut main = Workspace::new();
    let main_tile = main.tree.root().unwrap();
    seed_annotation(&mut main, main_tile, Kind::Rect);

    let mut extended = window_with(3, &[]);
    let extended_tile = extended.workspace.tree.root().unwrap();
    seed_annotation(&mut extended.workspace, extended_tile, Kind::HLine);
    let windows = vec![extended];

    let rows = annotation_rows(&main, &windows);
    assert_eq!(rows.len(), 2);
    let main_row = rows.iter().find(|r| r.window == 0).expect("a main row");
    let window_row = rows.iter().find(|r| r.window == 3).expect("a window row");
    assert_eq!(main_row.plot_label, "Plot 1");
    assert_eq!(window_row.plot_label, "Window 3 · Plot 1");
}

#[test]
fn removing_an_annotation_in_window_1_does_not_touch_the_main_windows_colliding_pane_id() {
    use crate::plotting::annotations::Kind;
    use crate::plotting::annotations::toolbar::ToolbarAction;

    let mut main = Workspace::new();
    let main_tile = main.tree.root().unwrap();
    seed_annotation(&mut main, main_tile, Kind::Rect);

    let mut extended = window_with(1, &[]);
    let extended_tile = extended.workspace.tree.root().unwrap();
    assert_eq!(
        main_tile, extended_tile,
        "this test only proves anything if the two panes share a tile id"
    );
    let extended_id = seed_annotation(&mut extended.workspace, extended_tile, Kind::Rect);

    let mut windows = vec![extended];
    let rows = annotation_rows(&main, &windows);
    assert_eq!(rows.len(), 2, "a row from each window's colliding pane");
    let window_1_row = rows
        .iter()
        .find(|r| r.window == 1)
        .expect("a row tagged with window 1");
    assert_eq!(window_1_row.pane, extended_tile.0);
    assert_eq!(window_1_row.id, extended_id);

    apply_annotation_action(
        &mut main,
        &mut windows,
        ToolbarAction::Remove {
            window: window_1_row.window,
            pane: window_1_row.pane,
            id: window_1_row.id,
        },
    );

    assert_eq!(
        annotation_count(&main, main_tile),
        1,
        "removing window 1's annotation must not touch the main window's identically-numbered pane"
    );
    assert_eq!(annotation_count(&windows[0].workspace, extended_tile), 0);
}

#[test]
fn remove_all_clears_annotations_in_every_window() {
    use crate::plotting::annotations::Kind;
    use crate::plotting::annotations::toolbar::ToolbarAction;

    let mut main = Workspace::new();
    let main_tile = main.tree.root().unwrap();
    seed_annotation(&mut main, main_tile, Kind::Rect);

    let mut extended = window_with(1, &[]);
    let extended_tile = extended.workspace.tree.root().unwrap();
    seed_annotation(&mut extended.workspace, extended_tile, Kind::Rect);
    let mut windows = vec![extended];

    apply_annotation_action(&mut main, &mut windows, ToolbarAction::RemoveAll);

    assert_eq!(annotation_count(&main, main_tile), 0);
    assert_eq!(annotation_count(&windows[0].workspace, extended_tile), 0);
}

#[test]
fn an_action_naming_a_closed_window_is_ignored() {
    use crate::plotting::annotations::Kind;
    use crate::plotting::annotations::toolbar::ToolbarAction;

    let mut main = Workspace::new();
    let main_tile = main.tree.root().unwrap();
    let id = seed_annotation(&mut main, main_tile, Kind::Rect);
    let mut windows: Vec<ExtendedWindow> = Vec::new();

    apply_annotation_action(
        &mut main,
        &mut windows,
        ToolbarAction::Remove {
            window: 7,
            pane: main_tile.0,
            id,
        },
    );

    assert_eq!(
        annotation_count(&main, main_tile),
        1,
        "an action naming a window that no longer exists must be ignored"
    );
}

#[test]
fn opening_an_editor_in_one_window_closes_an_editor_open_in_another() {
    use crate::plotting::annotations::Kind;
    use crate::plotting::annotations::toolbar::ToolbarAction;

    let mut main = Workspace::new();
    let main_tile = main.tree.root().unwrap();
    let main_id = seed_annotation(&mut main, main_tile, Kind::Rect);
    let mut no_windows: Vec<ExtendedWindow> = Vec::new();
    apply_annotation_action(
        &mut main,
        &mut no_windows,
        ToolbarAction::Edit {
            window: 0,
            pane: main_tile.0,
            id: main_id,
        },
    );
    assert_eq!(editing_id(&main, main_tile), Some(main_id));

    let mut extended = window_with(1, &[]);
    let extended_tile = extended.workspace.tree.root().unwrap();
    let extended_id = seed_annotation(&mut extended.workspace, extended_tile, Kind::Rect);
    let mut windows = vec![extended];

    apply_annotation_action(
        &mut main,
        &mut windows,
        ToolbarAction::Edit {
            window: 1,
            pane: extended_tile.0,
            id: extended_id,
        },
    );

    assert_eq!(
        editing_id(&main, main_tile),
        None,
        "opening an editor in window 1 must close the main window's editor"
    );
    assert_eq!(
        editing_id(&windows[0].workspace, extended_tile),
        Some(extended_id)
    );
}

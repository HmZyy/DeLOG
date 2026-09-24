#[cfg(feature = "scripting")]
use delog_api::control::{AnnotationInfo, PlotInfo};
use delog_core::identity::FieldId;

use crate::plotting::browser::{self, BrowserFilterCache};
use crate::shell::workspace::Workspace;

#[cfg(test)]
mod tests;

pub const DEFAULT_WINDOW_SIZE: [f32; 2] = [1280.0, 800.0];
pub const MIN_WINDOW_SIZE: [f32; 2] = [640.0, 400.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub u64);

impl WindowId {
    pub const MAIN: Self = Self(0);

    pub fn is_main(self) -> bool {
        self == Self::MAIN
    }

    pub fn viewport_id(self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("delog_window", self.0))
    }

    pub fn title(self) -> String {
        format!("DeLOG · Window {}", self.0)
    }

    pub fn id_salt(self) -> egui::Id {
        egui::Id::new(("delog_window", self.0))
    }
}

#[derive(Clone, Default)]
pub struct WindowBrowser {
    pub query: String,
    pub filter: BrowserFilterCache,
    pub selection: browser::Selection,
    pub collapsed: bool,
}

#[derive(Clone)]
pub struct ExtendedWindow {
    pub id: WindowId,
    pub title: String,
    pub workspace: Workspace,
    pub browser: WindowBrowser,
    pub size: [f32; 2],
}

impl ExtendedWindow {
    pub fn new(id: WindowId) -> Self {
        Self {
            id,
            title: id.title(),
            workspace: Workspace::new_for(id),
            browser: WindowBrowser::default(),
            size: DEFAULT_WINDOW_SIZE,
        }
    }

    pub fn restored(id: WindowId) -> Self {
        let mut window = Self::new(id);
        window.browser.collapsed = true;
        window
    }

    pub fn placeholder(id: WindowId) -> Self {
        Self {
            id,
            title: String::new(),
            workspace: Workspace::placeholder(),
            browser: WindowBrowser::default(),
            size: DEFAULT_WINDOW_SIZE,
        }
    }
}

const EXTENDED_TREE_SCOPES: [&str; 8] = [
    "workspace_tree_window_1",
    "workspace_tree_window_2",
    "workspace_tree_window_3",
    "workspace_tree_window_4",
    "workspace_tree_window_5",
    "workspace_tree_window_6",
    "workspace_tree_window_7",
    "workspace_tree_window_8",
];

pub fn tree_scope(window: WindowId) -> &'static str {
    match window.0 {
        0 => "workspace_tree",
        id => EXTENDED_TREE_SCOPES
            .get(id as usize - 1)
            .copied()
            .unwrap_or("workspace_tree_window_other"),
    }
}

pub fn next_window_id(windows: &[ExtendedWindow]) -> u64 {
    windows.iter().map(|window| window.id.0).max().unwrap_or(0) + 1
}

pub fn open_window(
    windows: &mut Vec<ExtendedWindow>,
    next_window_id: &mut u64,
    title: Option<String>,
) -> WindowId {
    let id = WindowId(*next_window_id);
    *next_window_id += 1;
    let mut window = ExtendedWindow::new(id);
    if let Some(title) = title {
        window.title = title;
    }
    windows.push(window);
    id
}

pub fn union_fields(main: &Workspace, windows: &[ExtendedWindow]) -> Vec<FieldId> {
    let mut seen = std::collections::HashSet::new();
    let mut union = Vec::new();
    for field in main
        .fields()
        .chain(windows.iter().flat_map(|w| w.workspace.fields()))
    {
        if seen.insert(field) {
            union.push(field);
        }
    }
    union
}

pub fn fields_only_in(
    window: &ExtendedWindow,
    main: &Workspace,
    others: &[ExtendedWindow],
) -> Vec<FieldId> {
    let kept: std::collections::HashSet<FieldId> = main
        .fields()
        .chain(
            others
                .iter()
                .filter(|other| other.id != window.id)
                .flat_map(|other| other.workspace.fields()),
        )
        .collect();
    let mut released = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for field in window.workspace.fields() {
        if !kept.contains(&field) && seen.insert(field) {
            released.push(field);
        }
    }
    released
}

#[cfg(feature = "scripting")]
pub fn plot_infos(main: &Workspace, windows: &[ExtendedWindow]) -> Vec<PlotInfo> {
    let mut infos = main.plot_infos(0);
    for window in windows {
        infos.extend(window.workspace.plot_infos(window.id.0));
    }
    infos
}

#[cfg(feature = "scripting")]
pub fn annotation_infos(main: &Workspace, windows: &[ExtendedWindow]) -> Vec<AnnotationInfo> {
    let mut infos = main.annotation_infos(0);
    for window in windows {
        infos.extend(window.workspace.annotation_infos(window.id.0));
    }
    infos
}

pub fn annotation_rows(
    main: &Workspace,
    windows: &[ExtendedWindow],
) -> Vec<crate::plotting::annotations::toolbar::AnnotationRow> {
    let mut rows = main.annotation_rows();
    for window in windows {
        let mut extra = window.workspace.annotation_rows();
        for row in &mut extra {
            row.window = window.id.0;
            row.plot_label = format!("Window {} · {}", window.id.0, row.plot_label);
        }
        rows.extend(extra);
    }
    rows
}

pub fn apply_annotation_action(
    main: &mut Workspace,
    windows: &mut [ExtendedWindow],
    action: crate::plotting::annotations::toolbar::ToolbarAction,
) {
    use crate::plotting::annotations::toolbar::ToolbarAction;
    match action {
        ToolbarAction::RemoveAll => {
            main.apply_annotation_action(action);
            for window in windows.iter_mut() {
                window.workspace.apply_annotation_action(action);
            }
        }
        ToolbarAction::Remove { window, .. } => {
            if window == 0 {
                main.apply_annotation_action(action);
            } else if let Some(target) = windows.iter_mut().find(|w| w.id.0 == window) {
                target.workspace.apply_annotation_action(action);
            }
        }
        ToolbarAction::Edit { window, .. } => {
            let opened = if window == 0 {
                main.apply_annotation_action(action);
                true
            } else if let Some(target) = windows.iter_mut().find(|w| w.id.0 == window) {
                target.workspace.apply_annotation_action(action);
                true
            } else {
                false
            };
            if !opened {
                return;
            }
            if window != 0 {
                main.close_all_annotation_editors();
            }
            for other in windows.iter_mut().filter(|w| w.id.0 != window) {
                other.workspace.close_all_annotation_editors();
            }
        }
    }
}

fn alt_in(ctx: &egui::Context, viewport: egui::ViewportId) -> bool {
    ctx.input_for(viewport, |input| input.focused && input.modifiers.alt)
}

pub fn alt_held(ctx: &egui::Context, windows: &[ExtendedWindow]) -> bool {
    alt_in(ctx, egui::ViewportId::ROOT)
        || windows
            .iter()
            .any(|window| alt_in(ctx, window.id.viewport_id()))
}

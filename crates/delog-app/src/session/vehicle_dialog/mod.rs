use delog_core::snapshot::StoreSnapshot;

use crate::scene3d::vehicle::VehicleConfig;
use crate::ui::logging::{LogLevel, PendingLog, log};

mod draft;
mod profile_draft;
mod profiles;
mod profiles_tab;
mod vehicles_tab;
mod widgets;

#[cfg(test)]
mod tests;

use draft::Draft;
use profile_draft::ProfileDraft;
use profiles::{profile_library, refresh_profiles};
use profiles_tab::show_profiles_tab;
use vehicles_tab::show_vehicle_config_tab;

const DIALOG_WIDTH: f32 = 760.0;
const DIALOG_HEIGHT: f32 = 520.0;
const DIALOG_MIN_WIDTH: f32 = 520.0;
const DIALOG_MIN_HEIGHT: f32 = 320.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum VehicleDialogTab {
    Vehicles,
    Profiles,
}

impl VehicleDialogTab {
    const ALL: [Self; 2] = [Self::Vehicles, Self::Profiles];

    const fn label(self) -> &'static str {
        match self {
            Self::Vehicles => "Vehicle Config",
            Self::Profiles => "Profiles",
        }
    }
}

pub struct VehicleDialog {
    pub open: bool,
    drafts: Vec<Draft>,
    selected_vehicle: usize,
    was_open: bool,
    dock_state: egui_dock::DockState<VehicleDialogTab>,
    profiles: Vec<String>,
    profile_editor_selected: Option<String>,
    profile_editor_name: String,
    profile_editor_draft: ProfileDraft,
    pending_profile_delete: Option<String>,
    pending_logs: Vec<PendingLog>,
}

impl Default for VehicleDialog {
    fn default() -> Self {
        Self {
            open: false,
            drafts: Vec::new(),
            selected_vehicle: 0,
            was_open: false,
            dock_state: egui_dock::DockState::new(VehicleDialogTab::ALL.to_vec()),
            profiles: Vec::new(),
            profile_editor_selected: None,
            profile_editor_name: String::new(),
            profile_editor_draft: ProfileDraft::default(),
            pending_profile_delete: None,
            pending_logs: Vec::new(),
        }
    }
}

impl VehicleDialog {
    pub fn take_logs(&mut self) -> Vec<PendingLog> {
        std::mem::take(&mut self.pending_logs)
    }

    fn clamp_selection(&mut self) {
        self.selected_vehicle = self
            .selected_vehicle
            .min(self.drafts.len().saturating_sub(1));
    }
}

#[track_caller]
fn log_profile(state: &mut VehicleDialog, level: LogLevel, message: impl Into<String>) {
    state.pending_logs.push(log(level, message));
}

enum ProfileAction {
    Apply { draft: usize, name: String },
    SaveAs { draft: usize },
    Delete(String),
}

/// Returns `true` when the vehicle set changed.
pub fn show(
    ctx: &egui::Context,
    state: &mut VehicleDialog,
    vehicles: &mut Vec<VehicleConfig>,
    snapshot: &StoreSnapshot,
) -> bool {
    // Resync drafts on the open edge so external changes (e.g. a loaded layout)
    // are reflected when the dialog opens.
    if state.open && !state.was_open {
        state.drafts = vehicles
            .iter()
            .map(|v| Draft::from_config(v, snapshot))
            .collect();
        state.clamp_selection();
        refresh_profiles(state);
    }
    state.was_open = state.open;
    if !state.open {
        state.pending_profile_delete = None;
        return false;
    }

    let mut open = state.open;
    egui::Window::new("Vehicles")
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .resizable(true)
        .default_size([DIALOG_WIDTH, DIALOG_HEIGHT])
        .min_width(DIALOG_MIN_WIDTH)
        .min_height(DIALOG_MIN_HEIGHT)
        .show(ctx, |ui| {
            let mut dock_state = std::mem::replace(
                &mut state.dock_state,
                egui_dock::DockState::new(VehicleDialogTab::ALL.to_vec()),
            );
            let mut viewer = VehicleDialogTabViewer { state, snapshot };
            egui_dock::DockArea::new(&mut dock_state)
                .id(egui::Id::new("vehicle_dialog_dock_area"))
                .style(crate::ui::docks::dock_style(ui.style().as_ref()))
                .allowed_splits(egui_dock::AllowedSplits::None)
                .draggable_tabs(false)
                .tab_context_menus(false)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut viewer);
            state.dock_state = dock_state;
        });
    show_profile_delete_confirmation(ctx, state);
    state.open = open;
    if !state.open {
        state.pending_profile_delete = None;
    }

    // Commit on any diff so cosmetic edits show immediately, but only report a
    // change (which drives the off-thread trajectory rebuild) when source or
    // position mapping moves.
    let rebuilt: Vec<VehicleConfig> = state.drafts.iter().filter_map(Draft::build).collect();
    if rebuilt == *vehicles {
        return false;
    }
    let traj_changed = rebuilt
        .iter()
        .map(|v| (v.source, &v.pos))
        .ne(vehicles.iter().map(|v| (v.source, &v.pos)));
    *vehicles = rebuilt;
    traj_changed
}

struct VehicleDialogTabViewer<'a> {
    state: &'a mut VehicleDialog,
    snapshot: &'a StoreSnapshot,
}

impl egui_dock::TabViewer for VehicleDialogTabViewer<'_> {
    type Tab = VehicleDialogTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            VehicleDialogTab::Vehicles => show_vehicle_config_tab(ui, self.state, self.snapshot),
            VehicleDialogTab::Profiles => show_profiles_tab(ui, self.state, self.snapshot),
        }
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }
}

fn show_profile_delete_confirmation(ctx: &egui::Context, state: &mut VehicleDialog) {
    let Some(name) = state.pending_profile_delete.clone() else {
        return;
    };

    let mut close_confirmation = false;

    egui::Window::new("Delete profile?")
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .show(ctx, |ui| {
            ui.label(format!("Delete vehicle profile '{name}'?"));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    match profile_library() {
                        Some(library) => match library.delete(&name) {
                            Ok(()) => {
                                for draft in &mut state.drafts {
                                    if draft.selected_profile.as_deref() == Some(name.as_str()) {
                                        draft.selected_profile = None;
                                    }
                                }
                                if state.profile_editor_selected.as_deref() == Some(name.as_str()) {
                                    state.profile_editor_selected = None;
                                    state.profile_editor_name.clear();
                                    state.profile_editor_draft = ProfileDraft::default();
                                }
                                refresh_profiles(state);
                                log_profile(
                                    state,
                                    LogLevel::Info,
                                    format!("deleted vehicle profile '{name}'"),
                                );
                            }
                            Err(err) => {
                                log_profile(
                                    state,
                                    LogLevel::Error,
                                    format!("failed to delete vehicle profile '{name}': {err}"),
                                );
                            }
                        },
                        None => {
                            log_profile(
                                state,
                                LogLevel::Warning,
                                "vehicle profile config directory is unavailable",
                            );
                        }
                    }
                    close_confirmation = true;
                }
                if ui.button("Cancel").clicked() {
                    close_confirmation = true;
                }
            });
        });

    if close_confirmation {
        state.pending_profile_delete = None;
    }
}

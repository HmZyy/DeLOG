use delog_core::snapshot::StoreSnapshot;

use crate::session::vehicle_profiles::VehicleProfileLibrary;
use crate::ui::logging::LogLevel;

use super::profile_draft::ProfileDraft;
use super::{ProfileAction, VehicleDialog, log_profile};

pub(super) fn profile_library() -> Option<VehicleProfileLibrary> {
    VehicleProfileLibrary::from_config_dir()
}

pub(super) fn refresh_profiles(state: &mut VehicleDialog) {
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        state.profiles.clear();
        for draft in &mut state.drafts {
            draft.selected_profile = None;
        }
        return;
    };

    match library.list() {
        Ok(profiles) => {
            state.profiles = profiles;
            if state
                .profile_editor_selected
                .as_ref()
                .is_some_and(|selected| !state.profiles.contains(selected))
            {
                state.profile_editor_selected = None;
                state.profile_editor_name.clear();
                state.profile_editor_draft = ProfileDraft::default();
            }
            for draft in &mut state.drafts {
                if draft
                    .selected_profile
                    .as_ref()
                    .is_some_and(|selected| !state.profiles.contains(selected))
                {
                    draft.selected_profile = None;
                }
            }
        }
        Err(err) => {
            log_profile(
                state,
                LogLevel::Error,
                format!("failed to list vehicle profiles: {err}"),
            );
            state.profiles.clear();
            state.profile_editor_selected = None;
            for draft in &mut state.drafts {
                draft.selected_profile = None;
            }
        }
    }
}

pub(super) fn handle_profile_action(
    action: ProfileAction,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
) {
    match action {
        ProfileAction::Apply { draft, name } => {
            apply_profile_to_draft(state, draft, &name, snapshot)
        }
        ProfileAction::Delete(name) => {
            state.pending_profile_delete = Some(name);
        }
    }
}

pub(super) fn load_profile_editor(state: &mut VehicleDialog) {
    let Some(name) = state.profile_editor_selected.clone() else {
        state.profile_editor_name.clear();
        state.profile_editor_draft = ProfileDraft::default();
        return;
    };
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        return;
    };
    match library.load(&name) {
        Ok(doc) => {
            state.profile_editor_name = doc.name.clone();
            state.profile_editor_draft = ProfileDraft::from_doc(&doc);
        }
        Err(err) => {
            log_profile(
                state,
                LogLevel::Error,
                format!("failed to load vehicle profile '{name}': {err}"),
            );
        }
    }
}

pub(super) fn save_profile_from_editor(state: &mut VehicleDialog) {
    let name = state.profile_editor_name.trim().to_owned();
    if name.is_empty() {
        log_profile(state, LogLevel::Warning, "enter a vehicle profile name");
        return;
    }
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        return;
    };
    let doc = match state.profile_editor_draft.to_doc(&name) {
        Ok(doc) => doc,
        Err(err) => {
            log_profile(
                state,
                LogLevel::Warning,
                format!("invalid vehicle profile '{name}': {err}"),
            );
            return;
        }
    };
    if let Err(err) = library.save(&name, &doc) {
        log_profile(
            state,
            LogLevel::Error,
            format!("failed to save vehicle profile '{name}': {err}"),
        );
        return;
    }

    refresh_profiles(state);
    state.profile_editor_selected = Some(name.clone());
    state.profile_editor_name = name.clone();
    log_profile(
        state,
        LogLevel::Info,
        format!("saved vehicle profile '{name}'"),
    );
}

pub(super) fn apply_profile_to_draft(
    state: &mut VehicleDialog,
    draft_index: usize,
    name: &str,
    snapshot: &StoreSnapshot,
) {
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        return;
    };
    let doc = match library.load(name) {
        Ok(doc) => doc,
        Err(err) => {
            log_profile(
                state,
                LogLevel::Error,
                format!("failed to load vehicle profile '{name}': {err}"),
            );
            return;
        }
    };
    let Some(draft) = state.drafts.get_mut(draft_index) else {
        log_profile(
            state,
            LogLevel::Warning,
            format!("missing vehicle draft {draft_index} for profile '{name}'"),
        );
        return;
    };
    let cfg = match draft.source {
        Some(source) => doc.to_config_for_source(snapshot, source),
        None => doc.to_config(snapshot),
    };
    let Some(cfg) = cfg else {
        log_profile(
            state,
            LogLevel::Warning,
            format!("vehicle profile '{name}' does not match the current data"),
        );
        return;
    };

    draft.apply_config_preserving_label(&cfg, snapshot);
    draft.selected_profile = Some(name.to_owned());
    log_profile(
        state,
        LogLevel::Info,
        format!("applied vehicle profile '{name}'"),
    );
}

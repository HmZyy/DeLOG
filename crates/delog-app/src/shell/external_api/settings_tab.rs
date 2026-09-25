use std::sync::Arc;

use delog_api::control::AccessMode;
use delog_core::ingest::IngestSender;
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_remote::{ClientId, LeaseId, RemoteConfig, ServerStatus};

use super::{
    ExternalApiController, ExternalApiLimits, ExternalApiStatus, MIB, api_version_label,
    build_config, error_chain,
};
use crate::ui::logging::LogLevel;

pub(crate) const FULL_ACCESS_WARNING: &str = "Full control lets connected clients modify or remove manual state, including plots, traces, markers, vehicles, layouts, and playback.";

pub(crate) const LIMITS_NOTE: &str = "Changes apply the next time access is enabled.";

pub(crate) const FULL_ACCESS_ACTIVE: &str =
    "Full control is active: connected clients may modify or remove manual state.";

fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

pub(crate) fn access_label(access: AccessMode) -> &'static str {
    match access {
        AccessMode::Safe => "Safe",
        AccessMode::Full => "Full",
    }
}

#[derive(Default)]
struct Actions {
    enable: bool,
    disable: bool,
    revoke_client: Option<ClientId>,
    revoke_lease: Option<LeaseId>,
    access: Option<AccessMode>,
    confirm_full: bool,
    cancel_full: bool,
}

fn client_rows(ui: &mut egui::Ui, status: &ServerStatus, actions: &mut Actions) {
    if status.clients.is_empty() {
        ui.weak("No clients connected");
        return;
    }
    ui.vertical(|ui| {
        for client in &status.clients {
            ui.horizontal(|ui| {
                ui.label(&client.owner_name);
                ui.weak(access_label(status.access));
                if ui.button("Revoke").clicked() {
                    actions.revoke_client = Some(client.client_id.clone());
                }
            });
        }
    });
}

fn lease_rows(ui: &mut egui::Ui, status: &ServerStatus, actions: &mut Actions) {
    if status.leases.is_empty() {
        ui.weak("No active leases");
        return;
    }
    ui.vertical(|ui| {
        for lease in &status.leases {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} - {} active readers",
                    lease.id.as_str(),
                    lease.active_readers
                ));
                if ui.button("Revoke").clicked() {
                    actions.revoke_lease = Some(lease.id.clone());
                }
            });
        }
    });
}

fn activity(status: &ServerStatus) -> String {
    [
        counted(status.clients.len(), "client"),
        counted(status.leases.len(), "lease"),
        counted(status.active_downloads, "download"),
        counted(status.active_uploads, "upload"),
        counted(status.queued_controls, "queued control"),
    ]
    .join(", ")
}

struct LimitRow<'a, T> {
    label: &'a str,
    hover: &'a str,
    suffix: &'a str,
    in_use: Option<T>,
}

fn limit_row<T: egui::emath::Numeric + std::fmt::Display>(
    ui: &mut egui::Ui,
    row: LimitRow<'_, T>,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
) {
    ui.label(row.label).on_hover_text(row.hover);
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(value).range(range).suffix(row.suffix));
        if let Some(in_use) = row.in_use
            && in_use != *value
        {
            ui.weak(format!("in use: {in_use}{}", row.suffix));
        }
    });
    ui.end_row();
}

fn limit_rows(ui: &mut egui::Ui, limits: &mut ExternalApiLimits, running: Option<&RemoteConfig>) {
    let secs = |value: std::time::Duration| value.as_secs();
    limit_row(
        ui,
        LimitRow {
            label: "Snapshot idle release",
            hover: "Release a snapshot after this long without requests.",
            suffix: " s",
            in_use: running.map(|config| secs(config.lease_idle_timeout)),
        },
        &mut limits.lease_idle_timeout_secs,
        1..=3600,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Request timeout",
            hover: "Longest time a request may wait for capacity, and the shutdown grace period.",
            suffix: " s",
            in_use: running.map(|config| secs(config.request_timeout)),
        },
        &mut limits.request_timeout_secs,
        1..=600,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Concurrent downloads",
            hover: "Data streams served at the same time across all clients.",
            suffix: "",
            in_use: running.map(|config| config.max_concurrent_downloads),
        },
        &mut limits.max_concurrent_downloads,
        1..=64,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Upload size",
            hover: "Largest publication a client may upload.",
            suffix: " MiB",
            in_use: running.map(|config| config.uploads.limits.max_bytes / MIB),
        },
        &mut limits.upload_max_mib,
        1..=16 * 1024,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Upload rows",
            hover: "Most rows a single publication may contain.",
            suffix: "",
            in_use: running.map(|config| config.uploads.limits.max_rows),
        },
        &mut limits.upload_max_rows,
        1..=1_000_000_000,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Upload fields",
            hover: "Most fields a single publication may contain.",
            suffix: "",
            in_use: running.map(|config| config.uploads.limits.max_fields),
        },
        &mut limits.upload_max_fields,
        1..=65_536,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Concurrent uploads",
            hover: "Publications staged at the same time across all clients.",
            suffix: "",
            in_use: running.map(|config| config.uploads.max_concurrent),
        },
        &mut limits.max_concurrent_uploads,
        1..=16,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Queued controls",
            hover: "Control requests waiting for the DeLOG window across all clients.",
            suffix: "",
            in_use: running.map(|config| config.control.max_queued),
        },
        &mut limits.max_queued_controls,
        1..=256,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Queued controls per client",
            hover: "Control requests one client may have waiting at once.",
            suffix: "",
            in_use: running.map(|config| config.control.max_queued_per_client),
        },
        &mut limits.max_queued_controls_per_client,
        1..=256,
    );
    limit_row(
        ui,
        LimitRow {
            label: "Control timeout",
            hover: "How long a control request waits for the DeLOG window before it is cancelled.",
            suffix: " s",
            in_use: running.map(|config| secs(config.control.timeout)),
        },
        &mut limits.control_timeout_secs,
        1..=600,
    );
}

fn full_access_dialog(ctx: &egui::Context, actions: &mut Actions) {
    egui::Window::new("Allow full control")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(FULL_ACCESS_WARNING);
            ui.horizontal(|ui| {
                if ui.button("Allow full control").clicked() {
                    actions.confirm_full = true;
                }
                if ui.button("Cancel").clicked() {
                    actions.cancel_full = true;
                }
            });
        });
}

fn revoke_log(kind: &str, id: &str, removed: bool) -> (LogLevel, String) {
    (
        LogLevel::Info,
        format!(
            "External API: revoke of {kind} {id} {}",
            if removed {
                "succeeded"
            } else {
                "found nothing to revoke"
            }
        ),
    )
}

fn access_log(access: AccessMode) -> (LogLevel, String) {
    (
        LogLevel::Info,
        format!("External API access set to {}", access_label(access)),
    )
}

impl ExternalApiController {
    pub fn frame(
        &mut self,
        ctx: &egui::Context,
        snapshot: &StoreSnapshot,
    ) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        if let Err(error) = self.sync_session(snapshot) {
            logs.push((
                LogLevel::Error,
                format!(
                    "External API could not refresh its discovery label: {}",
                    error_chain(&error)
                ),
            ));
        }
        if !self.confirming_full {
            return logs;
        }
        let mut actions = Actions::default();
        full_access_dialog(ctx, &mut actions);
        if actions.confirm_full
            && let Some(applied) = self.confirm_full_access()
        {
            logs.push(access_log(applied));
        }
        if actions.cancel_full {
            self.cancel_full_access();
        }
        logs
    }

    pub fn settings_ui(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &StoreSnapshot,
        store: Arc<DataStore>,
        ingest: IngestSender,
    ) -> Vec<(LogLevel, String)> {
        let mut logs = Vec::new();
        let status = self.status();
        let server_status = self.server_status();
        let access = self.access_mode();
        let running_config = self.running_config().cloned();
        let last_error = self.last_error().map(str::to_owned);
        let mut actions = Actions::default();

        egui::Grid::new("settings-external-api-grid")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 10.0))
            .show(ui, |ui| {
                ui.label("Status").on_hover_text(
                    "External programs can connect only while access is enabled. Disabling revokes every client and keeps what they created.",
                );
                ui.horizontal(|ui| match &status {
                    ExternalApiStatus::Disabled => {
                        ui.label("Disabled");
                        if ui.button("Enable").clicked() {
                            actions.enable = true;
                        }
                    }
                    ExternalApiStatus::Running(running) => {
                        ui.label(format!("Running at {}", running.endpoint));
                        if ui.button("Disable").clicked() {
                            actions.disable = true;
                        }
                    }
                });
                ui.end_row();

                ui.label("API version");
                ui.label(api_version_label());
                ui.end_row();

                if let Some(error) = &last_error {
                    ui.label("Last error");
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    ui.end_row();
                }

                if let Some(access) = access {
                    ui.label("Access").on_hover_text(
                        "Safe lets clients change only what they created. Full also lets them modify or remove manual state.",
                    );
                    ui.horizontal(|ui| {
                        for mode in [AccessMode::Safe, AccessMode::Full] {
                            if ui
                                .selectable_label(access == mode, access_label(mode))
                                .clicked()
                                && access != mode
                            {
                                actions.access = Some(mode);
                            }
                        }
                    });
                    ui.end_row();
                    if access == AccessMode::Full {
                        ui.label("");
                        ui.colored_label(ui.visuals().warn_fg_color, FULL_ACCESS_ACTIVE);
                        ui.end_row();
                    }
                }

                if let ExternalApiStatus::Running(running) = &status {
                    ui.label("Instance ID").on_hover_text(
                        "Pass this ID to DeLOG.connect() to reach this window. The access token is never shown.",
                    );
                    ui.horizontal(|ui| {
                        ui.monospace(&running.instance_id);
                        if ui.button("Copy").clicked() {
                            ui.ctx().copy_text(running.instance_id.clone());
                        }
                    });
                    ui.end_row();
                }

                if let Some(server_status) = &server_status {
                    ui.label("Activity");
                    ui.label(activity(server_status));
                    ui.end_row();

                    ui.label("Clients");
                    client_rows(ui, server_status, &mut actions);
                    ui.end_row();

                    ui.label("Snapshot leases");
                    lease_rows(ui, server_status, &mut actions);
                    ui.end_row();
                }

                ui.label("Limits");
                ui.weak(LIMITS_NOTE);
                ui.end_row();
                limit_rows(ui, &mut self.limits, running_config.as_ref());
            });

        if crate::config::settings::reset_to_defaults_button(ui) {
            self.limits = ExternalApiLimits::default();
        }

        if actions.enable {
            let config = build_config(snapshot, self.limits, None);
            if let Err(error) = self.enable(config, store, ingest) {
                logs.push((
                    LogLevel::Error,
                    format!("External API failed to start: {}", error_chain(&error)),
                ));
            }
        }
        if actions.disable
            && let Err(error) = self.disable()
        {
            logs.push((
                LogLevel::Error,
                format!("External API did not shut down cleanly: {error}"),
            ));
        }
        if let Some(access) = actions.access
            && let Some(applied) = self.request_access_mode(access)
        {
            logs.push(access_log(applied));
        }
        if let Some(client) = actions.revoke_client {
            let removed = self.revoke_client(&client);
            logs.push(revoke_log("client", client.as_str(), removed));
        }
        if let Some(lease) = actions.revoke_lease {
            let removed = self.revoke_lease(&lease);
            logs.push(revoke_log("lease", lease.as_str(), removed));
        }
        logs
    }
}

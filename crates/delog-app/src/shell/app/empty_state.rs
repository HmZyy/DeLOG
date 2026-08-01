use crate::shell::app::context_header::ShellEmphasis;

pub const PIVOT: egui::Align2 = egui::Align2::CENTER_CENTER;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyStateAction {
    Open,
    OpenWith(String),
    ConnectLive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellModelInput {
    pub file_sources: usize,
    pub live_links: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveShellModel {
    pub emphasis: ShellEmphasis,
    pub sources: ShellModelInput,
    pub show_empty_state: bool,
    pub browser_available: bool,
    pub workspace_visible: bool,
}

pub const fn should_show_empty_state(source_count: usize, rows: usize) -> bool {
    source_count == 0 && rows == 0
}

pub const fn shell_model(sources: ShellModelInput, emphasis: ShellEmphasis) -> AdaptiveShellModel {
    AdaptiveShellModel {
        emphasis,
        show_empty_state: should_show_empty_state(
            sources.file_sources + sources.live_links,
            sources.rows,
        ),
        browser_available: true,
        workspace_visible: true,
        sources,
    }
}

pub fn show(
    ui: &mut egui::Ui,
    emphasis: ShellEmphasis,
    parsers: &[(&str, &str)],
) -> Vec<EmptyStateAction> {
    let mut actions = Vec::new();
    egui::Frame::popup(ui.style())
        .inner_margin(egui::Margin::same(24))
        .show(ui, |ui| {
            ui.set_width(420.0);
            ui.heading(match emphasis {
                ShellEmphasis::Offline => "Open a log to start analyzing",
                ShellEmphasis::Live => "Connect a live source",
            });
            ui.weak(
                "The data browser, plots, Inspector, and timeline will populate automatically.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let primary = match emphasis {
                    ShellEmphasis::Offline => crate::ui::components::icon_text_button(
                        ui,
                        crate::ui::icons::folder_open(),
                        "Open log…",
                        true,
                    )
                    .clicked()
                    .then_some(EmptyStateAction::Open),
                    ShellEmphasis::Live => crate::ui::components::icon_text_button(
                        ui,
                        crate::ui::icons::satellite_dish(),
                        "Connect live…",
                        true,
                    )
                    .clicked()
                    .then_some(EmptyStateAction::ConnectLive),
                };
                if let Some(primary) = primary {
                    actions.push(primary);
                }
                if emphasis == ShellEmphasis::Live && ui.button("Open log…").clicked() {
                    actions.push(EmptyStateAction::Open);
                }
                if emphasis == ShellEmphasis::Offline && ui.button("Connect live…").clicked() {
                    actions.push(EmptyStateAction::ConnectLive);
                }
                ui.menu_button("Open with", |ui| {
                    for (id, label) in parsers {
                        if ui.button(*label).clicked() {
                            actions.push(EmptyStateAction::OpenWith((*id).to_owned()));
                            ui.close();
                        }
                    }
                });
            });
            ui.add_space(12.0);
            ui.separator();
            ui.weak("Supported: ArduPilot BIN · PX4 ULog · MAVLink TLOG · Parquet");
            ui.weak("Multiple files can be opened together and synchronized later.");
        });
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_is_visible_only_without_sources_or_rows() {
        assert!(should_show_empty_state(0, 0));
        assert!(!should_show_empty_state(1, 0));
        assert!(!should_show_empty_state(1, 42));
    }

    #[test]
    fn emphasis_transition_has_no_session_side_effect() {
        let before = ShellModelInput {
            file_sources: 1,
            live_links: 1,
            rows: 42,
        };
        let after = shell_model(before, ShellEmphasis::Live).sources;
        assert_eq!(after, before);
    }

    #[test]
    fn simultaneous_file_and_live_sources_stay_visible_in_both_emphases() {
        let sources = ShellModelInput {
            file_sources: 1,
            live_links: 1,
            rows: 42,
        };
        let offline = shell_model(sources, ShellEmphasis::Offline);
        let live = shell_model(sources, ShellEmphasis::Live);
        assert_eq!(offline.sources, live.sources);
        assert!(!offline.show_empty_state);
        assert!(!live.show_empty_state);
        assert_eq!(offline.browser_available, live.browser_available);
        assert_eq!(offline.workspace_visible, live.workspace_visible);
        assert_ne!(offline.emphasis, live.emphasis);
    }
}

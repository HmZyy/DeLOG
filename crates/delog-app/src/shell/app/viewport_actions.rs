use crate::shell::app::RunKind;
use crate::shell::app::commands::{AppCommand, CommandId};
use crate::shell::windows::WindowId;

pub(crate) const SHORTCUT_KEYS: &[egui::Key] = &[
    egui::Key::F1,
    egui::Key::F2,
    egui::Key::F3,
    egui::Key::F9,
    egui::Key::F12,
    egui::Key::Space,
    egui::Key::Home,
    egui::Key::End,
    egui::Key::ArrowLeft,
    egui::Key::ArrowRight,
    egui::Key::S,
    egui::Key::L,
    egui::Key::R,
    egui::Key::M,
    egui::Key::E,
    egui::Key::T,
    egui::Key::O,
    egui::Key::Equals,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ShortcutScope {
    Anywhere,
    WhenKeyboardIsFree,
}

impl ShortcutScope {
    pub(crate) fn allows(self, wants_keyboard: bool) -> bool {
        matches!(self, Self::Anywhere) || !wants_keyboard
    }
}

pub(crate) fn shortcut_for_key(
    key: egui::Key,
    command_modifier: bool,
) -> Option<(CommandId, ShortcutScope)> {
    use ShortcutScope::{Anywhere, WhenKeyboardIsFree};
    match (key, command_modifier) {
        (egui::Key::S, true) => Some((CommandId::SaveLayout, Anywhere)),
        (egui::Key::L, true) => Some((CommandId::LoadLayout, Anywhere)),
        (egui::Key::R, true) => Some((CommandId::RunPalette, Anywhere)),
        (egui::Key::E, true) => Some((CommandId::ToggleDataBrowser, Anywhere)),
        (egui::Key::T, true) => Some((CommandId::ToggleScene3d, Anywhere)),
        (egui::Key::O, true) => Some((CommandId::Open, Anywhere)),
        (egui::Key::F1, _) => Some((CommandId::OpenDiagnostics, Anywhere)),
        (egui::Key::F2, _) => Some((CommandId::OpenPerformance, Anywhere)),
        (egui::Key::F3, _) => Some((CommandId::OpenMarkers, Anywhere)),
        (egui::Key::F9, _) => Some((CommandId::OpenScripting, Anywhere)),
        (egui::Key::F12, _) => Some((CommandId::OpenLogging, Anywhere)),
        (egui::Key::Space, _) => Some((CommandId::TogglePlayback, WhenKeyboardIsFree)),
        (egui::Key::Home, _) => Some((CommandId::JumpStart, WhenKeyboardIsFree)),
        (egui::Key::End, _) => Some((CommandId::JumpEnd, WhenKeyboardIsFree)),
        (egui::Key::ArrowLeft, _) => Some((CommandId::StepLeft, WhenKeyboardIsFree)),
        (egui::Key::ArrowRight, _) => Some((CommandId::StepRight, WhenKeyboardIsFree)),
        (egui::Key::M, _) => Some((CommandId::AddMarker, WhenKeyboardIsFree)),
        (egui::Key::Equals, _) => Some((CommandId::EqualizePlots, WhenKeyboardIsFree)),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandInvocation {
    pub(crate) origin: WindowId,
    pub(crate) command: AppCommand,
}

impl CommandInvocation {
    pub(crate) fn new(origin: WindowId, command: AppCommand) -> Self {
        Self { origin, command }
    }

    pub(crate) fn main(command: AppCommand) -> Self {
        Self::new(WindowId::MAIN, command)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerFlow {
    CommandPalette,
    LoadLayout,
    RunScript,
    RunPalette,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PickerHost {
    pub(crate) window: WindowId,
    pub(crate) flow: PickerFlow,
}

impl PickerHost {
    pub(crate) fn new(window: WindowId, flow: PickerFlow) -> Self {
        Self { window, flow }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewportAction {
    Shortcut(CommandInvocation),
    ToggleCommandPalette {
        origin: WindowId,
    },
    Invoke {
        host: PickerHost,
        invocation: CommandInvocation,
    },
    PickerClosed {
        host: PickerHost,
    },
    ChooseRunKind {
        host: PickerHost,
        kind: RunKind,
    },
    RunItem {
        host: PickerHost,
        kind: RunKind,
        name: String,
    },
}

impl ViewportAction {
    pub(crate) fn origin(&self) -> WindowId {
        match self {
            Self::Shortcut(invocation) => invocation.origin,
            Self::ToggleCommandPalette { origin } => *origin,
            Self::Invoke { host, .. }
            | Self::PickerClosed { host }
            | Self::ChooseRunKind { host, .. }
            | Self::RunItem { host, .. } => host.window,
        }
    }
}

pub(crate) fn discard_picker_actions_from(actions: &mut Vec<ViewportAction>, closed: WindowId) {
    actions.retain(|action| {
        matches!(action, ViewportAction::Shortcut(_)) || action.origin() != closed
    });
}

pub(crate) fn command_palette_host_after_toggle(
    current: Option<PickerHost>,
    origin: WindowId,
) -> Option<PickerHost> {
    let requested = PickerHost::new(origin, PickerFlow::CommandPalette);
    (current != Some(requested)).then_some(requested)
}

pub(crate) fn picker_host_after_close(
    current: Option<PickerHost>,
    closed: PickerHost,
) -> Option<PickerHost> {
    if current == Some(closed) {
        None
    } else {
        current
    }
}

pub(crate) fn collect_shortcut_actions(
    ctx: &egui::Context,
    origin: WindowId,
    command_palette_open: bool,
) -> Vec<ViewportAction> {
    if !ctx.input(|input| input.focused) {
        return Vec::new();
    }

    let wants_keyboard = ctx.egui_wants_keyboard_input();
    let palette_shortcut = ctx.input(|input| {
        input.modifiers.command && input.modifiers.shift && input.key_pressed(egui::Key::P)
    });
    let mut actions = Vec::new();
    if crate::shell::app::command_palette::should_toggle_palette(palette_shortcut, wants_keyboard) {
        actions.push(ViewportAction::ToggleCommandPalette { origin });
    }

    if command_palette_open {
        return actions;
    }

    actions.extend(ctx.input(|input| {
        SHORTCUT_KEYS
            .iter()
            .copied()
            .filter(|key| input.key_pressed(*key))
            .filter_map(|key| shortcut_for_key(key, input.modifiers.command))
            .filter(|(_, scope)| scope.allows(wants_keyboard))
            .map(|(command, _)| {
                ViewportAction::Shortcut(CommandInvocation::new(
                    origin,
                    AppCommand::Static(command),
                ))
            })
            .collect::<Vec<_>>()
    }));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::app::commands::{AppCommand, CommandId};
    use crate::shell::windows::WindowId;

    fn key_press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn collect_frame(
        ctx: &egui::Context,
        viewport_id: egui::ViewportId,
        focused: bool,
        origin: WindowId,
        events: Vec<egui::Event>,
    ) -> Vec<ViewportAction> {
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let mut actions = Vec::new();
        let _ = ctx.run(
            egui::RawInput {
                viewport_id,
                viewports: std::iter::once((viewport_id, Default::default())).collect(),
                focused,
                modifiers,
                events,
                ..Default::default()
            },
            |ctx| actions = collect_shortcut_actions(ctx, origin, false),
        );
        actions
    }

    #[test]
    fn closing_a_picker_host_discards_only_picker_actions_for_that_host() {
        let closed = WindowId(4);
        let host = PickerHost::new(closed, PickerFlow::RunPalette);
        let main_host = PickerHost::new(WindowId::MAIN, PickerFlow::CommandPalette);
        let global = ViewportAction::Shortcut(CommandInvocation::new(
            closed,
            AppCommand::Static(CommandId::EqualizePlots),
        ));
        let main_shortcut = ViewportAction::Shortcut(CommandInvocation::main(AppCommand::Static(
            CommandId::TogglePlayback,
        )));
        let main_invoke = ViewportAction::Invoke {
            host: main_host,
            invocation: CommandInvocation::main(AppCommand::FitAll),
        };
        let other_toggle = ViewportAction::ToggleCommandPalette {
            origin: WindowId(5),
        };
        let mut actions = vec![
            main_shortcut.clone(),
            global.clone(),
            ViewportAction::ToggleCommandPalette { origin: closed },
            main_invoke.clone(),
            ViewportAction::Invoke {
                host,
                invocation: CommandInvocation::new(closed, AppCommand::FitAll),
            },
            ViewportAction::ChooseRunKind {
                host,
                kind: RunKind::Script,
            },
            ViewportAction::RunItem {
                host,
                kind: RunKind::Script,
                name: "demo".to_owned(),
            },
            ViewportAction::PickerClosed { host },
            other_toggle.clone(),
        ];

        discard_picker_actions_from(&mut actions, closed);

        assert_eq!(
            actions,
            vec![main_shortcut, global, main_invoke, other_toggle]
        );
    }

    #[test]
    fn same_host_palette_toggle_closes_it() {
        let host = PickerHost::new(WindowId(1), PickerFlow::CommandPalette);

        assert_eq!(
            command_palette_host_after_toggle(Some(host), WindowId(1)),
            None
        );
        assert_eq!(
            command_palette_host_after_toggle(None, WindowId(1)),
            Some(host)
        );
    }

    #[test]
    fn palette_toggle_from_another_window_moves_the_host() {
        let host = PickerHost::new(WindowId(1), PickerFlow::CommandPalette);

        assert_eq!(
            command_palette_host_after_toggle(Some(host), WindowId(2)),
            Some(PickerHost::new(WindowId(2), PickerFlow::CommandPalette))
        );
        assert_eq!(
            command_palette_host_after_toggle(Some(host), WindowId::MAIN),
            Some(PickerHost::new(WindowId::MAIN, PickerFlow::CommandPalette))
        );
    }

    #[test]
    fn opening_a_different_picker_replaces_the_previous_host() {
        let load_layout = PickerHost::new(WindowId(1), PickerFlow::LoadLayout);

        assert_eq!(
            command_palette_host_after_toggle(Some(load_layout), WindowId(1)),
            Some(PickerHost::new(WindowId(1), PickerFlow::CommandPalette))
        );
    }

    #[test]
    fn stale_picker_close_does_not_close_a_replacement_host() {
        let replacement = PickerHost::new(WindowId(2), PickerFlow::LoadLayout);
        let former = PickerHost::new(WindowId(1), PickerFlow::CommandPalette);

        assert_eq!(
            picker_host_after_close(Some(replacement), former),
            Some(replacement)
        );
        assert_eq!(
            picker_host_after_close(Some(replacement), replacement),
            None
        );
    }

    #[test]
    fn focused_viewport_shortcuts_keep_the_supplied_window_origin() {
        let ctx = egui::Context::default();
        let origin = WindowId(7);

        let actions = collect_frame(
            &ctx,
            origin.viewport_id(),
            true,
            origin,
            vec![key_press(egui::Key::E, egui::Modifiers::COMMAND)],
        );

        assert_eq!(
            actions,
            vec![ViewportAction::Shortcut(CommandInvocation::new(
                origin,
                AppCommand::Static(CommandId::ToggleDataBrowser),
            ))]
        );
    }

    #[test]
    fn unfocused_viewport_emits_no_shortcut_actions() {
        let ctx = egui::Context::default();
        let origin = WindowId(3);

        let actions = collect_frame(
            &ctx,
            origin.viewport_id(),
            false,
            origin,
            vec![key_press(egui::Key::Space, egui::Modifiers::NONE)],
        );

        assert!(actions.is_empty());
    }

    #[test]
    fn only_the_focused_viewport_emits_when_focus_moves() {
        let ctx = egui::Context::default();
        let first = WindowId(1);
        let second = WindowId(2);

        let unfocused = collect_frame(
            &ctx,
            first.viewport_id(),
            false,
            first,
            vec![key_press(egui::Key::Space, egui::Modifiers::NONE)],
        );
        let focused = collect_frame(
            &ctx,
            second.viewport_id(),
            true,
            second,
            vec![key_press(egui::Key::Space, egui::Modifiers::NONE)],
        );

        assert!(unfocused.is_empty());
        assert_eq!(
            focused,
            vec![ViewportAction::Shortcut(CommandInvocation::new(
                second,
                AppCommand::Static(CommandId::TogglePlayback),
            ))]
        );
    }

    #[test]
    fn space_home_end_and_arrows_emit_from_an_extended_origin() {
        let cases = [
            (egui::Key::Space, CommandId::TogglePlayback),
            (egui::Key::Home, CommandId::JumpStart),
            (egui::Key::End, CommandId::JumpEnd),
            (egui::Key::ArrowLeft, CommandId::StepLeft),
            (egui::Key::ArrowRight, CommandId::StepRight),
        ];

        for (key, command) in cases {
            let ctx = egui::Context::default();
            let origin = WindowId(5);
            let actions = collect_frame(
                &ctx,
                origin.viewport_id(),
                true,
                origin,
                vec![key_press(key, egui::Modifiers::NONE)],
            );

            assert_eq!(
                actions,
                vec![ViewportAction::Shortcut(CommandInvocation::new(
                    origin,
                    AppCommand::Static(command),
                ))],
                "{key:?} should retain the extended origin"
            );
        }
    }

    #[test]
    fn extended_text_focus_suppresses_typed_shortcuts_but_not_ctrl_e() {
        fn frame(
            ctx: &egui::Context,
            text: &mut String,
            focus: bool,
            events: Vec<egui::Event>,
        ) -> Vec<ViewportAction> {
            let modifiers = events
                .iter()
                .find_map(|event| match event {
                    egui::Event::Key { modifiers, .. } => Some(*modifiers),
                    _ => None,
                })
                .unwrap_or_default();
            let mut actions = Vec::new();
            let _ = ctx.run(
                egui::RawInput {
                    viewport_id: WindowId(7).viewport_id(),
                    viewports: std::iter::once((WindowId(7).viewport_id(), Default::default()))
                        .collect(),
                    focused: true,
                    modifiers,
                    events,
                    ..Default::default()
                },
                |ctx| {
                    actions = collect_shortcut_actions(ctx, WindowId(7), false);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.add(egui::TextEdit::singleline(text));
                        if focus {
                            response.request_focus();
                        }
                    });
                },
            );
            actions
        }

        let ctx = egui::Context::default();
        let mut text = String::new();
        frame(&ctx, &mut text, true, Vec::new());
        assert!(ctx.egui_wants_keyboard_input());

        let typed = frame(
            &ctx,
            &mut text,
            false,
            vec![
                key_press(egui::Key::Space, egui::Modifiers::NONE),
                egui::Event::Text(" ".to_owned()),
                key_press(egui::Key::ArrowLeft, egui::Modifiers::NONE),
            ],
        );
        assert!(typed.is_empty());
        assert_eq!(text, " ");

        let toggled = frame(
            &ctx,
            &mut text,
            false,
            vec![key_press(egui::Key::E, egui::Modifiers::COMMAND)],
        );
        assert_eq!(
            toggled,
            vec![ViewportAction::Shortcut(CommandInvocation::new(
                WindowId(7),
                AppCommand::Static(CommandId::ToggleDataBrowser),
            ))]
        );
    }
}

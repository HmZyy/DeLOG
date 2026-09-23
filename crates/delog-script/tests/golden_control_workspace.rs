#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_script::{
    ControlHost, ControlRequest, ControlResponse, PlaybackRequest, PlotInfo, SplitDirection,
    WorkspaceRequest,
};

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<ControlRequest>>,
}

impl ControlHost for Recorder {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::Plots(_) => Ok(ControlResponse::Plots(vec![plot_fixture()])),
            ControlRequest::Workspace(WorkspaceRequest::Split { .. }) => {
                Ok(ControlResponse::Plots(vec![plot_fixture()]))
            }
            ControlRequest::Workspace(WorkspaceRequest::AddPlot { .. }) => {
                Ok(ControlResponse::Plots(vec![plot_fixture()]))
            }
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow { .. }) => {
                Ok(ControlResponse::Window(1))
            }
            _ => Ok(ControlResponse::Unit),
        }
    }
}

fn plot_fixture() -> PlotInfo {
    PlotInfo {
        window: 0,
        tile: 7,
        index: 0,
        label: "Plot 1".into(),
    }
}

#[test]
fn splitting_a_plot_names_the_pane_and_the_direction() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.workspace.split(delog.plots()[0], 'vertical')",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Workspace(WorkspaceRequest::Split { window: 0, tile: 7, direction })
            if *direction == SplitDirection::Vertical
    )));
}

#[test]
fn an_unknown_split_direction_is_rejected_before_any_round_trip() {
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.workspace.split(delog.plots()[0], 'diagonal')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(
        !recorder
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, ControlRequest::Workspace(WorkspaceRequest::Split { .. })))
    );
}

#[test]
fn playback_speed_and_follow_live_are_settable() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.playback.speed = 2.0\ndelog.playback.follow_live = True",
    )
    .unwrap();
    let seen = recorder.seen.lock().unwrap();
    assert!(seen.iter().any(|r| matches!(
        r,
        ControlRequest::Playback(PlaybackRequest::Set { speed: Some(s), .. }) if (*s - 2.0).abs() < 1e-9
    )));
    assert!(seen.iter().any(|r| matches!(
        r,
        ControlRequest::Playback(PlaybackRequest::Set {
            follow_live: Some(true),
            ..
        })
    )));
}

#[test]
fn a_non_finite_playback_speed_is_rejected_before_any_round_trip() {
    let recorder = Arc::new(Recorder::default());
    for expression in [
        "delog.playback.speed = float('nan')",
        "delog.playback.speed = float('inf')",
    ] {
        let error = delog_script::control::testing::eval_with_host(recorder.clone(), expression)
            .unwrap_err();
        assert!(error.contains("ValueError"), "{error}");
    }
    assert!(
        !recorder
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, ControlRequest::Playback(PlaybackRequest::Set { .. })))
    );
}

#[test]
fn adding_a_plot_returns_a_new_handle() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.workspace.add_plot(split='horizontal')",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Workspace(WorkspaceRequest::AddPlot { direction })
            if *direction == SplitDirection::Horizontal
    )));
}

#[test]
fn an_unknown_add_plot_direction_is_rejected_before_any_round_trip() {
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.workspace.add_plot(split='diagonal')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(!recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Workspace(WorkspaceRequest::AddPlot { .. })
    )));
}

#[test]
fn closing_a_plot_sends_its_window_and_tile() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.workspace.close(delog.plots()[0])",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Workspace(WorkspaceRequest::Close { window: 0, tile: 7 })
    )));
}

#[test]
fn equalize_and_show_scene_reach_the_host() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.workspace.equalize()\ndelog.workspace.show_scene(True)",
    )
    .unwrap();
    let seen = recorder.seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|r| matches!(r, ControlRequest::Workspace(WorkspaceRequest::Equalize)))
    );
    assert!(seen.iter().any(|r| matches!(
        r,
        ControlRequest::Workspace(WorkspaceRequest::ShowScene { visible: true })
    )));
}

#[test]
fn opening_a_window_names_its_title() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.windows.open(title='Compare')",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Workspace(WorkspaceRequest::OpenWindow { title })
            if title.as_deref() == Some("Compare")
    )));
}

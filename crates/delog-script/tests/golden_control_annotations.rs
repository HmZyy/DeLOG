#![cfg(feature = "python")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use delog_api::control::{
    AnnotationFilter, AnnotationGeometry, AnnotationInfo, AnnotationKind, AnnotationRequest,
    ControlHost, ControlRequest, ControlResponse, PlotInfo,
};

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<ControlRequest>>,
    annotations: Mutex<Vec<AnnotationInfo>>,
    next_id: Mutex<HashMap<(u64, u64), u64>>,
}

impl ControlHost for Recorder {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::Plots(_) => Ok(ControlResponse::Plots(vec![
                plot_fixture(),
                plot_fixture_b(),
            ])),
            ControlRequest::Annotations(AnnotationRequest::Add {
                window,
                tile,
                geometry,
                label,
                style,
                owner,
            }) => {
                let mut next_id = self.next_id.lock().unwrap();
                let slot = next_id.entry((window, tile)).or_insert(0);
                let id = *slot;
                *slot += 1;
                let mut annotations = self.annotations.lock().unwrap();
                let index = annotations
                    .iter()
                    .filter(|info| info.window == window && info.tile == tile)
                    .count();
                let info = AnnotationInfo {
                    plot_instance_id: 1,
                    window,
                    tile,
                    id,
                    index,
                    kind: geometry.kind(),
                    geometry,
                    label,
                    color: style.color.unwrap_or([1.0, 1.0, 1.0, 1.0]),
                    owner: owner.map(|owner| owner.name),
                };
                annotations.push(info.clone());
                Ok(ControlResponse::Annotations(vec![info]))
            }
            ControlRequest::Annotations(AnnotationRequest::List { target }) => {
                let annotations = self.annotations.lock().unwrap();
                Ok(ControlResponse::Annotations(match target {
                    Some((window, tile)) => annotations
                        .iter()
                        .filter(|info| info.window == window && info.tile == tile)
                        .cloned()
                        .collect(),
                    None => annotations.clone(),
                }))
            }
            _ => Ok(ControlResponse::Unit),
        }
    }
}

fn plot_fixture() -> PlotInfo {
    PlotInfo {
        instance_id: 1,
        owner: None,
        window: 0,
        tile: 7,
        index: 0,
        label: "Plot 1".into(),
    }
}

fn plot_fixture_b() -> PlotInfo {
    PlotInfo {
        instance_id: 1,
        owner: None,
        window: 0,
        tile: 8,
        index: 1,
        label: "Plot 2".into(),
    }
}

#[test]
fn each_annotation_kind_reaches_the_app_with_its_own_geometry() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "p = delog.plots()[0]\n\
         p.annotations.add_text((100, 1.0), 'burst')\n\
         p.annotations.add_segment((100, 1.0), (200, 2.0), arrow=True)\n\
         p.annotations.add_rect((100, 1.0), (200, 2.0), fill_opacity=0.15)\n\
         p.annotations.add_ellipse((100, 1.0), (200, 2.0))\n\
         p.annotations.add_hline(9.81, label='1g')\n",
    )
    .unwrap();
    let seen = recorder.seen.lock().unwrap();
    let geometries: Vec<AnnotationGeometry> = seen
        .iter()
        .filter_map(|r| match r {
            ControlRequest::Annotations(AnnotationRequest::Add { geometry, .. }) => {
                Some(geometry.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        geometries,
        vec![
            AnnotationGeometry::Text { at: (100, 1.0) },
            AnnotationGeometry::Segment {
                from: (100, 1.0),
                to: (200, 2.0)
            },
            AnnotationGeometry::Rect {
                a: (100, 1.0),
                b: (200, 2.0)
            },
            AnnotationGeometry::Ellipse {
                a: (100, 1.0),
                b: (200, 2.0)
            },
            AnnotationGeometry::HLine { y: 9.81 },
        ]
    );
}

#[test]
fn a_malformed_color_is_rejected_before_any_round_trip() {
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.plots()[0].annotations.add_hline(1.0, color='blue')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(!recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Annotations(AnnotationRequest::Add { .. })
    )));
}

#[test]
fn a_scripted_annotation_carries_the_running_scripts_owner() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_named_with_host(
        recorder.clone(),
        "flight.py",
        4,
        "delog.plots()[0].annotations.add_hline(9.81)",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Annotations(AnnotationRequest::Add { owner: Some(o), .. })
            if o.name == "flight.py" && o.generation == 4
    )));
}

#[test]
fn every_removal_axis_reaches_the_app_as_its_own_filter() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "a = delog.plots()[0].annotations\n\
         a.remove(0)\n\
         a.remove(kind='rect')\n\
         a.remove(label='1g')\n\
         a.remove(owner='flight.py')\n\
         a.clear()\n",
    )
    .unwrap();
    let filters: Vec<AnnotationFilter> = recorder
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            ControlRequest::Annotations(AnnotationRequest::Remove { filter, .. }) => {
                Some(filter.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        filters,
        vec![
            AnnotationFilter::Index(0),
            AnnotationFilter::Kind(AnnotationKind::Rect),
            AnnotationFilter::Label("1g".into()),
            AnnotationFilter::Owner("flight.py".into()),
            AnnotationFilter::All,
        ]
    );
}

#[test]
fn combining_two_removal_filters_is_rejected_before_any_round_trip() {
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.plots()[0].annotations.remove(0, kind='rect')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(!recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Annotations(AnnotationRequest::Remove { .. })
    )));
}

#[test]
fn a_global_removal_names_no_single_plot() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "delog.annotations.remove(kind='hline')",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Annotations(AnnotationRequest::Remove { target: None, filter })
            if *filter == AnnotationFilter::Kind(AnnotationKind::HLine)
    )));
}

#[test]
fn removing_a_handle_from_another_plot_through_this_collection_is_rejected() {
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "plots = delog.plots()\n\
         a = plots[0].annotations.add_hline(1.0, 'a')\n\
         plots[1].annotations.remove(a)\n",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(!recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Annotations(AnnotationRequest::Remove { .. })
    )));
}

#[test]
fn removing_a_handle_through_the_global_collection_targets_its_own_pane() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        "plots = delog.plots()\n\
         a = plots[1].annotations.add_hline(1.0, 'a')\n\
         delog.annotations.remove(a)\n",
    )
    .unwrap();
    assert!(recorder.seen.lock().unwrap().iter().any(|r| matches!(
        r,
        ControlRequest::Annotations(AnnotationRequest::Remove {
            target: Some((0, 8)),
            filter
        }) if *filter == AnnotationFilter::Id(0)
    )));
}

#[test]
fn the_annotation_handle_surface_round_trips_through_python() {
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host(
        recorder.clone(),
        r#"
p = delog.plots()[0]
a = p.annotations.add_rect((0, 0.0), (1, 1.0), 'box')
assert a.kind == 'rect', a.kind
assert a.label == 'box', a.label
assert isinstance(a.color, str), a.color
assert a.owner is None

a.move_to((5, 5.0))

raised_on_a_non_hline_handle = False
try:
    a.y
except ValueError:
    raised_on_a_non_hline_handle = True
assert raised_on_a_non_hline_handle

h = p.annotations.add_hline(9.81, 'g')
h.y = 1.0

raised_on_move_to_an_hline_handle = False
try:
    h.move_to((0, 0.0))
except ValueError:
    raised_on_move_to_an_hline_handle = True
assert raised_on_move_to_an_hline_handle

assert len(p.annotations) == 2, len(p.annotations)
assert p.annotations[0].id == a.id
assert p.annotations[1].id == h.id

assert len(delog.annotations) == 2, len(delog.annotations)
assert len(delog.annotations.list()) == 2
names = [entry.kind for entry in delog.annotations]
assert names == ['rect', 'hline'], names
"#,
    )
    .unwrap();
}

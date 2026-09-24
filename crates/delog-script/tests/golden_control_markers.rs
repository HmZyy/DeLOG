#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_api::control::{
    ControlHost, ControlRequest, ControlResponse, MarkerFilter, MarkerInfo, MarkerOrigin,
    MarkerRequest,
};

struct MarkerHost {
    seen: Mutex<Vec<ControlRequest>>,
    markers: Mutex<Vec<MarkerInfo>>,
}

impl Default for MarkerHost {
    fn default() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            markers: Mutex::new(vec![
                MarkerInfo {
                    id: 7,
                    index: 0,
                    t_us: 10,
                    label: "manual".into(),
                    color: [1.0, 1.0, 1.0, 1.0],
                    note: String::new(),
                    origin: MarkerOrigin::Manual,
                    owner: None,
                },
                MarkerInfo {
                    id: 11,
                    index: 1,
                    t_us: 20,
                    label: "armed".into(),
                    color: [0.0, 1.0, 0.0, 1.0],
                    note: "detected".into(),
                    origin: MarkerOrigin::Script,
                    owner: Some("flight.py".into()),
                },
            ]),
        }
    }
}

impl MarkerHost {
    fn taken(&self) -> Vec<ControlRequest> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

impl ControlHost for MarkerHost {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::Markers(MarkerRequest::List) => Ok(ControlResponse::Markers(
                self.markers.lock().unwrap().clone(),
            )),
            ControlRequest::Markers(MarkerRequest::Set { id, patch }) => {
                let mut markers = self.markers.lock().unwrap();
                let marker = markers
                    .iter_mut()
                    .find(|marker| marker.id == id)
                    .ok_or_else(|| delog_api::Error::execution(format!("marker {id} is gone")))?;
                if let Some(t_us) = patch.t_us {
                    marker.t_us = t_us;
                }
                if let Some(label) = patch.label {
                    marker.label = label;
                }
                if let Some(color) = patch.color {
                    marker.color = color;
                }
                if let Some(note) = patch.note {
                    marker.note = note;
                }
                Ok(ControlResponse::Unit)
            }
            ControlRequest::Markers(MarkerRequest::Remove(_)) => Ok(ControlResponse::Unit),
            other => Err(delog_api::Error::execution(format!(
                "unexpected request: {other:?}"
            ))),
        }
    }
}

#[test]
fn marker_collection_and_stable_handle_surface_round_trip() {
    let host = Arc::new(MarkerHost::default());
    delog_script::control::testing::eval_with_host(
        host.clone(),
        "assert len(delog.markers) == 2\n\
         assert [m.label for m in delog.markers] == ['manual', 'armed']\n\
         m = delog.markers[1]\n\
         assert m.id == 11\n\
         assert m.index == 1\n\
         assert m.origin == 'script'\n\
         assert m.owner == 'flight.py'\n\
         m.label = 'takeoff'\n\
         m.color = '#E74C3C'\n\
         m.note = 'confirmed'\n\
         m.t_us = 5\n\
         delog.markers.remove(m)",
    )
    .unwrap();

    let seen = host.taken();
    assert!(seen.iter().any(|request| matches!(
        request,
        ControlRequest::Markers(MarkerRequest::Set { id: 11, .. })
    )));
    assert!(seen.iter().any(|request| matches!(
        request,
        ControlRequest::Markers(MarkerRequest::Remove(MarkerFilter::Id(11)))
    )));
}

#[test]
fn marker_removal_axes_preserve_manual_protection_in_the_request() {
    let host = Arc::new(MarkerHost::default());
    delog_script::control::testing::eval_with_host(
        host.clone(),
        "delog.markers.remove(0)\n\
         delog.markers.remove(owner='flight.py')\n\
         delog.markers.remove(origin='manual')\n\
         delog.markers.remove(label='landing')\n\
         delog.markers.remove(after=10, before=20)\n\
         delog.markers.clear()\n\
         delog.markers.clear(manual=True)",
    )
    .unwrap();

    let filters = host
        .taken()
        .into_iter()
        .filter_map(|request| match request {
            ControlRequest::Markers(MarkerRequest::Remove(filter)) => Some(filter),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        filters,
        [
            MarkerFilter::Index(0),
            MarkerFilter::Owner("flight.py".into()),
            MarkerFilter::Origin(MarkerOrigin::Manual),
            MarkerFilter::ScriptLabel("landing".into()),
            MarkerFilter::ScriptTimeRange {
                after: Some(10),
                before: Some(20),
            },
            MarkerFilter::ScriptAll,
            MarkerFilter::All,
        ]
    );
}

#[test]
fn invalid_marker_calls_fail_before_a_round_trip() {
    for statement in [
        "delog.markers.remove()",
        "delog.markers.remove(0, label='x')",
        "delog.markers.remove(origin='other')",
        "delog.markers.remove(after=20, before=10)",
        "delog.markers.add(1, '')",
        "delog.markers.extend([(1, 'ok'), (2, '')])",
    ] {
        let host = Arc::new(MarkerHost::default());
        let error =
            delog_script::control::testing::eval_with_host(host.clone(), statement).unwrap_err();
        assert!(error.contains("ValueError"), "{statement}: {error}");
        assert!(host.taken().is_empty(), "host called for {statement}");
    }
}

#[test]
fn marker_add_and_extend_share_the_deferred_buffer_with_legacy_add_marker() {
    let host = Arc::new(MarkerHost::default());
    let staged = delog_script::control::testing::eval_with_host_and_staged_markers(
        host.clone(),
        "delog.add_marker(30, 'legacy')\n\
         delog.markers.add(10, 'armed', color='#2ECC71', note='detected')\n\
         delog.markers.extend([(20, 'takeoff'), (40, 'landing')])",
    )
    .unwrap();

    assert!(host.taken().is_empty());
    assert_eq!(
        staged
            .iter()
            .map(|marker| (marker.time_us, marker.label.as_str()))
            .collect::<Vec<_>>(),
        [
            (30, "legacy"),
            (10, "armed"),
            (20, "takeoff"),
            (40, "landing")
        ]
    );
    assert_eq!(
        staged[1].color,
        Some([46.0 / 255.0, 204.0 / 255.0, 113.0 / 255.0, 1.0])
    );
    assert_eq!(staged[1].note, "detected");
}

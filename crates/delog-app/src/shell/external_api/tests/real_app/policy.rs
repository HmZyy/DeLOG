use super::*;

#[derive(Debug, PartialEq)]
struct Inventory {
    windows: Vec<(u64, Option<String>)>,
    plots: Vec<(u64, Option<String>)>,
    traces: Vec<(u64, Option<String>)>,
    markers: Vec<String>,
    vehicles: Vec<(u64, Option<String>)>,
}

impl HeadlessExternalApp {
    fn inventory(&mut self) -> Inventory {
        Inventory {
            windows: self
                .windows()
                .into_iter()
                .map(|item| (item.id, item.owner.map(|owner| owner.name)))
                .collect(),
            plots: self
                .plots()
                .into_iter()
                .map(|item| (item.instance_id, item.owner.map(|owner| owner.name)))
                .collect(),
            traces: self
                .traces()
                .into_iter()
                .map(|item| (item.instance_id, item.owner.map(|owner| owner.name)))
                .collect(),
            markers: self.marker_labels(),
            vehicles: self
                .vehicles()
                .into_iter()
                .map(|item| (item.id, item.spec.owner.map(|owner| owner.name)))
                .collect(),
        }
    }

    fn publish_diagnosis(&mut self) {
        self.serve(|api| {
            let client = api.connect(true);
            api.publish(&client, "diagnosis");
        });
    }

    fn grant_full_access(&mut self) {
        assert_eq!(self.controller.request_access_mode(AccessMode::Full), None);
        assert_eq!(
            self.controller.confirm_full_access(),
            Some(AccessMode::Full)
        );
    }
}

fn remove_manual_resources(api: &Api, client: &Client, plot_label: &str) -> Vec<(u16, Value)> {
    let state = api.state(client);
    let plot = state["plots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plot| plot["owner"].is_null() && plot["label"] == plot_label)
        .expect("seeded manual plot is visible")["handle"]
        .clone();
    let mut outcomes = Vec::new();
    for trace in manual_handles(&state, "traces") {
        outcomes.push(api.command(client, json!({"op": "trace_remove", "trace": trace})));
    }
    for marker in manual_handles(&state, "markers") {
        outcomes.push(api.command(client, json!({"op": "marker_remove", "marker": marker})));
    }
    for vehicle in manual_handles(&state, "vehicles") {
        outcomes.push(api.command(client, json!({"op": "vehicle_remove", "vehicle": vehicle})));
    }
    outcomes.push(api.command(client, json!({"op": "workspace_close", "plot": plot})));
    outcomes
}

fn assert_all_forbidden(outcomes: &[(u16, Value)]) {
    assert_eq!(outcomes.len(), 4, "{outcomes:?}");
    for (status, body) in outcomes {
        assert_eq!(*status, 403, "{body}");
        assert_eq!(body["code"], "forbidden", "{body}");
    }
}

#[test]
fn safe_mode_protects_manual_state_until_full_access_is_confirmed_and_again_after_reenable() {
    let mut app = HeadlessExternalApp::new();
    app.publish_diagnosis();
    let seed = app.seed_manual();
    let before = app.inventory();

    let label = seed.plot_label.clone();
    let outcomes = app.serve(move |api| {
        let client = api.connect(true);
        remove_manual_resources(&api, &client, &label)
    });
    assert_all_forbidden(&outcomes);
    assert_eq!(app.inventory(), before);

    app.grant_full_access();
    let label = seed.plot_label.clone();
    let outcomes = app.serve(move |api| {
        let client = api.connect(true);
        remove_manual_resources(&api, &client, &label)
    });
    assert_eq!(outcomes.len(), 4, "{outcomes:?}");
    for (status, body) in &outcomes {
        assert_eq!(*status, 200, "{body}");
    }
    let after = app.inventory();
    assert!(after.traces.is_empty(), "{after:?}");
    assert!(after.markers.is_empty(), "{after:?}");
    assert!(after.vehicles.iter().all(|(id, _)| *id != seed.vehicle));
    assert_eq!(after.plots.len(), before.plots.len() - 1);
    assert!(
        after
            .windows
            .iter()
            .any(|(id, owner)| *id == seed.window && owner.is_none())
    );

    app.controller.disable().unwrap();
    app.enable(Duration::from_secs(5));
    assert_eq!(app.controller.access_mode(), Some(AccessMode::Safe));
    let seed = app.seed_manual();
    let before = app.inventory();
    let label = seed.plot_label.clone();
    let outcomes = app.serve(move |api| {
        let client = api.connect(true);
        remove_manual_resources(&api, &client, &label)
    });
    assert_all_forbidden(&outcomes);
    assert_eq!(app.inventory(), before);
    app.controller.disable().unwrap();
}

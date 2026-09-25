use super::*;

#[test]
fn http_requests_drained_on_the_app_thread_create_owned_resources_in_real_state() {
    let mut app = HeadlessExternalApp::new();
    let (publication, state) = app.serve(|api| {
        let client = api.connect(false);
        let publication = api.publish(&client, "diagnosis");
        let error_field = publication["topic"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["name"] == "error")
            .unwrap()["handle"]
            .clone();
        let field = |name: &str| {
            publication["topic"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|field| field["name"] == name)
                .unwrap()["handle"]
                .clone()
        };
        let window = api.resource(&client, json!({"op": "window_open", "title": "diagnosis"}));
        let plot = api.resource(
            &client,
            json!({"op": "workspace_add_plot", "window": window, "direction": "horizontal"}),
        );
        api.resource(
            &client,
            json!({"op": "trace_add", "plot": plot, "field": error_field, "mode": "line"}),
        );
        api.resource(
            &client,
            json!({
                "op": "annotation_add",
                "plot": plot,
                "geometry": {"kind": "text", "at": {"time_ns": 2_000_000, "y": 1.5}},
                "label": "spike",
            }),
        );
        api.resource(
            &client,
            json!({
                "op": "vehicle_add",
                "source": publication["handle"],
                "label": "diagnosis vehicle",
                "show": true,
                "show_path": true,
                "position": {
                    "kind": "gps",
                    "lat": field("lat"),
                    "lon": field("lon"),
                    "alt": field("alt"),
                    "lat_lon_dege7": false,
                    "alt_mm": false,
                    "alt_offset_m": 0.0,
                },
                "orientation": {"kind": "static"},
                "model": "quad",
                "color": "#ff8800",
                "path_color": "#00ff88",
                "scale": 1.0,
            }),
        );
        let state = api.state(&client);
        (publication, state)
    });

    let error = app.published_field("diagnosis", "error");
    let windows = app.windows();
    let window = windows
        .iter()
        .find(|window| window.title == "diagnosis")
        .expect("window opened through HTTP");
    assert_eq!(window.owner.as_ref().unwrap().name, OWNER);
    let plots = app.plots();
    let plot = plots
        .iter()
        .find(|plot| plot.window == window.id)
        .expect("plot added to the HTTP window");
    assert_eq!(plot.owner.as_ref().unwrap().name, OWNER);
    let traces = app.traces();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].field_id, error.id);
    assert_eq!(traces[0].owner.as_ref().unwrap().name, OWNER);
    let annotations = app.annotations();
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].label, "spike");
    assert_eq!(annotations[0].owner.as_deref(), Some(OWNER));
    let vehicles = app.vehicles();
    assert_eq!(vehicles.len(), 1);
    assert_eq!(vehicles[0].spec.source_id, error.source);
    assert_eq!(vehicles[0].spec.owner.as_ref().unwrap().name, OWNER);

    assert_eq!(state["traces"].as_array().unwrap().len(), 1);
    assert_eq!(
        state["traces"][0]["field"],
        "flight-diagnosis/diagnosis/error"
    );
    assert_eq!(state["vehicles"].as_array().unwrap().len(), 1);
    assert!(publication["generation"].as_u64().unwrap() >= 1);
    app.controller.disable().unwrap();
}

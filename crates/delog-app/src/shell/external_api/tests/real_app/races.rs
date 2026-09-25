use super::*;

fn add_marker(api: &Api, client: &Client, key: &str) -> (u16, Value) {
    api.command_with_key(
        client,
        key,
        json!({"op": "marker_add", "time_ns": 5_000_000, "label": "queued"}),
    )
}

fn wait_for_queued(app: &HeadlessExternalApp, queued: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.controller.server_status().unwrap().queued_controls < queued {
        assert!(Instant::now() < deadline, "control never queued");
        thread::sleep(Duration::from_millis(2));
    }
    thread::sleep(Duration::from_millis(50));
}

#[test]
fn an_unclaimed_control_that_times_out_is_never_applied_by_a_later_drain() {
    let mut app = HeadlessExternalApp::with_control_timeout(Duration::from_millis(200));
    let api = app.api();
    let client = api.connect(false);
    let (status, body) = add_marker(&api, &client, &next_key());
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "unavailable", "{body}");
    assert_eq!(body["completion"], "not_started", "{body}");

    app.drain_once();
    assert_eq!(app.applied, 0);
    assert!(app.marker_labels().is_empty());
    app.controller.disable().unwrap();
}

#[test]
fn a_claimed_slow_control_completes_once_and_its_retry_replays_without_reapplying() {
    let mut app = HeadlessExternalApp::with_control_timeout(Duration::from_secs(1));
    let api = app.api();
    let client = api.connect(false);
    let key = next_key();
    let worker = {
        let (api, client, key) = (api.clone(), client.clone(), key.clone());
        thread::spawn(move || add_marker(&api, &client, &key))
    };
    wait_for_queued(&app, 1);
    app.drain_slowly(Duration::from_millis(1_500));
    let (status, first) = worker.join().unwrap();
    assert_eq!(status, 200, "{first}");
    assert_eq!(app.applied, 1);

    let (status, replay) = add_marker(&api, &client, &key);
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay, first);
    app.drain_once();
    assert_eq!(app.applied, 1);
    assert_eq!(app.marker_labels(), ["queued"]);
    app.controller.disable().unwrap();
}

#[test]
fn disabling_with_a_queued_unclaimed_control_never_applies_it() {
    let mut app = HeadlessExternalApp::new();
    let api = app.api();
    let client = api.connect(false);
    let worker = {
        let (api, client) = (api.clone(), client.clone());
        thread::spawn(move || {
            api.http
                .post(api.url("/v1/control"))
                .bearer_auth(&client.token)
                .header("content-type", "application/json")
                .header("idempotency-key", next_key())
                .body(
                    json!({"op": "marker_add", "time_ns": 5_000_000, "label": "queued"})
                        .to_string(),
                )
                .send()
                .map(|response| response.status().as_u16())
        })
    };
    wait_for_queued(&app, 1);
    let started = Instant::now();
    app.controller.disable().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    app.drain_once();
    assert_eq!(app.applied, 0);
    assert!(app.marker_labels().is_empty());
    if let Ok(status) = worker.join().unwrap() {
        assert_ne!(status, 200);
    }
}

#[test]
fn a_takeover_cancels_the_previous_clients_queued_control() {
    let mut app = HeadlessExternalApp::new();
    let api = app.api();
    let first = api.connect(false);
    let worker = {
        let (api, first) = (api.clone(), first.clone());
        thread::spawn(move || add_marker(&api, &first, &next_key()))
    };
    wait_for_queued(&app, 1);
    let second = api.connect(true);

    app.drain_once();
    let (status, body) = worker.join().unwrap();
    assert_ne!(status, 200, "{body}");
    assert_eq!(body["completion"], "not_started", "{body}");
    assert_eq!(app.applied, 0);
    assert!(app.marker_labels().is_empty());

    let (status, body) = app.serve(move |api| add_marker(&api, &second, &next_key()));
    assert_eq!(status, 200, "{body}");
    assert_eq!(app.marker_labels(), ["queued"]);
    app.controller.disable().unwrap();
}

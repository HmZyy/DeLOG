pub mod popup;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

#[derive(serde::Deserialize)]
struct LatestReleaseDoc {
    tag_name: String,
    html_url: String,
}

pub fn latest_release_api_url() -> String {
    let slug = REPOSITORY_URL
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .unwrap_or(REPOSITORY_URL);
    format!("https://api.github.com/repos/{slug}/releases/latest")
}

pub fn parse_release(json: &str) -> Result<Release, String> {
    let doc: LatestReleaseDoc = serde_json::from_str(json).map_err(|err| err.to_string())?;
    let version = normalized_version(&doc.tag_name)
        .ok_or_else(|| format!("release tag {:?} is not a version", doc.tag_name))?;
    if doc.html_url.is_empty() {
        return Err("release payload has no url".to_owned());
    }
    Ok(Release {
        version,
        url: doc.html_url,
    })
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(candidate), Some(current)) = (numeric_parts(candidate), numeric_parts(current))
    else {
        return false;
    };
    for index in 0..candidate.len().max(current.len()) {
        let left = candidate.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    false
}

pub fn should_notify(latest: &Release, current: &str, skipped: Option<&str>) -> bool {
    if !is_newer(&latest.version, current) {
        return false;
    }
    skipped.is_none_or(|skipped| numeric_parts(skipped) != numeric_parts(&latest.version))
}

pub const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

pub fn fetch_latest(client: &reqwest::blocking::Client, url: &str) -> Result<Release, String> {
    let response = client.get(url).send().map_err(|err| err.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err("release payload is too large".to_owned());
    }
    let body = response.text().map_err(|err| err.to_string())?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("release payload is too large".to_owned());
    }
    parse_release(&body)
}

pub fn spawn_check(
    sender: std::sync::mpsc::Sender<Result<Release, String>>,
    repaint: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("delog-update-check".to_owned())
        .spawn(move || {
            let outcome = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent(format!("DeLOG/{CURRENT_VERSION} update check"))
                .build()
                .map_err(|err| err.to_string())
                .and_then(|client| fetch_latest(&client, &latest_release_api_url()));
            if sender.send(outcome).is_ok() {
                repaint();
            }
        })
        .map(|_| ())
        .unwrap_or_default();
}

pub fn apply_action(
    action: popup::UpdateAction,
    release: &Release,
    check_for_updates: &mut bool,
    skipped_version: &mut Option<String>,
) {
    match action {
        popup::UpdateAction::RemindLater => {}
        popup::UpdateAction::SkipVersion => *skipped_version = Some(release.version.clone()),
        popup::UpdateAction::DisableChecks => *check_for_updates = false,
    }
}

fn numeric_parts(version: &str) -> Option<Vec<u64>> {
    let trimmed = version.trim();
    let trimmed = trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed);
    let core = trimmed.split(['-', '+']).next().unwrap_or_default();
    if core.is_empty() {
        return None;
    }
    core.split('.')
        .map(|segment| segment.parse::<u64>().ok())
        .collect()
}

fn normalized_version(tag: &str) -> Option<String> {
    let parts = numeric_parts(tag)?;
    Some(
        parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> Release {
        Release {
            version: version.to_owned(),
            url: format!("{REPOSITORY_URL}/releases/tag/v{version}"),
        }
    }

    #[test]
    fn the_api_url_is_derived_from_the_manifest_repository() {
        assert_eq!(
            latest_release_api_url(),
            "https://api.github.com/repos/HmZyy/DeLOG/releases/latest"
        );
    }

    #[test]
    fn a_higher_version_is_newer() {
        assert!(is_newer("0.3.4", "0.3.3"));
        assert!(is_newer("0.4.0", "0.3.3"));
        assert!(is_newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn the_same_or_an_older_version_is_not_newer() {
        assert!(!is_newer("0.3.3", "0.3.3"));
        assert!(!is_newer("0.3.2", "0.3.3"));
        assert!(!is_newer("0.2.9", "0.3.3"));
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("0.3.10", "0.3.9"));
    }

    #[test]
    fn a_leading_v_and_a_short_form_still_compare() {
        assert!(is_newer("v0.3.4", "0.3.3"));
        assert!(is_newer("0.4", "0.3.3"));
        assert!(!is_newer("v0.3.3", "0.3.3"));
    }

    #[test]
    fn an_unparseable_version_is_never_newer() {
        assert!(!is_newer("nightly", "0.3.3"));
        assert!(!is_newer("", "0.3.3"));
        assert!(!is_newer("0.3.4", "not-a-version"));
    }

    #[test]
    fn a_release_is_parsed_from_the_github_payload() {
        let json = r#"{
            "tag_name": "v0.3.4",
            "html_url": "https://github.com/HmZyy/DeLOG/releases/tag/v0.3.4",
            "name": "DeLOG 0.3.4"
        }"#;

        let parsed = parse_release(json).expect("a well-formed payload should parse");

        assert_eq!(parsed.version, "0.3.4");
        assert_eq!(
            parsed.url,
            "https://github.com/HmZyy/DeLOG/releases/tag/v0.3.4"
        );
    }

    #[test]
    fn a_payload_without_a_usable_tag_or_url_is_rejected() {
        assert!(parse_release("{}").is_err());
        assert!(parse_release("not json").is_err());
        assert!(
            parse_release(r#"{"tag_name": "nightly", "html_url": "https://example.com"}"#).is_err(),
            "a tag that is not a version should not be offered as an update"
        );
    }

    #[test]
    fn a_newer_release_is_worth_notifying_about() {
        assert!(should_notify(&release("0.3.4"), "0.3.3", None));
    }

    #[test]
    fn the_current_or_an_older_release_is_not_worth_notifying_about() {
        assert!(!should_notify(&release("0.3.3"), "0.3.3", None));
        assert!(!should_notify(&release("0.3.2"), "0.3.3", None));
    }

    #[test]
    fn a_skipped_version_is_not_notified_again() {
        assert!(!should_notify(&release("0.3.4"), "0.3.3", Some("0.3.4")));
        assert!(
            !should_notify(&release("0.3.4"), "0.3.3", Some("v0.3.4")),
            "the skipped version should match regardless of a leading v"
        );
    }

    #[test]
    fn skipping_one_version_still_notifies_about_the_next() {
        assert!(should_notify(&release("0.3.5"), "0.3.3", Some("0.3.4")));
    }

    fn serve_once(status: u16, body: &'static str) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("a local server should bind");
        let address = format!("http://{}", server.server_addr());
        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let _ = request
                    .respond(tiny_http::Response::from_string(body).with_status_code(status));
            }
        });
        address
    }

    #[test]
    fn fetching_reads_the_release_from_the_response() {
        let client = reqwest::blocking::Client::new();
        let url = serve_once(
            200,
            r#"{"tag_name":"v1.2.3","html_url":"https://example.invalid/releases/tag/v1.2.3"}"#,
        );

        let release = fetch_latest(&client, &url).expect("a well-formed response should parse");

        assert_eq!(release.version, "1.2.3");
        assert_eq!(release.url, "https://example.invalid/releases/tag/v1.2.3");
    }

    #[test]
    fn a_rate_limited_or_failing_response_is_an_error() {
        let client = reqwest::blocking::Client::new();

        let forbidden = fetch_latest(&client, &serve_once(403, "rate limited"))
            .expect_err("a 403 should not be treated as a release");
        assert!(
            forbidden.contains("403"),
            "the error should name the status, got {forbidden:?}"
        );
        assert!(fetch_latest(&client, &serve_once(200, "not json")).is_err());
    }

    #[test]
    fn an_unreachable_host_is_an_error_not_a_panic() {
        let client = reqwest::blocking::Client::new();
        assert!(fetch_latest(&client, "http://127.0.0.1:1/nothing").is_err());
    }

    #[test]
    fn skipping_a_version_records_only_that_version() {
        let mut check = true;
        let mut skipped = None;

        apply_action(
            popup::UpdateAction::SkipVersion,
            &release("0.4.0"),
            &mut check,
            &mut skipped,
        );

        assert_eq!(skipped.as_deref(), Some("0.4.0"));
        assert!(
            check,
            "skipping one version must not stop future update checks"
        );
    }

    #[test]
    fn stopping_the_checks_unticks_the_setting() {
        let mut check = true;
        let mut skipped = None;

        apply_action(
            popup::UpdateAction::DisableChecks,
            &release("0.4.0"),
            &mut check,
            &mut skipped,
        );

        assert!(!check, "stopping the checks must turn the setting off");
        assert_eq!(
            skipped, None,
            "stopping the checks is not the same as skipping a version"
        );
    }

    #[test]
    fn asking_to_be_reminded_later_changes_nothing() {
        let mut check = true;
        let mut skipped = Some("0.3.9".to_owned());

        apply_action(
            popup::UpdateAction::RemindLater,
            &release("0.4.0"),
            &mut check,
            &mut skipped,
        );

        assert!(check);
        assert_eq!(skipped.as_deref(), Some("0.3.9"));
    }
}

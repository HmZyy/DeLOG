use delog_remote::{CatalogDto, ControlCommandDto, ErrorEnvelope, InstanceDto};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const INSTANCE: &str = include_str!("golden/instance-v1.json");
const CATALOG: &str = include_str!("golden/catalog-v1.json");
const ERROR: &str = include_str!("golden/error-v1.json");
const CONTROL: &str = include_str!("golden/control-v1.json");

fn round_trip<T>(document: &str)
where
    T: DeserializeOwned + Serialize,
{
    let expected: Value = serde_json::from_str(document).unwrap();
    let decoded: T = serde_json::from_str(document).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
}

#[test]
fn checked_in_v1_documents_round_trip_exactly() {
    round_trip::<InstanceDto>(INSTANCE);
    round_trip::<CatalogDto>(CATALOG);
    round_trip::<ErrorEnvelope>(ERROR);
    round_trip::<ControlCommandDto>(CONTROL);
}

#[test]
fn v1_requests_reject_additive_fields_but_responses_tolerate_them() {
    let mut request: Value = serde_json::from_str(CONTROL).unwrap();
    request["future_request_field"] = json!(true);
    assert!(serde_json::from_value::<ControlCommandDto>(request).is_err());

    let mut response: Value = serde_json::from_str(INSTANCE).unwrap();
    response["future_response_field"] = json!({"enabled": true});
    let decoded: InstanceDto = serde_json::from_value(response).unwrap();
    assert_eq!(decoded.api_major, 1);
}

#[test]
fn advertised_version_range_selects_supported_minor_and_rejects_major_mismatch() {
    fn compatible(server: &InstanceDto, client_major: u16, client_minor: u16) -> bool {
        server.api_major == client_major
            && server.api_min_minor <= client_minor
            && client_minor <= server.api_max_minor
    }

    let server: InstanceDto = serde_json::from_str(INSTANCE).unwrap();
    assert!(compatible(&server, 1, 0));
    assert!(!compatible(&server, 2, 0));
    assert!(!compatible(&server, 1, 1));
}

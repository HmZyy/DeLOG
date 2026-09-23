#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_core::identity::{FieldId, IdentityRegistry, SourceId};
use delog_core::snapshot::StoreSnapshot;
use delog_script::{
    ControlHost, ControlRequest, ControlResponse, ProfileFieldRef, ProfileNedReference,
    ProfileOrientation, ProfilePosition, ResolvedVehicleField, ScriptOwner, VehicleInfo,
    VehicleModel, VehicleOrientation, VehiclePosition, VehicleProfileInfo, VehicleProfileRequest,
    VehicleSpec,
};

#[derive(Default)]
struct ProfileHost {
    seen: Mutex<Vec<ControlRequest>>,
}

impl ProfileHost {
    fn taken(&self) -> Vec<ControlRequest> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    fn request_kinds(&self) -> Vec<&'static str> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|request| match request {
                ControlRequest::VehicleProfiles(VehicleProfileRequest::List) => Some("list"),
                ControlRequest::VehicleProfiles(VehicleProfileRequest::Load { .. }) => Some("load"),
                ControlRequest::VehicleProfiles(VehicleProfileRequest::Apply { .. }) => {
                    Some("apply")
                }
                ControlRequest::VehicleProfiles(VehicleProfileRequest::Save { .. }) => Some("save"),
                ControlRequest::VehicleProfiles(VehicleProfileRequest::Delete { .. }) => {
                    Some("delete")
                }
                _ => None,
            })
            .collect()
    }
}

impl ControlHost for ProfileHost {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::VehicleProfiles(VehicleProfileRequest::List) => {
                Ok(ControlResponse::Names(vec!["quad-gps".to_owned()]))
            }
            ControlRequest::VehicleProfiles(VehicleProfileRequest::Load { name }) => {
                Ok(ControlResponse::VehicleProfile(profile_fixture(name)))
            }
            ControlRequest::VehicleProfiles(VehicleProfileRequest::Apply {
                source_id,
                source,
                owner,
                ..
            }) => Ok(ControlResponse::Vehicles(vec![vehicle_fixture(
                source_id, source, owner,
            )])),
            ControlRequest::VehicleProfiles(VehicleProfileRequest::Save { .. })
            | ControlRequest::VehicleProfiles(VehicleProfileRequest::Delete { .. }) => {
                Ok(ControlResponse::Unit)
            }
            other => Err(format!("unexpected request: {other:?}")),
        }
    }
}

fn field(topic: &str, field: &str) -> ProfileFieldRef {
    ProfileFieldRef {
        topic: topic.to_owned(),
        field: field.to_owned(),
    }
}

fn profile_fixture(name: String) -> VehicleProfileInfo {
    let mut profile = VehicleProfileInfo {
        name: name.clone(),
        label: "UAV".to_owned(),
        show: true,
        show_path: false,
        position: ProfilePosition::Gps {
            lat: field("GPS", "lat"),
            lon: field("GPS", "lon"),
            alt: field("GPS", "alt"),
            lat_lon_dege7: true,
            alt_mm: true,
            alt_offset_m: 2.5,
        },
        orientation: ProfileOrientation::Static,
        model: VehicleModel::Quad,
        color: [0.25, 0.5, 0.75, 1.0],
        path_color: [1.0, 0.5, 0.0, 0.5],
        scale: 1.5,
    };
    match name.as_str() {
        "ned-euler" => {
            profile.position = ProfilePosition::Ned {
                north: field("LOCAL", "north"),
                east: field("LOCAL", "east"),
                down: field("LOCAL", "down"),
                reference: Some(ProfileNedReference::Manual {
                    lat_deg: 48.5,
                    lon_deg: 2.2,
                    alt_m: 125.0,
                }),
            };
            profile.orientation = ProfileOrientation::Euler {
                roll: field("ATT", "roll"),
                pitch: field("ATT", "pitch"),
                yaw: field("ATT", "yaw"),
                degrees: true,
            };
        }
        "ned-fields-quat" => {
            profile.position = ProfilePosition::Ned {
                north: field("LOCAL", "north"),
                east: field("LOCAL", "east"),
                down: field("LOCAL", "down"),
                reference: Some(ProfileNedReference::Fields {
                    lat: field("HOME", "lat"),
                    lon: field("HOME", "lon"),
                    alt: field("HOME", "alt"),
                }),
            };
            profile.orientation = ProfileOrientation::Quat {
                w: field("ATT", "w"),
                x: field("ATT", "x"),
                y: field("ATT", "y"),
                z: field("ATT", "z"),
            };
        }
        _ => {}
    }
    profile
}

fn resolved(id: u32, path: &str) -> ResolvedVehicleField {
    ResolvedVehicleField {
        id: FieldId(id),
        path: path.to_owned(),
    }
}

fn vehicle_fixture(source_id: SourceId, source: String, owner: Option<ScriptOwner>) -> VehicleInfo {
    VehicleInfo {
        id: 42,
        index: 0,
        spec: VehicleSpec {
            source_id,
            source,
            label: "UAV".to_owned(),
            show: true,
            show_path: false,
            position: VehiclePosition::Gps {
                lat: resolved(0, "flight/GPS/lat"),
                lon: resolved(1, "flight/GPS/lon"),
                alt: resolved(2, "flight/GPS/alt"),
                lat_lon_dege7: true,
                alt_mm: true,
                alt_offset_m: 2.5,
            },
            orientation: VehicleOrientation::Static,
            model: VehicleModel::Quad,
            color: [0.25, 0.5, 0.75, 1.0],
            path_color: [1.0, 0.5, 0.0, 0.5],
            scale: 1.5,
            owner,
        },
    }
}

fn vehicle_snapshot() -> Arc<StoreSnapshot> {
    let mut ids = IdentityRegistry::new();
    let source = ids.add_source("flight");
    let topic = ids.add_topic(source, "GPS").unwrap();
    for name in ["lat", "lon", "alt"] {
        ids.add_field(topic, name).unwrap();
    }
    Arc::new(StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"))
}

#[test]
fn profile_library_surface_round_trips() {
    let host = Arc::new(ProfileHost::default());
    delog_script::control::testing::eval_with_host_and_snapshot(
        host.clone(),
        vehicle_snapshot(),
        "names = delog.vehicle_profiles.list()\n\
         assert names == ['quad-gps']\n\
         p = delog.vehicle_profiles.load('quad-gps')\n\
         assert p.name == 'quad-gps'\n\
         assert p.label == 'UAV'\n\
         assert p.show is True\n\
         assert p.show_path is False\n\
         assert p.position.kind == 'gps'\n\
         assert p.position.lat.topic == 'GPS'\n\
         assert p.position.lat.field == 'lat'\n\
         assert p.position.lon.field == 'lon'\n\
         assert p.position.alt.field == 'alt'\n\
         assert p.position.dege7 is True\n\
         assert p.position.alt_mm is True\n\
         assert p.position.alt_offset_m == 2.5\n\
         assert p.orientation.kind == 'static'\n\
         assert p.model == 'quad'\n\
         assert p.color == '#4080BFFF'\n\
         assert p.path_color == '#FF800080'\n\
         assert p.scale == 1.5\n\
         v = delog.vehicle_profiles.apply('quad-gps', source='flight')\n\
         assert v.id == 42\n\
         delog.vehicle_profiles.save('copy', v)\n\
         delog.vehicle_profiles.delete('copy')",
    )
    .unwrap();

    assert_eq!(
        host.request_kinds(),
        ["list", "load", "apply", "save", "delete"]
    );
    let requests = host.taken();
    assert!(matches!(
        &requests[2],
        ControlRequest::VehicleProfiles(VehicleProfileRequest::Apply {
            name,
            source,
            owner: None,
            ..
        }) if name == "quad-gps" && source == "flight"
    ));
    assert!(matches!(
        &requests[3],
        ControlRequest::VehicleProfiles(VehicleProfileRequest::Save { name, vehicle_id })
            if name == "copy" && *vehicle_id == 42
    ));
}

#[test]
fn invalid_profile_names_fail_before_a_round_trip() {
    for name in ["", "../x", "a/b", "a\\b", "   "] {
        let host = Arc::new(ProfileHost::default());
        let statement = format!("delog.vehicle_profiles.load({name:?})");
        let error = delog_script::control::testing::eval_with_host_and_snapshot(
            host.clone(),
            vehicle_snapshot(),
            &statement,
        )
        .unwrap_err();
        assert!(error.contains("ValueError"), "{error}");
        assert!(host.taken().is_empty());
    }
}

#[test]
fn loaded_profiles_preserve_ned_reference_and_orientation_payloads() {
    let host = Arc::new(ProfileHost::default());
    delog_script::control::testing::eval_with_host_and_snapshot(
        host,
        vehicle_snapshot(),
        "manual = delog.vehicle_profiles.load('ned-euler')\n\
         assert manual.position.kind == 'ned'\n\
         assert manual.position.north.topic == 'LOCAL'\n\
         assert manual.position.east.field == 'east'\n\
         assert manual.position.down.field == 'down'\n\
         assert manual.position.reference.kind == 'manual'\n\
         assert manual.position.reference.lat_deg == 48.5\n\
         assert manual.position.reference.lon_deg == 2.2\n\
         assert manual.position.reference.alt_m == 125.0\n\
         assert manual.orientation.kind == 'euler'\n\
         assert manual.orientation.roll.field == 'roll'\n\
         assert manual.orientation.pitch.field == 'pitch'\n\
         assert manual.orientation.yaw.field == 'yaw'\n\
         assert manual.orientation.degrees is True\n\
         fields = delog.vehicle_profiles.load('ned-fields-quat')\n\
         assert fields.position.reference.kind == 'fields'\n\
         assert fields.position.reference.lat.topic == 'HOME'\n\
         assert fields.position.reference.lon.field == 'lon'\n\
         assert fields.position.reference.alt.field == 'alt'\n\
         assert fields.orientation.kind == 'quat'\n\
         assert fields.orientation.w.field == 'w'\n\
         assert fields.orientation.x.field == 'x'\n\
         assert fields.orientation.y.field == 'y'\n\
         assert fields.orientation.z.field == 'z'",
    )
    .unwrap();
}

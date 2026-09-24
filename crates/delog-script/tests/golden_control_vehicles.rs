#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_api::control::{
    ControlHost, ControlRequest, ControlResponse, VehicleFilter, VehicleInfo, VehicleModel,
    VehicleNedReference, VehicleOrientation, VehiclePatch, VehiclePosition, VehicleRequest,
};
use delog_core::identity::IdentityRegistry;
use delog_core::snapshot::StoreSnapshot;

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<ControlRequest>>,
    vehicles: Mutex<Vec<VehicleInfo>>,
    next_id: Mutex<u64>,
}

impl Recorder {
    fn taken(&self) -> Vec<ControlRequest> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    fn vehicle(&self) -> VehicleInfo {
        self.vehicles.lock().unwrap()[0].clone()
    }
}

impl ControlHost for Recorder {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::Vehicles(VehicleRequest::List) => Ok(ControlResponse::Vehicles(
                self.vehicles.lock().unwrap().clone(),
            )),
            ControlRequest::Vehicles(VehicleRequest::Add(spec)) => {
                let mut next_id = self.next_id.lock().unwrap();
                *next_id += 1;
                let mut vehicles = self.vehicles.lock().unwrap();
                let info = VehicleInfo {
                    id: *next_id,
                    index: vehicles.len(),
                    spec,
                };
                vehicles.push(info.clone());
                Ok(ControlResponse::Vehicles(vec![info]))
            }
            ControlRequest::Vehicles(VehicleRequest::Set { id, patch }) => {
                let mut vehicles = self.vehicles.lock().unwrap();
                let info = vehicles
                    .iter_mut()
                    .find(|info| info.id == id)
                    .ok_or_else(|| delog_api::Error::execution(format!("vehicle {id} is gone")))?;
                apply_patch(info, patch);
                Ok(ControlResponse::Vehicles(vec![info.clone()]))
            }
            ControlRequest::Vehicles(VehicleRequest::Remove(filter)) => {
                let mut vehicles = self.vehicles.lock().unwrap();
                match filter {
                    VehicleFilter::Id(id) => vehicles.retain(|info| info.id != id),
                    VehicleFilter::Index(index) => {
                        if index >= vehicles.len() {
                            return Err(delog_api::Error::execution(format!(
                                "vehicle index {index} is gone"
                            )));
                        }
                        vehicles.remove(index);
                    }
                    VehicleFilter::Label(label) => vehicles.retain(|info| info.spec.label != label),
                    VehicleFilter::Source(source) => {
                        vehicles.retain(|info| info.spec.source_id != source)
                    }
                    VehicleFilter::All => vehicles.clear(),
                }
                for (index, info) in vehicles.iter_mut().enumerate() {
                    info.index = index;
                }
                Ok(ControlResponse::Unit)
            }
            _ => Ok(ControlResponse::Unit),
        }
    }
}

fn apply_patch(info: &mut VehicleInfo, patch: VehiclePatch) {
    if let Some(label) = patch.label {
        info.spec.label = label;
    }
    if let Some(show) = patch.show {
        info.spec.show = show;
    }
    if let Some(show_path) = patch.show_path {
        info.spec.show_path = show_path;
    }
    if let Some(position) = patch.position {
        info.spec.position = position;
    }
    if let Some(orientation) = patch.orientation {
        info.spec.orientation = orientation;
    }
    if let Some(model) = patch.model {
        info.spec.model = model;
    }
    if let Some(color) = patch.color {
        info.spec.color = color;
    }
    if let Some(path_color) = patch.path_color {
        info.spec.path_color = path_color;
    }
    if let Some(scale) = patch.scale {
        info.spec.scale = scale;
    }
}

fn add_vehicle_fields(ids: &mut IdentityRegistry, source: delog_core::identity::SourceId) {
    for (topic_name, fields) in [
        ("GPS", &["lat", "lon", "alt"][..]),
        ("LOCAL", &["north", "east", "down"]),
        ("ATT", &["roll", "pitch", "yaw", "w", "x", "y", "z"]),
    ] {
        let topic = ids.add_topic(source, topic_name).unwrap();
        for field in fields {
            ids.add_field(topic, *field).unwrap();
        }
    }
}

fn vehicle_snapshot() -> Arc<StoreSnapshot> {
    let mut ids = IdentityRegistry::new();
    let source = ids.add_source("flight");
    add_vehicle_fields(&mut ids, source);
    Arc::new(StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"))
}

fn two_source_vehicle_snapshot() -> Arc<StoreSnapshot> {
    let mut ids = IdentityRegistry::new();
    let source_a = ids.add_source("flight_a");
    add_vehicle_fields(&mut ids, source_a);
    let source_b = ids.add_source("flight_b");
    add_vehicle_fields(&mut ids, source_b);
    Arc::new(StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"))
}

#[test]
fn mapping_constructors_resolve_fields_and_preserve_options() {
    let (position, orientation) =
        delog_script::control::testing::eval_vehicle_mappings_with_snapshot(
            vehicle_snapshot(),
            "pos = delog.gps('GPS.lat', 'GPS.lon', 'GPS.alt', dege7=True, alt_mm=True, alt_offset_m=2.5)\n\
             ori = delog.euler('ATT.roll', 'ATT.pitch', 'ATT.yaw', degrees=True)",
        )
        .unwrap();
    assert!(matches!(
        position,
        VehiclePosition::Gps {
            lat_lon_dege7: true,
            alt_mm: true,
            alt_offset_m: 2.5,
            ref lat,
            ref lon,
            ref alt,
        } if lat.path == "flight/GPS/lat"
            && lon.path == "flight/GPS/lon"
            && alt.path == "flight/GPS/alt"
    ));
    assert!(matches!(
        orientation,
        VehicleOrientation::Euler { degrees: true, ref roll, ref pitch, ref yaw }
            if roll.path == "flight/ATT/roll"
                && pitch.path == "flight/ATT/pitch"
                && yaw.path == "flight/ATT/yaw"
    ));
}

#[test]
fn ned_quaternion_geo_and_field_refs_preserve_their_variants() {
    let (position, orientation) =
        delog_script::control::testing::eval_vehicle_mappings_with_snapshot(
            vehicle_snapshot(),
            "pos = delog.ned(delog.find('LOCAL', 'north'), 'LOCAL.east', 'LOCAL.down', reference=delog.geo(48.5, 2.2, 125.0))\n\
             ori = delog.quat('ATT.w', 'ATT.x', 'ATT.y', 'ATT.z')",
        )
        .unwrap();
    assert!(matches!(
        position,
        VehiclePosition::Ned {
            reference: Some(VehicleNedReference::Manual {
                lat_deg: 48.5,
                lon_deg: 2.2,
                alt_m: 125.0,
            }),
            ref north,
            ..
        } if north.path == "flight/LOCAL/north"
    ));
    assert!(matches!(orientation, VehicleOrientation::Quat { .. }));

    let (position, orientation) =
        delog_script::control::testing::eval_vehicle_mappings_with_snapshot(
            vehicle_snapshot(),
            "pos = delog.ned('LOCAL.north', 'LOCAL.east', 'LOCAL.down', reference=delog.geo_fields('GPS.lat', 'GPS.lon', 'GPS.alt'))\n\
             ori = delog.static_ori()",
        )
        .unwrap();
    assert!(matches!(
        position,
        VehiclePosition::Ned {
            reference: Some(VehicleNedReference::Fields { .. }),
            ..
        }
    ));
    assert_eq!(orientation, VehicleOrientation::Static);
}

#[test]
fn ambiguous_mapping_fields_fail_before_a_round_trip() {
    let host = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host_and_snapshot(
        host.clone(),
        two_source_vehicle_snapshot(),
        "delog.gps('GPS.lat', 'GPS.lon', 'GPS.alt')",
    )
    .unwrap_err();
    assert!(
        error.contains("ambiguous") && error.contains("flight_a/GPS/lat"),
        "{error}"
    );
    assert!(host.taken().is_empty());
}

#[test]
fn invalid_numeric_mapping_arguments_fail_before_a_round_trip() {
    for statement in [
        "delog.geo(float('nan'), 2.0, 3.0)",
        "delog.geo(91.0, 2.0, 3.0)",
        "delog.geo(45.0, 181.0, 3.0)",
        "delog.gps('GPS.lat', 'GPS.lon', 'GPS.alt', alt_offset_m=float('inf'))",
    ] {
        let host = Arc::new(Recorder::default());
        let error = delog_script::control::testing::eval_with_host_and_snapshot(
            host.clone(),
            vehicle_snapshot(),
            statement,
        )
        .unwrap_err();
        assert!(error.contains("ValueError"), "{error}");
        assert!(host.taken().is_empty());
    }
}

#[test]
fn vehicle_collection_and_handle_surface_round_trip() {
    let host = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host_and_snapshot(
        host.clone(),
        vehicle_snapshot(),
        "v = delog.vehicles.add(source='flight', label='UAV',\n\
             pos=delog.gps('GPS.lat', 'GPS.lon', 'GPS.alt'), model='quad')\n\
         assert len(delog.vehicles) == 1\n\
         assert delog.vehicles[0].id == v.id\n\
         assert v.source == 'flight'\n\
         assert v.owner is None\n\
         v.label = 'UAV 1'\n\
         v.show = False\n\
         v.show_path = False\n\
         v.scale = 2.0\n\
         v.model = 'fixedwing'\n\
         v.color = '#4C9AFF'\n\
         v.path_color = '#E74C3C'\n\
         v.pos = delog.ned('LOCAL.north', 'LOCAL.east', 'LOCAL.down')\n\
         v.ori = delog.static_ori()\n\
         assert v.label == 'UAV 1'\n\
         assert v.show is False\n\
         assert v.show_path is False\n\
         assert v.scale == 2.0\n\
         assert v.model == 'fixedwing'\n\
         assert v.color == '#4C9AFFFF'\n\
         assert v.path_color == '#E74C3CFF'",
    )
    .unwrap();
    let vehicle = host.vehicle();
    assert_eq!(vehicle.spec.label, "UAV 1");
    assert!(!vehicle.spec.show);
    assert!(!vehicle.spec.show_path);
    assert_eq!(vehicle.spec.scale, 2.0);
    assert_eq!(vehicle.spec.model, VehicleModel::FixedWing);
    assert!(matches!(vehicle.spec.position, VehiclePosition::Ned { .. }));
    assert_eq!(vehicle.spec.orientation, VehicleOrientation::Static);
}

#[test]
fn every_vehicle_removal_axis_is_exclusive() {
    let host = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host_and_snapshot(
        host.clone(),
        vehicle_snapshot(),
        "v0 = delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'))\n\
         delog.vehicles.remove(0)\n\
         v1 = delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'))\n\
         delog.vehicles.remove(v1)\n\
         delog.vehicles.remove(label='UAV')\n\
         delog.vehicles.remove(source='flight')\n\
         delog.vehicles.clear()",
    )
    .unwrap();
    let filters: Vec<VehicleFilter> = host
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| match request {
            ControlRequest::Vehicles(VehicleRequest::Remove(filter)) => Some(filter.clone()),
            _ => None,
        })
        .collect();
    assert!(matches!(
        filters.as_slice(),
        [
            VehicleFilter::Index(0),
            VehicleFilter::Id(_),
            VehicleFilter::Label(label),
            VehicleFilter::Source(_),
            VehicleFilter::All,
        ] if label == "UAV"
    ));

    let host = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host_and_snapshot(
        host.clone(),
        vehicle_snapshot(),
        "delog.vehicles.remove(0, label='UAV')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(
        !host
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, ControlRequest::Vehicles(VehicleRequest::Remove(_))))
    );
}

#[test]
fn bad_model_color_and_scale_are_rejected_before_the_host() {
    for statement in [
        "delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'), model='jet')",
        "delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'), color='blue')",
        "delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'), path_color='red')",
        "delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'), scale=0.0)",
        "delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'), scale=float('nan'))",
        "delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'), scale=float('inf'))",
    ] {
        let host = Arc::new(Recorder::default());
        let error = delog_script::control::testing::eval_with_host_and_snapshot(
            host.clone(),
            vehicle_snapshot(),
            statement,
        )
        .unwrap_err();
        assert!(error.contains("ValueError"), "{error}");
        assert!(
            !host
                .seen
                .lock()
                .unwrap()
                .iter()
                .any(|request| matches!(request, ControlRequest::Vehicles(VehicleRequest::Add(_))))
        );
    }
}

#[test]
fn invalid_handle_updates_are_rejected_before_set_reaches_the_host() {
    for setter in [
        "v.scale = float('nan')",
        "v.color = 'blue'",
        "v.model = 'jet'",
    ] {
        let host = Arc::new(Recorder::default());
        let statement = format!(
            "v = delog.vehicles.add(source='flight', pos=delog.gps('GPS.lat','GPS.lon','GPS.alt'))\n{setter}"
        );
        let error = delog_script::control::testing::eval_with_host_and_snapshot(
            host.clone(),
            vehicle_snapshot(),
            &statement,
        )
        .unwrap_err();
        assert!(error.contains("ValueError"), "{error}");
        assert!(!host.seen.lock().unwrap().iter().any(|request| matches!(
            request,
            ControlRequest::Vehicles(VehicleRequest::Set { .. })
        )));
    }
}

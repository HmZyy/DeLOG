use delog_api::control::{
    ResolvedVehicleField, VehicleModel as ScriptVehicleModel,
    VehicleNedReference as ScriptNedReference, VehicleOrientation as ScriptVehicleOrientation,
    VehiclePosition,
};
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;

pub(super) fn validate_source(
    snapshot: &StoreSnapshot,
    id: SourceId,
    path: &str,
) -> Result<(), String> {
    let source = snapshot
        .source(id)
        .filter(|source| !source.entry.removed && source.entry.id == id)
        .ok_or_else(|| format!("source '{path}' is gone"))?;
    if source.entry.label != path {
        return Err(format!(
            "source '{path}' no longer matches source {} ('{}')",
            id.0, source.entry.label
        ));
    }
    Ok(())
}

fn validate_field(
    snapshot: &StoreSnapshot,
    source: SourceId,
    field: &ResolvedVehicleField,
) -> Result<FieldId, String> {
    let entry = snapshot
        .fields
        .get(field.id.index())
        .filter(|entry| !entry.removed && entry.id == field.id)
        .ok_or_else(|| format!("field '{}' is gone", field.path))?;
    let topic = snapshot
        .topic(entry.topic)
        .filter(|topic| !topic.entry.removed)
        .ok_or_else(|| format!("field '{}' is gone", field.path))?;
    if topic.entry.source != source {
        return Err(format!(
            "field '{}' belongs to source {}, not vehicle source {}",
            field.path, topic.entry.source.0, source.0
        ));
    }
    Ok(field.id)
}

pub(super) fn resolved_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
) -> Result<ResolvedVehicleField, String> {
    let entry = snapshot
        .fields
        .get(field.index())
        .filter(|entry| !entry.removed && entry.id == field)
        .ok_or_else(|| format!("field {} is gone", field.0))?;
    let topic = snapshot
        .topic(entry.topic)
        .filter(|topic| !topic.entry.removed)
        .ok_or_else(|| format!("field {} is gone", field.0))?;
    let source = snapshot
        .source(topic.entry.source)
        .filter(|source| !source.entry.removed)
        .ok_or_else(|| format!("field {} source is gone", field.0))?;
    Ok(ResolvedVehicleField {
        id: field,
        path: format!("{}/{}/{}", source.entry.label, topic.entry.name, entry.name),
    })
}

pub(super) fn app_position(
    snapshot: &StoreSnapshot,
    source: SourceId,
    position: VehiclePosition,
) -> Result<crate::scene3d::vehicle::PosMapping, String> {
    use crate::scene3d::vehicle::{GeoRef, NedReference, PosMapping};

    match position {
        VehiclePosition::Ned {
            north,
            east,
            down,
            reference,
        } => {
            let reference = reference
                .map(|reference| -> Result<NedReference, String> {
                    match reference {
                        ScriptNedReference::Manual {
                            lat_deg,
                            lon_deg,
                            alt_m,
                        } => {
                            if !lat_deg.is_finite()
                                || !lon_deg.is_finite()
                                || !alt_m.is_finite()
                                || !(-90.0..=90.0).contains(&lat_deg)
                                || !(-180.0..=180.0).contains(&lon_deg)
                            {
                                return Err("vehicle NED georeference is invalid".to_string());
                            }
                            Ok(NedReference::Manual(GeoRef {
                                lat_deg,
                                lon_deg,
                                alt_m,
                            }))
                        }
                        ScriptNedReference::Fields { lat, lon, alt } => Ok(NedReference::Fields {
                            lat: validate_field(snapshot, source, &lat)?,
                            lon: validate_field(snapshot, source, &lon)?,
                            alt: validate_field(snapshot, source, &alt)?,
                        }),
                    }
                })
                .transpose()?;
            Ok(PosMapping::Ned {
                north: validate_field(snapshot, source, &north)?,
                east: validate_field(snapshot, source, &east)?,
                down: validate_field(snapshot, source, &down)?,
                reference,
            })
        }
        VehiclePosition::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => {
            if !alt_offset_m.is_finite() {
                return Err("vehicle GPS alt_offset_m must be finite".into());
            }
            Ok(PosMapping::Gps {
                lat: validate_field(snapshot, source, &lat)?,
                lon: validate_field(snapshot, source, &lon)?,
                alt: validate_field(snapshot, source, &alt)?,
                lat_lon_dege7,
                alt_mm,
                alt_offset_m,
            })
        }
    }
}

pub(super) fn app_orientation(
    snapshot: &StoreSnapshot,
    source: SourceId,
    orientation: ScriptVehicleOrientation,
) -> Result<crate::scene3d::vehicle::OriMapping, String> {
    use crate::scene3d::vehicle::OriMapping;

    match orientation {
        ScriptVehicleOrientation::Static => Ok(OriMapping::Static),
        ScriptVehicleOrientation::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => Ok(OriMapping::Euler {
            roll: validate_field(snapshot, source, &roll)?,
            pitch: validate_field(snapshot, source, &pitch)?,
            yaw: validate_field(snapshot, source, &yaw)?,
            degrees,
        }),
        ScriptVehicleOrientation::Quat { w, x, y, z } => Ok(OriMapping::Quat {
            w: validate_field(snapshot, source, &w)?,
            x: validate_field(snapshot, source, &x)?,
            y: validate_field(snapshot, source, &y)?,
            z: validate_field(snapshot, source, &z)?,
        }),
    }
}

pub(super) fn app_model(model: ScriptVehicleModel) -> crate::scene3d::vehicle::ModelKind {
    use crate::scene3d::vehicle::ModelKind;

    match model {
        ScriptVehicleModel::None => ModelKind::None,
        ScriptVehicleModel::Quad => ModelKind::Quad,
        ScriptVehicleModel::FixedWing => ModelKind::FixedWing,
        ScriptVehicleModel::DeltaWing => ModelKind::DeltaWing,
        ScriptVehicleModel::Cone => ModelKind::Cone,
        ScriptVehicleModel::Sphere => ModelKind::Sphere,
        ScriptVehicleModel::Cube => ModelKind::Cube,
        ScriptVehicleModel::CustomGlb(path) => ModelKind::CustomGlb(path.into()),
    }
}

pub(super) fn script_model(model: &crate::scene3d::vehicle::ModelKind) -> ScriptVehicleModel {
    use crate::scene3d::vehicle::ModelKind;

    match model {
        ModelKind::None => ScriptVehicleModel::None,
        ModelKind::Quad => ScriptVehicleModel::Quad,
        ModelKind::FixedWing => ScriptVehicleModel::FixedWing,
        ModelKind::DeltaWing => ScriptVehicleModel::DeltaWing,
        ModelKind::Cone => ScriptVehicleModel::Cone,
        ModelKind::Sphere => ScriptVehicleModel::Sphere,
        ModelKind::Cube => ScriptVehicleModel::Cube,
        ModelKind::CustomGlb(path) => {
            ScriptVehicleModel::CustomGlb(path.to_string_lossy().into_owned())
        }
    }
}

pub(super) fn app_color(color: [f32; 4], name: &str) -> Result<egui::Color32, String> {
    if color
        .iter()
        .any(|component| !component.is_finite() || !(0.0..=1.0).contains(component))
    {
        return Err(format!(
            "{name} components must be finite and between 0 and 1"
        ));
    }
    Ok(egui::Color32::from_rgba_unmultiplied(
        (color[0] * 255.0).round() as u8,
        (color[1] * 255.0).round() as u8,
        (color[2] * 255.0).round() as u8,
        (color[3] * 255.0).round() as u8,
    ))
}

pub(super) fn color_to_script(color: egui::Color32) -> [f32; 4] {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

pub(super) fn validate_scale(scale: f32) -> Result<(), String> {
    if scale.is_finite() && scale > 0.0 {
        Ok(())
    } else {
        Err("vehicle scale must be finite and > 0".into())
    }
}

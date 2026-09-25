use delog_api::color::{format_hex_color, parse_hex_color};
use delog_api::control::{
    AnnotationGeometry, ProfileFieldRef, ProfileNedReference, ProfileOrientation, ProfilePosition,
    ResolvedVehicleField, VehicleNedReference, VehicleOrientation, VehiclePosition,
};

use crate::protocol::v1::control::*;
use crate::protocol::v1::error::ApiError;

pub(super) fn geometry_dto(
    geometry: AnnotationGeometry,
) -> Result<AnnotationGeometryDto, ApiError> {
    let point = |value: (i64, f64)| -> Result<PointDto, ApiError> {
        Ok(PointDto {
            time_ns: us_to_ns(value.0)?,
            y: value.1,
        })
    };
    Ok(match geometry {
        AnnotationGeometry::Text { at } => AnnotationGeometryDto::Text { at: point(at)? },
        AnnotationGeometry::Segment { from, to } => AnnotationGeometryDto::Segment {
            from: point(from)?,
            to: point(to)?,
        },
        AnnotationGeometry::Rect { a, b } => AnnotationGeometryDto::Rect {
            a: point(a)?,
            b: point(b)?,
        },
        AnnotationGeometry::Ellipse { a, b } => AnnotationGeometryDto::Ellipse {
            a: point(a)?,
            b: point(b)?,
        },
        AnnotationGeometry::HLine { y } => AnnotationGeometryDto::HLine { y },
    })
}

pub(super) fn ns_to_us(value: i64) -> Result<i64, ApiError> {
    if value % 1000 != 0 {
        return Err(ApiError::invalid_input(
            "timestamp nanoseconds must be an exact microsecond",
        ));
    }
    Ok(value / 1000)
}

pub(super) fn us_to_ns(value: i64) -> Result<i64, ApiError> {
    value
        .checked_mul(1000)
        .ok_or_else(|| ApiError::internal("timestamp overflows wire nanoseconds"))
}

pub(super) fn color(value: Option<String>) -> Result<Option<[f32; 4]>, ApiError> {
    value
        .map(|value| parse_hex_color(&value).map_err(ApiError::from))
        .transpose()
}

pub(super) fn profile_dto(profile: delog_api::control::VehicleProfileInfo) -> VehicleProfileDto {
    VehicleProfileDto {
        name: profile.name,
        label: profile.label,
        show: profile.show,
        show_path: profile.show_path,
        position: profile_position_json(profile.position),
        orientation: profile_orientation_json(profile.orientation),
        model: profile.model.as_str().to_owned(),
        color: format_hex_color(profile.color),
        path_color: format_hex_color(profile.path_color),
        scale: profile.scale,
    }
}

pub(super) fn profile_field_json(field: ProfileFieldRef) -> serde_json::Value {
    serde_json::json!({"topic": field.topic, "field": field.field})
}

pub(super) fn vehicle_field_json(field: ResolvedVehicleField) -> serde_json::Value {
    serde_json::json!({"path": field.path})
}

pub(super) fn vehicle_reference_json(reference: VehicleNedReference) -> serde_json::Value {
    match reference {
        VehicleNedReference::Manual {
            lat_deg,
            lon_deg,
            alt_m,
        } => serde_json::json!({
            "kind": "manual", "lat_deg": lat_deg, "lon_deg": lon_deg, "alt_m": alt_m,
        }),
        VehicleNedReference::Fields { lat, lon, alt } => serde_json::json!({
            "kind": "fields", "lat": vehicle_field_json(lat), "lon": vehicle_field_json(lon),
            "alt": vehicle_field_json(alt),
        }),
    }
}

pub(super) fn vehicle_position_json(position: VehiclePosition) -> serde_json::Value {
    match position {
        VehiclePosition::Ned {
            north,
            east,
            down,
            reference,
        } => serde_json::json!({
            "kind": "ned", "north": vehicle_field_json(north), "east": vehicle_field_json(east),
            "down": vehicle_field_json(down), "reference": reference.map(vehicle_reference_json),
        }),
        VehiclePosition::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => serde_json::json!({
            "kind": "gps", "lat": vehicle_field_json(lat), "lon": vehicle_field_json(lon),
            "alt": vehicle_field_json(alt), "lat_lon_dege7": lat_lon_dege7,
            "alt_mm": alt_mm, "alt_offset_m": alt_offset_m,
        }),
    }
}

pub(super) fn vehicle_orientation_json(orientation: VehicleOrientation) -> serde_json::Value {
    match orientation {
        VehicleOrientation::Static => serde_json::json!({"kind": "static"}),
        VehicleOrientation::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => serde_json::json!({
            "kind": "euler", "roll": vehicle_field_json(roll), "pitch": vehicle_field_json(pitch),
            "yaw": vehicle_field_json(yaw), "degrees": degrees,
        }),
        VehicleOrientation::Quat { w, x, y, z } => serde_json::json!({
            "kind": "quat", "w": vehicle_field_json(w), "x": vehicle_field_json(x),
            "y": vehicle_field_json(y), "z": vehicle_field_json(z),
        }),
    }
}

pub(super) fn profile_reference_json(reference: ProfileNedReference) -> serde_json::Value {
    match reference {
        ProfileNedReference::Manual {
            lat_deg,
            lon_deg,
            alt_m,
        } => serde_json::json!({
            "kind": "manual", "lat_deg": lat_deg, "lon_deg": lon_deg, "alt_m": alt_m,
        }),
        ProfileNedReference::Fields { lat, lon, alt } => serde_json::json!({
            "kind": "fields", "lat": profile_field_json(lat), "lon": profile_field_json(lon), "alt": profile_field_json(alt),
        }),
    }
}

pub(super) fn profile_position_json(position: ProfilePosition) -> serde_json::Value {
    match position {
        ProfilePosition::Ned {
            north,
            east,
            down,
            reference,
        } => serde_json::json!({
            "kind": "ned", "north": profile_field_json(north), "east": profile_field_json(east),
            "down": profile_field_json(down), "reference": reference.map(profile_reference_json),
        }),
        ProfilePosition::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => serde_json::json!({
            "kind": "gps", "lat": profile_field_json(lat), "lon": profile_field_json(lon),
            "alt": profile_field_json(alt), "lat_lon_dege7": lat_lon_dege7, "alt_mm": alt_mm,
            "alt_offset_m": alt_offset_m,
        }),
    }
}

pub(super) fn profile_orientation_json(orientation: ProfileOrientation) -> serde_json::Value {
    match orientation {
        ProfileOrientation::Static => serde_json::json!({"kind": "static"}),
        ProfileOrientation::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => serde_json::json!({
            "kind": "euler", "roll": profile_field_json(roll), "pitch": profile_field_json(pitch),
            "yaw": profile_field_json(yaw), "degrees": degrees,
        }),
        ProfileOrientation::Quat { w, x, y, z } => serde_json::json!({
            "kind": "quat", "w": profile_field_json(w), "x": profile_field_json(x),
            "y": profile_field_json(y), "z": profile_field_json(z),
        }),
    }
}

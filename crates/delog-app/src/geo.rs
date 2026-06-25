//! Frames:
//! - **Geodetic**: WGS84 latitude/longitude (radians) + altitude (m).
//! - **ECEF**: earth-centred earth-fixed metres (f64).
//! - **NED**: local tangent metres about a reference origin — North, East, Down.
//! - **Render**: right-handed Y-up, `render = (E, −D, −N)` i.e. X=East, Y=Up,
//!   Z=South.

use glam::{DVec3, EulerRot, Mat3, Quat, Vec3};

const WGS84_A: f64 = 6_378_137.0; // semi-major axis (m)
const WGS84_F: f64 = 1.0 / 298.257_223_563; // flattening
const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F); // first eccentricity squared

/// Geodetic (lat/lon radians, alt m) → ECEF metres.
pub fn geodetic_to_ecef(lat_rad: f64, lon_rad: f64, alt_m: f64) -> DVec3 {
    let (sin_lat, cos_lat) = lat_rad.sin_cos();
    let (sin_lon, cos_lon) = lon_rad.sin_cos();
    let n = WGS84_A / (1.0 - WGS84_E2 * sin_lat * sin_lat).sqrt();
    DVec3::new(
        (n + alt_m) * cos_lat * cos_lon,
        (n + alt_m) * cos_lat * sin_lon,
        (n * (1.0 - WGS84_E2) + alt_m) * sin_lat,
    )
}

/// ECEF → local NED metres; `ref_lat`/`ref_lon` in radians. Returns
/// `(North, East, Down)`.
pub fn ecef_to_ned(ecef: DVec3, ref_ecef: DVec3, ref_lat: f64, ref_lon: f64) -> DVec3 {
    let d = ecef - ref_ecef;
    let (sl, cl) = ref_lat.sin_cos();
    let (so, co) = ref_lon.sin_cos();
    DVec3::new(
        -sl * co * d.x - sl * so * d.y + cl * d.z,
        -so * d.x + co * d.y,
        -cl * co * d.x - cl * so * d.y - sl * d.z,
    )
}

/// Geodetic → NED metres about a geodetic reference origin (all radians/m).
pub fn geodetic_to_ned(
    lat_rad: f64,
    lon_rad: f64,
    alt_m: f64,
    ref_lat_rad: f64,
    ref_lon_rad: f64,
    ref_alt_m: f64,
) -> DVec3 {
    let ecef = geodetic_to_ecef(lat_rad, lon_rad, alt_m);
    let ref_ecef = geodetic_to_ecef(ref_lat_rad, ref_lon_rad, ref_alt_m);
    ecef_to_ned(ecef, ref_ecef, ref_lat_rad, ref_lon_rad)
}

/// NED metres → render space `(E, −D, −N)`.
pub fn ned_to_render(ned: Vec3) -> Vec3 {
    Vec3::new(ned.y, -ned.z, -ned.x)
}

pub fn ned_to_render_mat3() -> Mat3 {
    Mat3::from_cols(
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
    )
}

/// Body→NED quaternion from intrinsic Z-Y-X Euler angles (radians) — the
/// AP/PX4 convention.
pub fn euler_to_quat(roll: f32, pitch: f32, yaw: f32) -> Quat {
    Quat::from_euler(EulerRot::ZYX, yaw, pitch, roll)
}

pub fn body_to_render_rot(body_to_ned: Quat) -> Mat3 {
    ned_to_render_mat3() * Mat3::from_quat(body_to_ned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEG: f64 = std::f64::consts::PI / 180.0;

    #[test]
    fn reference_point_maps_to_ned_origin() {
        let (lat, lon, alt) = (37.0 * DEG, -122.0 * DEG, 30.0);
        let ned = geodetic_to_ned(lat, lon, alt, lat, lon, alt);
        assert!(
            ned.length() < 1e-6,
            "reference should be NED origin, got {ned:?}"
        );
    }

    #[test]
    fn moving_north_increases_north_only() {
        let (lat, lon, alt) = (37.0 * DEG, -122.0 * DEG, 0.0);
        let ned = geodetic_to_ned(lat + 0.001 * DEG, lon, alt, lat, lon, alt);
        assert!(
            ned.x > 100.0 && ned.x < 120.0,
            "north ≈111 m, got {}",
            ned.x
        );
        assert!(ned.y.abs() < 1.0, "east drift {}", ned.y);
        assert!(ned.z.abs() < 1.0, "down drift {}", ned.z);
    }

    #[test]
    fn moving_east_increases_east_and_altitude_is_down_negative() {
        let (lat, lon, alt) = (0.0, 0.0, 0.0);
        let east = geodetic_to_ned(0.0, 0.001 * DEG, 0.0, lat, lon, alt);
        assert!(
            east.y > 100.0 && east.y < 120.0,
            "east ≈111 m, got {}",
            east.y
        );
        let up = geodetic_to_ned(0.0, 0.0, 50.0, lat, lon, alt);
        assert!(
            (up.z + 50.0).abs() < 1e-3,
            "down should be -50, got {}",
            up.z
        );
    }

    #[test]
    fn ned_maps_to_render_e_negd_negn() {
        let r = ned_to_render(Vec3::new(1.0, 2.0, 3.0));
        assert!((r - Vec3::new(2.0, -3.0, -1.0)).length() < 1e-6);
        let rm = ned_to_render_mat3() * Vec3::new(1.0, 2.0, 3.0);
        assert!((rm - r).length() < 1e-6);
    }

    #[test]
    fn ned_to_render_is_a_proper_rotation() {
        let m = ned_to_render_mat3();
        assert!(
            (m.determinant() - 1.0).abs() < 1e-6,
            "det = {}",
            m.determinant()
        );
    }

    #[test]
    fn yaw_rotates_body_forward_in_the_horizontal_plane() {
        let q = euler_to_quat(0.0, 0.0, 90f32.to_radians());
        let ned_forward = q * Vec3::X;
        assert!(
            (ned_forward - Vec3::Y).length() < 1e-5,
            "yaw 90° should map N→E, got {ned_forward:?}"
        );
    }

    #[test]
    fn zero_attitude_body_forward_points_render_south_axis() {
        let rot = body_to_render_rot(euler_to_quat(0.0, 0.0, 0.0));
        let fwd = rot * Vec3::X;
        assert!(
            (fwd - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5,
            "got {fwd:?}"
        );
    }
}

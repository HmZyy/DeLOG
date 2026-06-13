//! Tracking orbit camera for the 3D scene (PLAN.md §12.3, TDV-01/02).
//!
//! Pure math — no egui, no wgpu — so it unit-tests without a window. The scene
//! works in render space (`X = East, Y = Up, Z = South`, right-handed, Y-up;
//! PLAN.md §12.2). There is one camera: it always orbits a `target` point that
//! tracks the selected vehicle's pose (or the world origin when no vehicle is
//! configured). `yaw` is the azimuth about the up axis, `pitch` the elevation
//! above the horizon, `distance` the radius; setting a new `target` preserves
//! that offset, so the view follows the vehicle rigidly. The app maps a
//! left-drag to [`OrbitCamera::orbit`] and the wheel to [`OrbitCamera::zoom`].

use glam::{Mat4, Vec3};

/// Vertical field of view, radians.
const FOV_Y: f32 = 0.95; // ~54°
const NEAR: f32 = 0.05;
#[cfg(test)]
const FAR: f32 = 2000.0;

/// Pitch is clamped just shy of the poles so the view never gimbal-flips.
const PITCH_LIMIT: f32 = 1.5533; // ~89° in radians

const MIN_DISTANCE: f32 = 0.5;
#[cfg(test)]
const MAX_DISTANCE: f32 = 1500.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// Azimuth about the up (Y) axis, radians.
    pub yaw: f32,
    /// Elevation above the horizon plane, radians (clamped to ±[`PITCH_LIMIT`]).
    pub pitch: f32,
    pub distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        // A three-quarter view onto the origin: looking roughly north-east and
        // down at ~30°, far enough to frame a few dozen grid cells.
        Self {
            target: Vec3::ZERO,
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: 0.52, // ~30°
            distance: 30.0,
        }
    }
}

impl OrbitCamera {
    /// World-space eye position derived from the orbit angles.
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // Spherical → cartesian about the target, up = +Y.
        let dir = Vec3::new(cp * sy, sp, cp * cy);
        self.target + dir * self.distance
    }

    /// World → clip transform for a viewport of the given aspect (w / h).
    /// Uses the `[0, 1]` depth convention (wgpu), matching the grid shader.
    #[cfg(test)]
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.view_proj_with_far(aspect, FAR)
    }

    /// World → clip transform using a caller-provided far plane.
    pub fn view_proj_with_far(&self, aspect: f32, far: f32) -> Mat4 {
        let far = far.max(NEAR + 1.0);
        let proj = Mat4::perspective_rh(FOV_Y, aspect.max(1e-3), NEAR, far);
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Y);
        proj * view
    }

    /// Rotate the orbit by the given yaw/pitch deltas (radians); pitch is
    /// clamped so the camera never crosses the poles.
    pub fn orbit(&mut self, d_yaw: f32, d_pitch: f32) {
        self.yaw += d_yaw;
        self.pitch = (self.pitch + d_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Scale the orbit radius by `factor` (`<1` zooms in), clamped to a sane
    /// range so the camera can neither tunnel through the target nor fly off.
    #[cfg(test)]
    pub fn zoom(&mut self, factor: f32) {
        self.zoom_with_max(factor, MAX_DISTANCE);
    }

    /// Scale the orbit radius with a caller-provided maximum distance.
    pub fn zoom_with_max(&mut self, factor: f32, max_distance: f32) {
        let max_distance = max_distance.max(MIN_DISTANCE);
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, max_distance);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ndc(m: Mat4, p: Vec3) -> Vec3 {
        let c = m * p.extend(1.0);
        c.truncate() / c.w
    }

    #[test]
    fn eye_sits_at_distance_from_target() {
        for &(yaw, pitch) in &[(0.0, 0.0), (1.0, 0.5), (-2.3, -0.9), (3.0, 1.2)] {
            let cam = OrbitCamera {
                target: Vec3::new(2.0, -1.0, 4.0),
                yaw,
                pitch,
                distance: 12.0,
            };
            let r = (cam.eye() - cam.target).length();
            assert!((r - 12.0).abs() < 1e-3, "radius {r} != distance");
        }
    }

    #[test]
    fn pitch_clamps_at_the_poles() {
        let mut cam = OrbitCamera::default();
        cam.orbit(0.0, 100.0);
        assert!(cam.pitch <= PITCH_LIMIT && cam.pitch >= -PITCH_LIMIT);
        cam.orbit(0.0, -100.0);
        assert!(cam.pitch >= -PITCH_LIMIT);
    }

    #[test]
    fn zoom_scales_and_clamps_distance() {
        let mut cam = OrbitCamera::default(); // distance 30
        cam.zoom(0.5);
        assert!((cam.distance - 15.0).abs() < 1e-3);
        cam.zoom(0.0001); // would underflow
        assert!(cam.distance >= MIN_DISTANCE);
        cam.zoom(1e9); // would overflow
        assert!(cam.distance <= MAX_DISTANCE);
    }

    #[test]
    fn zoom_can_use_a_custom_max_distance() {
        let mut cam = OrbitCamera::default();
        cam.zoom_with_max(1e9, 12_000.0);
        assert!((cam.distance - 12_000.0).abs() < 1e-3);
    }

    #[test]
    fn target_projects_to_screen_center() {
        let cam = OrbitCamera::default();
        let p = ndc(cam.view_proj(16.0 / 9.0), cam.target);
        assert!(
            p.x.abs() < 1e-3 && p.y.abs() < 1e-3,
            "target not centered: {p:?}"
        );
        // In front of the camera: clip-space depth within [0, 1].
        assert!(p.z > 0.0 && p.z < 1.0, "target depth out of range: {}", p.z);
    }

    #[test]
    fn custom_far_plane_keeps_far_points_inside_clip_depth() {
        let cam = OrbitCamera {
            target: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            distance: 30.0,
        };
        let point = Vec3::new(0.0, 0.0, -8_000.0);
        let p = ndc(cam.view_proj_with_far(16.0 / 9.0, 10_000.0), point);
        assert!(p.z > 0.0 && p.z < 1.0, "far point clipped: {p:?}");
    }

    #[test]
    fn up_in_world_is_up_on_screen() {
        let cam = OrbitCamera::default();
        let vp = cam.view_proj(1.0);
        let center = ndc(vp, cam.target);
        let above = ndc(vp, cam.target + Vec3::Y); // one unit up (render Y = Up)
        assert!(
            above.y > center.y,
            "raising a point in world Y should move it up in NDC ({} !> {})",
            above.y,
            center.y
        );
    }

    #[test]
    fn moving_the_target_preserves_the_orbit_offset() {
        // The tracking camera follows a vehicle by moving its target; the
        // yaw/pitch/distance offset must be preserved so the eye translates
        // rigidly with the target (§12.3).
        let mut cam = OrbitCamera {
            target: Vec3::new(2.0, 0.0, -1.0),
            ..OrbitCamera::default()
        };
        let eye_a = cam.eye();
        cam.target = Vec3::new(12.0, 4.0, -1.0); // target moves +X+Y
        let delta = cam.eye() - eye_a;
        assert!(
            (delta - Vec3::new(10.0, 4.0, 0.0)).length() < 1e-3,
            "eye should follow target rigidly, moved {delta:?}"
        );
    }
}

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

use glam::{DMat4, DVec3, DVec4, Mat4, Vec3};

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

    /// World → clip, plus a clip → **camera-relative world** inverse (mapping a
    /// clip point to `world − eye`). Both are built and inverted in **f64**
    /// before downcasting to the f32 GPU uniform; the grid shader adds `cam_pos`
    /// back after the per-pixel ray/ground intersection.
    ///
    /// Why camera-relative: the grid is world-anchored, so the shader
    /// reconstructs each pixel's ground position by unprojecting through this
    /// inverse. When the camera tracks a vehicle far from the render origin, the
    /// absolute ground point has a large magnitude (kilometres), and pushing it
    /// through f32 unprojection arithmetic leaves *metres* of error that shift
    /// discretely as the camera moves — the world grid visibly crawls while
    /// zooming/following. Unprojecting **relative to the camera** keeps every f32
    /// operand small (order of the camera's height, not its absolute position),
    /// and the only large quantity, `eye`, is added back as a clean uniform — so
    /// the reconstructed grid coordinate stays stable to a fraction of a
    /// millimetre regardless of how far the vehicle has flown.
    ///
    /// The inverse uses the rotation-only view (translation zeroed): for the full
    /// view `V`, `V·world = R·(world − eye)`, so `world − eye = (proj·R)⁻¹·clip`.
    pub fn view_proj_and_inverse(&self, aspect: f32, far: f32) -> (Mat4, Mat4) {
        let far = (far as f64).max(NEAR as f64 + 1.0);
        let proj = DMat4::perspective_rh(FOV_Y as f64, (aspect as f64).max(1e-3), NEAR as f64, far);
        let view = DMat4::look_at_rh(self.eye().as_dvec3(), self.target.as_dvec3(), DVec3::Y);
        // Rotation-only view (drop the translation column) → its inverse maps
        // clip space to world coordinates measured from the camera.
        let mut view_rot = view;
        view_rot.w_axis = DVec4::new(0.0, 0.0, 0.0, 1.0);
        let inv_rel = (proj * view_rot).inverse();
        ((proj * view).as_mat4(), inv_rel.as_mat4())
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

    /// Old (absolute) reconstruction: unproject near/far through an absolute
    /// clip→world inverse in f32 and intersect y=0. This is the pre-fix path,
    /// kept here only as the baseline the camera-relative path must beat.
    fn ground_hit_abs(inv: Mat4, ndc: glam::Vec2) -> glam::Vec2 {
        let up = |z: f32| {
            let p = inv * ndc.extend(z).extend(1.0);
            p.truncate() / p.w
        };
        let (near, far) = (up(0.0), up(1.0));
        let dir = far - near;
        let t = -near.y / dir.y;
        let w = near + t * dir;
        glam::Vec2::new(w.x, w.z)
    }

    /// Camera-relative reconstruction, mirroring the fixed grid shader: `inv` is
    /// clip→(world−cam_pos), so the hit is found in small camera-relative
    /// coordinates and `cam_pos` is added back at the end. Done in f32, like the
    /// GPU.
    fn ground_hit_rel(inv: Mat4, cam_pos: Vec3, ndc: glam::Vec2) -> glam::Vec2 {
        let up = |z: f32| {
            let p = inv * ndc.extend(z).extend(1.0);
            p.truncate() / p.w
        };
        let (near, far) = (up(0.0), up(1.0));
        let dir = far - near;
        let t = (-cam_pos.y - near.y) / dir.y;
        let rel = near + t * dir;
        glam::Vec2::new(cam_pos.x + rel.x, cam_pos.z + rel.z)
    }

    /// Regression for 3D grid jitter (GPU-21): when the camera tracks a vehicle
    /// far from the render origin, the world-anchored grid must not crawl as the
    /// camera follows it. The shader anchors lines to the reconstructed
    /// `world.xz`, so the spurious frame-to-frame shift of that point (beyond the
    /// vehicle's true motion) is the visible jitter. Camera-relative
    /// reconstruction (what [`OrbitCamera::view_proj_and_inverse`] feeds the
    /// shader) keeps that shift to a fraction of a millimetre; the original
    /// absolute f32 path let it reach ~20 % of a cell, and even an f64 *absolute*
    /// inverse still left ~0.9 m when zooming at range.
    #[test]
    fn following_a_distant_vehicle_does_not_jitter_the_grid() {
        let aspect = 16.0 / 9.0;
        let far = 20_000.0;
        let ndc = glam::Vec2::new(0.13, -0.27); // a pixel looking at the ground

        // Vehicle 3 km from the render origin; one follow step of 0.3 m.
        let cam_a = OrbitCamera {
            target: Vec3::new(3000.0, 80.0, -2000.0),
            yaw: 0.7,
            pitch: 0.52,
            distance: 150.0,
        };
        let cam_b = OrbitCamera {
            target: cam_a.target + Vec3::new(0.3, 0.0, 0.0),
            ..cam_a
        };

        // Ground truth motion of the reconstructed point, computed in full f64.
        let truth = |c: &OrbitCamera| {
            let proj = DMat4::perspective_rh(FOV_Y as f64, aspect as f64, NEAR as f64, far as f64);
            let view = DMat4::look_at_rh(c.eye().as_dvec3(), c.target.as_dvec3(), DVec3::Y);
            let inv = (proj * view).inverse();
            let up = |z: f64| {
                let p = inv * glam::DVec4::new(ndc.x as f64, ndc.y as f64, z, 1.0);
                p.truncate() / p.w
            };
            let (near, far_p) = (up(0.0), up(1.0));
            let dir = far_p - near;
            let t = -near.y / dir.y;
            let w = near + t * dir;
            glam::DVec2::new(w.x, w.z)
        };
        let true_move = truth(&cam_b) - truth(&cam_a);
        let true_move = glam::Vec2::new(true_move.x as f32, true_move.y as f32);

        // Camera-relative (the fix): reconstructed motion ≈ true motion.
        let rel_move = ground_hit_rel(cam_b.view_proj_and_inverse(aspect, far).1, cam_b.eye(), ndc)
            - ground_hit_rel(cam_a.view_proj_and_inverse(aspect, far).1, cam_a.eye(), ndc);
        let rel_jitter = (rel_move - true_move).length();

        // Original absolute f32 path: much larger spurious shift.
        let abs_move = ground_hit_abs(cam_b.view_proj_with_far(aspect, far).inverse(), ndc)
            - ground_hit_abs(cam_a.view_proj_with_far(aspect, far).inverse(), ndc);
        let abs_jitter = (abs_move - true_move).length();

        // The fix keeps jitter well under a millimetre and orders of magnitude
        // tighter than the original absolute path.
        assert!(
            rel_jitter < 1e-3,
            "camera-relative jitter should be sub-mm, got {rel_jitter} m"
        );
        assert!(
            rel_jitter * 100.0 < abs_jitter,
            "camera-relative ({rel_jitter} m) should be »100× tighter than absolute ({abs_jitter} m)"
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

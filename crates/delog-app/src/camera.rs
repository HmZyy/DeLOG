//! Orbit camera for the 3D scene (PLAN.md §12.3, TDV-01).
//!
//! Pure math — no egui, no wgpu — so it unit-tests without a window. The scene
//! works in render space (`X = East, Y = Up, Z = South`, right-handed, Y-up;
//! PLAN.md §12.2), and the orbit camera circles a `target` point: `yaw` is the
//! azimuth about the up axis, `pitch` the elevation above the horizon, and
//! `distance` the radius. The app maps a left-drag to [`OrbitCamera::orbit`] and
//! the wheel to [`OrbitCamera::zoom`].

use glam::{Mat4, Vec3};

/// Vertical field of view, radians.
const FOV_Y: f32 = 0.95; // ~54°
const NEAR: f32 = 0.05;
const FAR: f32 = 2000.0;

/// Pitch is clamped just shy of the poles so the view never gimbal-flips.
const PITCH_LIMIT: f32 = 1.5533; // ~89° in radians

const MIN_DISTANCE: f32 = 0.5;
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
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let proj = Mat4::perspective_rh(FOV_Y, aspect.max(1e-3), NEAR, FAR);
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
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }
}

/// A free-fly camera (PLAN.md §12.3): an eye position with yaw/pitch look
/// angles. The app maps mouse-drag to [`FreeCamera::look`] and WASD to
/// [`FreeCamera::fly`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeCamera {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for FreeCamera {
    fn default() -> Self {
        Self::looking_from(Vec3::new(20.0, 15.0, 20.0), Vec3::ZERO)
    }
}

impl FreeCamera {
    /// Place the camera at `eye` looking toward `target` — used when switching
    /// from orbit so the view doesn't jump.
    pub fn looking_from(eye: Vec3, target: Vec3) -> Self {
        let d = (target - eye).normalize_or_zero();
        let pitch = d.y.clamp(-1.0, 1.0).asin();
        // yaw chosen so `forward()` reproduces `d` (see its convention).
        let yaw = d.x.atan2(d.z);
        Self {
            pos: eye,
            yaw,
            pitch,
        }
    }

    /// Unit look direction from the yaw/pitch angles.
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cp * sy, sp, cp * cy)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let proj = Mat4::perspective_rh(FOV_Y, aspect.max(1e-3), NEAR, FAR);
        let view = Mat4::look_at_rh(self.pos, self.pos + self.forward(), Vec3::Y);
        proj * view
    }

    /// Rotate the look direction; pitch is clamped shy of the poles.
    pub fn look(&mut self, d_yaw: f32, d_pitch: f32) {
        self.yaw += d_yaw;
        self.pitch = (self.pitch + d_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Translate along the camera basis: `fwd` along the look direction, `right`
    /// sideways, `up` along world up (WASD + vertical).
    pub fn fly(&mut self, fwd: f32, right: f32, up: f32) {
        let f = self.forward();
        let r = f.cross(Vec3::Y).normalize_or_zero();
        self.pos += f * fwd + r * right + Vec3::Y * up;
    }
}

/// Which camera drives the 3D scene (PLAN.md §12.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraMode {
    /// Circle a fixed target (origin or a selected vehicle).
    Orbit,
    /// Circle a target that follows the tracked vehicle, preserving the user's
    /// orbit offset.
    Track,
    /// Free fly (WASD + mouse).
    Free,
}

/// The scene's camera: an orbit rig shared by Orbit/Track plus a free-fly rig,
/// switched by [`CameraMode`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneCamera {
    pub mode: CameraMode,
    pub orbit: OrbitCamera,
    pub free: FreeCamera,
}

impl Default for SceneCamera {
    fn default() -> Self {
        Self {
            mode: CameraMode::Orbit,
            orbit: OrbitCamera::default(),
            free: FreeCamera::default(),
        }
    }
}

impl SceneCamera {
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        match self.mode {
            CameraMode::Orbit | CameraMode::Track => self.orbit.view_proj(aspect),
            CameraMode::Free => self.free.view_proj(aspect),
        }
    }

    pub fn eye(&self) -> Vec3 {
        match self.mode {
            CameraMode::Orbit | CameraMode::Track => self.orbit.eye(),
            CameraMode::Free => self.free.pos,
        }
    }

    /// Distance used to scale the grid's distance fade.
    pub fn fade_distance(&self) -> f32 {
        match self.mode {
            CameraMode::Orbit | CameraMode::Track => self.orbit.distance,
            CameraMode::Free => self.free.pos.length().max(1.0),
        }
    }

    /// Switch mode, seeding the free camera from the current orbit view so the
    /// picture doesn't jump when entering Free.
    pub fn set_mode(&mut self, mode: CameraMode) {
        if mode == CameraMode::Free && self.mode != CameraMode::Free {
            self.free = FreeCamera::looking_from(self.orbit.eye(), self.orbit.target);
        }
        self.mode = mode;
    }

    /// In Track mode, recenter on the followed point — the orbit offset
    /// (yaw/pitch/distance) is preserved, so the eye translates with the target
    /// (§12.3). No-op in other modes.
    pub fn set_track_target(&mut self, target: Vec3) {
        if self.mode == CameraMode::Track {
            self.orbit.target = target;
        }
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
    fn free_camera_forward_is_unit_and_recovered_from_a_look_direction() {
        let cam = FreeCamera::looking_from(Vec3::new(5.0, 5.0, 5.0), Vec3::ZERO);
        let f = cam.forward();
        assert!((f.length() - 1.0).abs() < 1e-4);
        // forward should point from the eye toward the origin.
        let want = (Vec3::ZERO - Vec3::new(5.0, 5.0, 5.0)).normalize();
        assert!((f - want).length() < 1e-3, "forward {f:?} != {want:?}");
    }

    #[test]
    fn free_camera_flies_along_its_basis_and_clamps_pitch() {
        let mut cam = FreeCamera {
            pos: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
        };
        // yaw=pitch=0 ⇒ forward = +Z.
        cam.fly(2.0, 0.0, 0.0);
        assert!((cam.pos - Vec3::new(0.0, 0.0, 2.0)).length() < 1e-4);
        cam.fly(0.0, 0.0, 3.0); // world-up
        assert!((cam.pos - Vec3::new(0.0, 3.0, 2.0)).length() < 1e-4);
        cam.look(0.0, 100.0);
        assert!(cam.pitch <= PITCH_LIMIT);
    }

    #[test]
    fn track_preserves_orbit_offset_as_the_target_moves() {
        let mut cam = SceneCamera {
            mode: CameraMode::Track,
            ..SceneCamera::default()
        };
        cam.set_track_target(Vec3::new(2.0, 0.0, -1.0));
        let eye_a = cam.eye();
        cam.set_track_target(Vec3::new(12.0, 4.0, -1.0)); // target moves +X+Y
        let eye_b = cam.eye();
        // The eye should translate by exactly the target delta (offset kept).
        let delta = eye_b - eye_a;
        assert!(
            (delta - Vec3::new(10.0, 4.0, 0.0)).length() < 1e-3,
            "eye should follow target rigidly, moved {delta:?}"
        );
    }

    #[test]
    fn entering_free_mode_seeds_from_the_orbit_eye() {
        let mut cam = SceneCamera::default(); // Orbit
        let orbit_eye = cam.orbit.eye();
        cam.set_mode(CameraMode::Free);
        assert!(
            (cam.free.pos - orbit_eye).length() < 1e-4,
            "free eye should match orbit eye"
        );
        // And the view is continuous: the orbit target stays roughly centered.
        let p = ndc(cam.view_proj(1.0), cam.orbit.target);
        assert!(p.x.abs() < 0.05 && p.y.abs() < 0.05, "view jumped: {p:?}");
    }

    #[test]
    fn track_target_is_ignored_outside_track_mode() {
        let mut cam = SceneCamera::default(); // Orbit, target at origin
        cam.set_track_target(Vec3::new(99.0, 0.0, 0.0));
        assert_eq!(cam.orbit.target, Vec3::ZERO);
    }
}

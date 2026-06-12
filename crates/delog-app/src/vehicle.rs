//! Vehicle configuration + pose/trajectory resolution for the 3D view
//! (PLAN.md §12.1-§12.2, TDV-03/04/06). A [`VehicleConfig`] maps a source's
//! fields to position and orientation; [`pose_at`] reads the pose at a playback
//! time and [`build_trajectory`] resamples the whole path — both into render
//! space (§12.2) via [`crate::geo`].
//!
//! Field samples are read through `delog-core`'s [`FieldView`] (the app never
//! touches Arrow directly, §3.2); `sample_at` returns the raw stored value, so
//! the schema multiplier is applied here to get engineering units.

use std::path::PathBuf;

use delog_core::field_view::{FieldView, SampleMode};
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;
use glam::{Mat3, Mat4, Quat, Vec3};

use crate::geo;

/// Which mesh represents a vehicle (§12.1). `Cone` is the basic procedural
/// cone — the same shape used as the unconditional fallback.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ModelKind {
    Quad,
    FixedWing,
    DeltaWing,
    Cone,
    CustomGlb(PathBuf),
}

impl ModelKind {
    pub const PRESETS: [ModelKind; 4] = [
        ModelKind::Quad,
        ModelKind::FixedWing,
        ModelKind::DeltaWing,
        ModelKind::Cone,
    ];

    pub fn label(&self) -> &str {
        match self {
            ModelKind::Quad => "Quad",
            ModelKind::FixedWing => "Fixed-wing",
            ModelKind::DeltaWing => "Delta-wing",
            ModelKind::Cone => "Cone",
            ModelKind::CustomGlb(_) => "Custom GLB",
        }
    }

    /// Mesh→body correction. Meshes are authored Y-up (glTF convention); the
    /// body frame is X-forward/Z-down, so every model gets a −90° about X
    /// (mesh up → body up). Quad and Delta-wing are additionally authored with
    /// the nose along mesh −Z and get a −90° about mesh-up first to bring the
    /// nose to body +X. Values match the reference tiplot implementation
    /// (base × per-type offset).
    pub fn orientation_offset(&self) -> Mat3 {
        let base = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        match self {
            ModelKind::Quad | ModelKind::DeltaWing => {
                base * Mat3::from_rotation_y(-std::f32::consts::FRAC_PI_2)
            }
            ModelKind::FixedWing | ModelKind::Cone | ModelKind::CustomGlb(_) => base,
        }
    }
}

/// A fixed geodetic reference origin (degrees / metres).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoRef {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
}

/// The geodetic origin of a local NED frame: read from log columns, or fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NedReference {
    /// Fixed lat/lon (degrees) + altitude (m).
    Manual(GeoRef),
    /// Read from lat/lon/alt columns (first valid sample).
    Fields {
        lat: FieldId,
        lon: FieldId,
        alt: FieldId,
    },
}

/// How a vehicle's position is read (§12.1).
#[derive(Clone, Debug, PartialEq)]
pub enum PosMapping {
    /// Already-local NED metres, optionally annotated with the geodetic origin
    /// of the local frame (captured for geo-referencing; does not move a single
    /// vehicle's local render).
    Ned {
        north: FieldId,
        east: FieldId,
        down: FieldId,
        reference: Option<NedReference>,
    },
    /// Geodetic latitude/longitude (degrees) + altitude → NED about the first
    /// valid fix (auto reference).
    Gps {
        lat: FieldId,
        lon: FieldId,
        alt: FieldId,
    },
}

/// How a vehicle's orientation is read (§12.1).
#[derive(Clone, Debug, PartialEq)]
pub enum OriMapping {
    /// Level attitude (identity body→NED rotation).
    Static,
    /// Intrinsic Z-Y-X Euler (yaw-pitch-roll), body→NED.
    Euler {
        roll: FieldId,
        pitch: FieldId,
        yaw: FieldId,
        degrees: bool,
    },
    /// Hamilton quaternion, body→NED, w-first.
    Quat {
        w: FieldId,
        x: FieldId,
        y: FieldId,
        z: FieldId,
    },
}

/// A configured vehicle (§12.1).
#[derive(Clone, Debug)]
pub struct VehicleConfig {
    pub source: SourceId,
    pub label: String,
    pub show: bool,
    pub pos: PosMapping,
    pub ori: OriMapping,
    pub model: ModelKind,
    pub color: Color32,
    pub path_color: Color32,
    pub scale: f32,
}

/// A vehicle pose in render space: position + rotation.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub pos: Vec3,
    pub rot: Mat3,
}

impl Pose {
    /// Model matrix placing a body-frame mesh at this pose, scaled.
    pub fn model_matrix(&self, scale: f32) -> Mat4 {
        Mat4::from_translation(self.pos)
            * Mat4::from_mat3(self.rot)
            * Mat4::from_scale(Vec3::splat(scale))
    }
}

/// The schema multiplier that converts a field's raw stored value to its
/// engineering unit (1.0 if unknown).
fn field_multiplier(snapshot: &StoreSnapshot, field: FieldId) -> f64 {
    let Some(entry) = snapshot.fields.get(field.index()) else {
        return 1.0;
    };
    snapshot
        .topic_store(entry.topic)
        .and_then(|store| store.schema.field_by_name(&entry.name))
        .map(|f| f.multiplier)
        .unwrap_or(1.0)
}

/// Read a field's engineering value (raw × multiplier) at an effective time.
fn read_eng(view: &FieldView<'_>, mult: f64, t_us: i64) -> Option<f64> {
    view.sample_at(t_us, SampleMode::Prev)?
        .value
        .as_f64()
        .map(|v| v * mult)
}

/// Open a [`FieldView`] + its multiplier for a field.
fn open<'a>(snapshot: &'a StoreSnapshot, field: FieldId) -> Option<(FieldView<'a>, f64)> {
    let view = FieldView::new(snapshot, field).ok()?;
    Some((view, field_multiplier(snapshot, field)))
}

/// The effective (offset-applied) time range covering a vehicle's position
/// topic — the span the trajectory is resampled over.
fn position_topic_range(snapshot: &StoreSnapshot, pos: &PosMapping) -> Option<(i64, i64)> {
    let anchor = match pos {
        PosMapping::Ned { north, .. } => *north,
        PosMapping::Gps { lat, .. } => *lat,
    };
    let topic_id = snapshot.fields.get(anchor.index())?.topic;
    let store = snapshot.topic_store(topic_id)?;
    let source_id = snapshot.topic(topic_id)?.entry.source;
    let offset = snapshot
        .source(source_id)
        .map(|s| s.entry.offset_us)
        .unwrap_or(0);
    let range = store.time_range()?.offset(offset)?;
    Some((range.min_us, range.max_us))
}

/// Resolve the GPS reference origin (first valid fix) to
/// `(lat_rad, lon_rad, alt_m)`. Lat/lon are read as degrees.
fn resolve_gps_ref(
    snapshot: &StoreSnapshot,
    lat: FieldId,
    lon: FieldId,
    alt: FieldId,
    range: (i64, i64),
) -> Option<(f64, f64, f64)> {
    let (lat_v, lm) = open(snapshot, lat)?;
    let (lon_v, om) = open(snapshot, lon)?;
    let (alt_v, am) = open(snapshot, alt)?;
    // First sample with a finite, non-zero fix.
    let mut t = range.0;
    while t <= range.1 {
        if let (Some(la), Some(lo), Some(al)) = (
            lat_v
                .sample_at(t, SampleMode::Next)
                .and_then(|s| s.value.as_f64()),
            lon_v
                .sample_at(t, SampleMode::Next)
                .and_then(|s| s.value.as_f64()),
            alt_v
                .sample_at(t, SampleMode::Next)
                .and_then(|s| s.value.as_f64()),
        ) {
            let (la, lo, al) = (la * lm, lo * om, al * am);
            if la != 0.0 || lo != 0.0 {
                return Some((la.to_radians(), lo.to_radians(), al));
            }
        }
        t += 1_000_000; // step 1 s looking for the first fix
    }
    None
}

/// Render-space position of a vehicle at an effective time.
fn position_at(
    snapshot: &StoreSnapshot,
    pos: &PosMapping,
    gps_ref: Option<(f64, f64, f64)>,
    t_us: i64,
) -> Option<Vec3> {
    match pos {
        PosMapping::Ned {
            north,
            east,
            down,
            reference: _, // captured for geo-referencing; no effect on local render
        } => {
            let (nv, nm) = open(snapshot, *north)?;
            let (ev, em) = open(snapshot, *east)?;
            let (dv, dm) = open(snapshot, *down)?;
            let ned = Vec3::new(
                read_eng(&nv, nm, t_us)? as f32,
                read_eng(&ev, em, t_us)? as f32,
                read_eng(&dv, dm, t_us)? as f32,
            );
            Some(geo::ned_to_render(ned))
        }
        PosMapping::Gps { lat, lon, alt } => {
            let (rlat, rlon, ralt) = gps_ref?;
            let (lav, lam) = open(snapshot, *lat)?;
            let (lov, lom) = open(snapshot, *lon)?;
            let (alv, alm) = open(snapshot, *alt)?;
            let to_rad = 1f64.to_radians();
            let la = read_eng(&lav, lam, t_us)? * to_rad;
            let lo = read_eng(&lov, lom, t_us)? * to_rad;
            let al = read_eng(&alv, alm, t_us)?;
            let ned = geo::geodetic_to_ned(la, lo, al, rlat, rlon, ralt).as_vec3();
            Some(geo::ned_to_render(ned))
        }
    }
}

/// Render-space rotation of a vehicle at an effective time. Falls back to
/// level attitude (identity body→NED) when samples can't be read.
fn orientation_at(snapshot: &StoreSnapshot, ori: &OriMapping, t_us: i64) -> Mat3 {
    match ori {
        OriMapping::Static => geo::ned_to_render_mat3(),
        OriMapping::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => {
            let read = |f: FieldId| -> Option<f32> {
                let (v, m) = open(snapshot, f)?;
                read_eng(&v, m, t_us).map(|x| x as f32)
            };
            let conv = if *degrees {
                |d: f32| d.to_radians()
            } else {
                |r: f32| r
            };
            match (read(*roll), read(*pitch), read(*yaw)) {
                (Some(r), Some(p), Some(y)) => {
                    geo::body_to_render_rot(geo::euler_to_quat(conv(r), conv(p), conv(y)))
                }
                _ => geo::ned_to_render_mat3(),
            }
        }
        OriMapping::Quat { w, x, y, z } => {
            let read = |f: FieldId| -> Option<f32> {
                let (v, m) = open(snapshot, f)?;
                read_eng(&v, m, t_us).map(|x| x as f32)
            };
            match (read(*w), read(*x), read(*y), read(*z)) {
                (Some(qw), Some(qx), Some(qy), Some(qz)) => {
                    let q = Quat::from_xyzw(qx, qy, qz, qw);
                    if q.length_squared() > 1e-6 {
                        geo::body_to_render_rot(q.normalize())
                    } else {
                        geo::ned_to_render_mat3()
                    }
                }
                _ => geo::ned_to_render_mat3(),
            }
        }
    }
}

/// The vehicle's render-space pose at an effective playback time, or `None`
/// when its position can't be read (e.g. before the first sample).
pub fn pose_at(snapshot: &StoreSnapshot, config: &VehicleConfig, t_us: i64) -> Option<Pose> {
    let gps_ref = gps_reference(snapshot, config);
    let pos = position_at(snapshot, &config.pos, gps_ref, t_us)?;
    let rot = orientation_at(snapshot, &config.ori, t_us);
    Some(Pose { pos, rot })
}

fn gps_reference(snapshot: &StoreSnapshot, config: &VehicleConfig) -> Option<(f64, f64, f64)> {
    if let PosMapping::Gps { lat, lon, alt } = &config.pos {
        let range = position_topic_range(snapshot, &config.pos)?;
        resolve_gps_ref(snapshot, *lat, *lon, *alt, range)
    } else {
        None
    }
}

/// Maximum trajectory samples (resampled across the position range, v1).
const MAX_TRAJECTORY_POINTS: usize = 4000;

/// Resample a vehicle's full path into render-space points (with NaN points
/// marking gaps, so the line shader breaks there). Off-thread work (§19.6):
/// the caller runs this on a worker and feeds the result to the renderer.
pub fn build_trajectory(snapshot: &StoreSnapshot, config: &VehicleConfig) -> Vec<[f32; 3]> {
    let Some((t0, t1)) = position_topic_range(snapshot, &config.pos) else {
        return Vec::new();
    };
    let gps_ref = gps_reference(snapshot, config);
    let span = (t1 - t0).max(1);
    let steps = (span / 50_000).clamp(2, MAX_TRAJECTORY_POINTS as i64) as usize; // ~20 Hz cap
    let mut pts = Vec::with_capacity(steps);
    for i in 0..steps {
        let t = t0 + span * i as i64 / (steps as i64 - 1);
        match position_at(snapshot, &config.pos, gps_ref, t) {
            Some(p) => pts.push([p.x, p.y, p.z]),
            None => pts.push([f32::NAN, f32::NAN, f32::NAN]),
        }
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use arrow::array::Int64Array;

    /// A source with one POS topic carrying N/E/D float metres.
    fn ned_snapshot(
        times: Vec<i64>,
        n: Vec<f64>,
        e: Vec<f64>,
        d: Vec<f64>,
    ) -> (StoreSnapshot, [FieldId; 3]) {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("veh");
        let topic = id.add_topic(src, "POS").unwrap();
        let fnf = id.add_field(topic, "N").unwrap();
        let fef = id.add_field(topic, "E").unwrap();
        let fdf = id.add_field(topic, "D").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "POS",
                [
                    FieldSchema::new("N", DataType::Float64, Some("m"), 1.0).unwrap(),
                    FieldSchema::new("E", DataType::Float64, Some("m"), 1.0).unwrap(),
                    FieldSchema::new("D", DataType::Float64, Some("m"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Float64Array::from(n)),
            Arc::new(Float64Array::from(e)),
            Arc::new(Float64Array::from(d)),
        ];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();
        (snap, [fnf, fef, fdf])
    }

    fn ned_config(fields: [FieldId; 3]) -> VehicleConfig {
        VehicleConfig {
            source: SourceId(0),
            label: "v".into(),
            show: true,
            pos: PosMapping::Ned {
                north: fields[0],
                east: fields[1],
                down: fields[2],
                reference: None,
            },
            ori: OriMapping::Static,
            model: ModelKind::Cone,
            color: Color32::WHITE,
            path_color: Color32::WHITE,
            scale: 1.0,
        }
    }

    #[test]
    fn model_orientation_offsets_map_mesh_axes_to_body_axes() {
        // Every offset is a proper rotation mapping mesh-up (+Y) to body-up
        // (−Z, since body Z is down) and the authored nose to body +X
        // (forward): mesh −Z for Quad/Delta, mesh +X for the rest.
        for kind in [
            ModelKind::Quad,
            ModelKind::FixedWing,
            ModelKind::DeltaWing,
            ModelKind::Cone,
            ModelKind::CustomGlb("x.glb".into()),
        ] {
            let m = kind.orientation_offset();
            let label = kind.label();
            assert!((m.determinant() - 1.0).abs() < 1e-6, "{label}");
            assert!(
                (m * Vec3::Y - Vec3::NEG_Z).length() < 1e-6,
                "{label}: mesh up should map to body up (−Z)"
            );
            let nose = match kind {
                ModelKind::Quad | ModelKind::DeltaWing => Vec3::NEG_Z,
                _ => Vec3::X,
            };
            assert!(
                (m * nose - Vec3::X).length() < 1e-6,
                "{label}: nose should map to body +X"
            );
        }
    }

    #[test]
    fn level_attitude_renders_models_upright_facing_north() {
        // Static orientation = level attitude. Composed with the mesh offset
        // (as the draw path does: pose.rot × offset), mesh-up ends render-up
        // (+Y) and the nose ends render north (−Z) for every model kind.
        let (snap, f) = ned_snapshot(vec![0], vec![0.0], vec![0.0], vec![0.0]);
        let pose = pose_at(&snap, &ned_config(f), 0).unwrap();
        for kind in [ModelKind::Quad, ModelKind::FixedWing, ModelKind::Cone] {
            let rot = pose.rot * kind.orientation_offset();
            assert!(
                (rot * Vec3::Y - Vec3::Y).length() < 1e-5,
                "{}: mesh up should render up",
                kind.label()
            );
            let nose = match kind {
                ModelKind::Quad | ModelKind::DeltaWing => Vec3::NEG_Z,
                _ => Vec3::X,
            };
            assert!(
                (rot * nose - Vec3::NEG_Z).length() < 1e-5,
                "{}: nose should render north (−Z)",
                kind.label()
            );
        }
    }

    #[test]
    fn ned_pose_maps_to_render_space() {
        // N=10, E=20, D=-5 → render (E, −D, −N) = (20, 5, −10).
        let (snap, f) = ned_snapshot(vec![0], vec![10.0], vec![20.0], vec![-5.0]);
        let pose = pose_at(&snap, &ned_config(f), 0).unwrap();
        assert!(
            (pose.pos - Vec3::new(20.0, 5.0, -10.0)).length() < 1e-4,
            "{:?}",
            pose.pos
        );
    }

    #[test]
    fn trajectory_follows_the_path_in_render_space() {
        // A straight line north 0→100 m over 0..2 s.
        let (snap, f) = ned_snapshot(
            vec![0, 1_000_000, 2_000_000],
            vec![0.0, 50.0, 100.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
        );
        let traj = build_trajectory(&snap, &ned_config(f));
        assert!(traj.len() >= 2);
        // North maps to render −Z, so z should sweep 0 → −100.
        let first = traj.first().unwrap();
        let last = traj.last().unwrap();
        assert!(first[2].abs() < 1.0, "start near origin, got {first:?}");
        assert!(
            (last[2] + 100.0).abs() < 2.0,
            "end near −100 Z, got {last:?}"
        );
    }
}

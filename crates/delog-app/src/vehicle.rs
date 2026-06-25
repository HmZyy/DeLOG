//! Vehicle configuration + pose/trajectory resolution for the 3D view.
//!
//! `sample_at` returns the raw stored value, so the schema multiplier is
//! applied here to get engineering units.

use std::path::PathBuf;

use delog_core::field_view::{FieldView, SampleMode, array_row_as_f64};
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;
use egui::Color32;
use glam::{Mat3, Mat4, Quat, Vec3};

use crate::geo;

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

    /// Mesh→body correction: meshes are authored Y-up (glTF), body frame is
    /// X-forward/Z-down. Quad/Delta-wing have the nose along mesh −Z (extra
    /// −90° about up); the rest along mesh +X.
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoRef {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NedReference {
    Manual(GeoRef),
    /// Read from lat/lon/alt columns (first valid sample).
    Fields {
        lat: FieldId,
        lon: FieldId,
        alt: FieldId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PosMapping {
    /// Already-local NED metres. `reference` is captured for geo-referencing
    /// only; it does not move a single vehicle's local render.
    Ned {
        north: FieldId,
        east: FieldId,
        down: FieldId,
        reference: Option<NedReference>,
    },
    /// Geodetic → NED about the first valid fix.
    Gps {
        lat: FieldId,
        lon: FieldId,
        alt: FieldId,
        /// degE7 integers (×1e-7 → degrees).
        lat_lon_dege7: bool,
        /// millimetres (×1e-3 → metres).
        alt_mm: bool,
        /// Fixed vertical offset, metres, up-positive.
        alt_offset_m: f64,
    },
}

impl PosMapping {
    /// Extra GPS unit scales `(lat_lon, alt)` on top of the schema multiplier:
    /// `1e-7` for degE7 lat/lon, `1e-3` for mm alt.
    fn gps_unit_scales(lat_lon_dege7: bool, alt_mm: bool) -> (f64, f64) {
        (
            if lat_lon_dege7 { 1e-7 } else { 1.0 },
            if alt_mm { 1e-3 } else { 1.0 },
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum OriMapping {
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

#[derive(Clone, Debug, PartialEq)]
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

/// Render-space pose. `rot` already includes the mesh→body correction, so it
/// applies to mesh vertices as-is.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub pos: Vec3,
    pub rot: Mat3,
}

impl Pose {
    pub fn model_matrix(&self, scale: f32) -> Mat4 {
        Mat4::from_translation(self.pos)
            * Mat4::from_mat3(self.rot)
            * Mat4::from_scale(Vec3::splat(scale))
    }
}

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

fn read_eng(view: &FieldView<'_>, mult: f64, t_us: i64) -> Option<f64> {
    view.sample_at(t_us, SampleMode::Prev)?
        .value
        .as_f64()
        .map(|v| v * mult)
}

fn open<'a>(snapshot: &'a StoreSnapshot, field: FieldId) -> Option<(FieldView<'a>, f64)> {
    let view = FieldView::new(snapshot, field).ok()?;
    Some((view, field_multiplier(snapshot, field)))
}

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

/// Resolve the GPS reference origin (first valid fix) to `(lat_rad, lon_rad, alt_m)`.
fn resolve_gps_ref(
    snapshot: &StoreSnapshot,
    lat: FieldId,
    lon: FieldId,
    alt: FieldId,
    ll_scale: f64,
    alt_scale: f64,
    range: (i64, i64),
) -> Option<(f64, f64, f64)> {
    let (lat_v, lm) = open(snapshot, lat)?;
    let (lon_v, om) = open(snapshot, lon)?;
    let (alt_v, am) = open(snapshot, alt)?;
    // Skip null/zero fixes to find the first real one.
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
            let (la, lo, al) = (la * lm * ll_scale, lo * om * ll_scale, al * am * alt_scale);
            if la != 0.0 || lo != 0.0 {
                return Some((la.to_radians(), lo.to_radians(), al));
            }
        }
        t += 1_000_000; // 1 s step
    }
    None
}

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
            reference: _, // no effect on local render
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
        PosMapping::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => {
            let (rlat, rlon, ralt) = gps_ref?;
            let (ll_scale, alt_scale) = PosMapping::gps_unit_scales(*lat_lon_dege7, *alt_mm);
            let (lav, lam) = open(snapshot, *lat)?;
            let (lov, lom) = open(snapshot, *lon)?;
            let (alv, alm) = open(snapshot, *alt)?;
            let la = (read_eng(&lav, lam, t_us)? * ll_scale).to_radians();
            let lo = (read_eng(&lov, lom, t_us)? * ll_scale).to_radians();
            let al = read_eng(&alv, alm, t_us)? * alt_scale;
            let ned = geo::geodetic_to_ned(la, lo, al, rlat, rlon, ralt).as_vec3();
            Some(geo::ned_to_render(ned) + Vec3::new(0.0, *alt_offset_m as f32, 0.0))
        }
    }
}

fn read_f32(snapshot: &StoreSnapshot, field: FieldId, t_us: i64) -> Option<f32> {
    let (view, mult) = open(snapshot, field)?;
    read_eng(&view, mult, t_us).map(|x| x as f32)
}

/// Falls back to level attitude (identity body→NED) when samples can't be read.
fn orientation_at(snapshot: &StoreSnapshot, ori: &OriMapping, t_us: i64) -> Mat3 {
    let read = |f: FieldId| read_f32(snapshot, f, t_us);
    match ori {
        OriMapping::Static => geo::ned_to_render_mat3(),
        OriMapping::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => {
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
        OriMapping::Quat { w, x, y, z } => match (read(*w), read(*x), read(*y), read(*z)) {
            (Some(qw), Some(qx), Some(qy), Some(qz)) => {
                let q = Quat::from_xyzw(qx, qy, qz, qw);
                if q.length_squared() > 1e-6 {
                    geo::body_to_render_rot(q.normalize())
                } else {
                    geo::ned_to_render_mat3()
                }
            }
            _ => geo::ned_to_render_mat3(),
        },
    }
}

#[cfg(test)]
pub fn pose_at(snapshot: &StoreSnapshot, config: &VehicleConfig, t_us: i64) -> Option<Pose> {
    let gps_ref = gps_reference(snapshot, config);
    pose_at_with_ref(snapshot, config, gps_ref, t_us)
}

/// GPS reference is supplied by the caller so it can be resolved once and
/// reused across frames instead of re-scanning for the first fix per call.
/// Ignored for NED-mapped vehicles.
pub(crate) fn pose_at_with_ref(
    snapshot: &StoreSnapshot,
    config: &VehicleConfig,
    gps_ref: Option<(f64, f64, f64)>,
    t_us: i64,
) -> Option<Pose> {
    let pos = position_at(snapshot, &config.pos, gps_ref, t_us)?;
    let rot = orientation_at(snapshot, &config.ori, t_us) * config.model.orientation_offset();
    Some(Pose { pos, rot })
}

pub(crate) fn gps_reference(
    snapshot: &StoreSnapshot,
    config: &VehicleConfig,
) -> Option<(f64, f64, f64)> {
    if let PosMapping::Gps {
        lat,
        lon,
        alt,
        lat_lon_dege7,
        alt_mm,
        alt_offset_m: _,
    } = &config.pos
    {
        let (ll_scale, alt_scale) = PosMapping::gps_unit_scales(*lat_lon_dege7, *alt_mm);
        let range = position_topic_range(snapshot, &config.pos)?;
        resolve_gps_ref(snapshot, *lat, *lon, *alt, ll_scale, alt_scale, range)
    } else {
        None
    }
}

const MAX_TRAJECTORY_POINTS: usize = 4000;

struct PositionRows<'a> {
    store: &'a TopicStore,
    col_indices: [usize; 3],
    multipliers: [f64; 3],
}

fn position_row_source<'a>(
    snapshot: &'a StoreSnapshot,
    pos: &PosMapping,
) -> Option<PositionRows<'a>> {
    let fields = match pos {
        PosMapping::Ned {
            north, east, down, ..
        } => [*north, *east, *down],
        PosMapping::Gps { lat, lon, alt, .. } => [*lat, *lon, *alt],
    };
    let entries = fields.map(|field| snapshot.fields.get(field.index()).filter(|e| e.id == field));
    let [Some(a), Some(b), Some(c)] = entries else {
        return None;
    };
    if a.topic != b.topic || a.topic != c.topic {
        return None;
    }

    let store = snapshot.topic_store(a.topic)?;
    let names = [&a.name, &b.name, &c.name];
    let mut col_indices = [0; 3];
    let mut multipliers = [1.0; 3];
    for (idx, name) in names.into_iter().enumerate() {
        let col_index = store.schema.field_index(name)?;
        col_indices[idx] = col_index;
        multipliers[idx] = store.schema.field(col_index)?.multiplier;
    }

    Some(PositionRows {
        store,
        col_indices,
        multipliers,
    })
}

fn row_value(
    rows: &PositionRows<'_>,
    chunk: &delog_core::chunk::Chunk,
    col: usize,
    row: usize,
) -> Option<f64> {
    let value =
        array_row_as_f64(chunk.cols[rows.col_indices[col]].as_ref(), row) * rows.multipliers[col];
    value.is_finite().then_some(value)
}

fn position_from_row(
    pos: &PosMapping,
    rows: &PositionRows<'_>,
    gps_ref: Option<(f64, f64, f64)>,
    chunk: &delog_core::chunk::Chunk,
    row: usize,
) -> Option<Vec3> {
    match pos {
        PosMapping::Ned { .. } => {
            let ned = Vec3::new(
                row_value(rows, chunk, 0, row)? as f32,
                row_value(rows, chunk, 1, row)? as f32,
                row_value(rows, chunk, 2, row)? as f32,
            );
            Some(geo::ned_to_render(ned))
        }
        PosMapping::Gps {
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
            ..
        } => {
            let (rlat, rlon, ralt) = gps_ref?;
            let (ll_scale, alt_scale) = PosMapping::gps_unit_scales(*lat_lon_dege7, *alt_mm);
            let la = (row_value(rows, chunk, 0, row)? * ll_scale).to_radians();
            let lo = (row_value(rows, chunk, 1, row)? * ll_scale).to_radians();
            let al = row_value(rows, chunk, 2, row)? * alt_scale;
            let ned = geo::geodetic_to_ned(la, lo, al, rlat, rlon, ralt).as_vec3();
            Some(geo::ned_to_render(ned) + Vec3::new(0.0, *alt_offset_m as f32, 0.0))
        }
    }
}

fn build_trajectory_from_rows(
    snapshot: &StoreSnapshot,
    config: &VehicleConfig,
) -> Option<VehicleTrajectory> {
    let rows = position_row_source(snapshot, &config.pos)?;
    let total_rows = usize::try_from(rows.store.rows).ok()?;
    if total_rows == 0 {
        return Some(VehicleTrajectory::default());
    }

    let gps_ref = gps_reference(snapshot, config);
    // Full-resolution path in stored (append) order: no decimation, so vertices
    // never move between rebuilds (fixes the old jiggle) and the GPU upload only
    // writes the new tail. Canonical µs time rides alongside (`chunk.t`, not the
    // f32 cache) so the path can be clipped to the playhead at draw time.
    let mut points = Vec::with_capacity(total_rows);
    let mut times_us = Vec::with_capacity(total_rows);
    for chunk in rows.store.chunks.iter() {
        for row in 0..chunk.len() {
            times_us.push(chunk.t.value(row));
            match position_from_row(&config.pos, &rows, gps_ref, chunk, row) {
                Some(p) => points.push([p.x, p.y, p.z]),
                None => points.push([f32::NAN, f32::NAN, f32::NAN]),
            }
        }
    }

    Some(VehicleTrajectory { points, times_us })
}

fn build_trajectory_by_time(snapshot: &StoreSnapshot, config: &VehicleConfig) -> VehicleTrajectory {
    let Some((t0, t1)) = position_topic_range(snapshot, &config.pos) else {
        return VehicleTrajectory::default();
    };
    let gps_ref = gps_reference(snapshot, config);
    let span = (t1 - t0).max(1);
    let steps = (span / 50_000).clamp(2, MAX_TRAJECTORY_POINTS as i64) as usize; // ~20 Hz cap
    let mut points = Vec::with_capacity(steps);
    let mut times_us = Vec::with_capacity(steps);
    for i in 0..steps {
        let t = t0 + span * i as i64 / (steps as i64 - 1);
        times_us.push(t);
        match position_at(snapshot, &config.pos, gps_ref, t) {
            Some(p) => points.push([p.x, p.y, p.z]),
            None => points.push([f32::NAN, f32::NAN, f32::NAN]),
        }
    }
    VehicleTrajectory { points, times_us }
}

#[derive(Debug, Default, Clone)]
pub struct VehicleTrajectory {
    /// NaN = gap, so the line shader breaks there.
    pub points: Vec<[f32; 3]>,
    /// Canonical µs timestamp, 1:1 with `points`.
    pub times_us: Vec<i64>,
}

pub fn build_trajectory(snapshot: &StoreSnapshot, config: &VehicleConfig) -> VehicleTrajectory {
    build_trajectory_from_rows(snapshot, config)
        .unwrap_or_else(|| build_trajectory_by_time(snapshot, config))
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

    // Raw Lat/Lng/Alt columns, no schema multiplier: unit interpretation is up
    // to the vehicle config.
    fn gps_snapshot(
        times: Vec<i64>,
        lat: Vec<f64>,
        lon: Vec<f64>,
        alt: Vec<f64>,
    ) -> (StoreSnapshot, [FieldId; 3]) {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("veh");
        let topic = id.add_topic(src, "GPS").unwrap();
        let flat = id.add_field(topic, "Lat").unwrap();
        let flon = id.add_field(topic, "Lng").unwrap();
        let falt = id.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [
                    FieldSchema::new("Lat", DataType::Float64, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("Lng", DataType::Float64, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Float64Array::from(lat)),
            Arc::new(Float64Array::from(lon)),
            Arc::new(Float64Array::from(alt)),
        ];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();
        (snap, [flat, flon, falt])
    }

    #[test]
    fn gps_unit_scales_select_dege7_and_mm_factors() {
        assert_eq!(PosMapping::gps_unit_scales(false, false), (1.0, 1.0));
        assert_eq!(PosMapping::gps_unit_scales(true, true), (1e-7, 1e-3));
        assert_eq!(PosMapping::gps_unit_scales(true, false), (1e-7, 1.0));
    }

    #[test]
    fn gps_reference_applies_dege7_and_mm_unit_scales() {
        // ArduPilot-style raw integers: lat/lon in degE7, alt in mm.
        let (snap, [lat, lon, alt]) = gps_snapshot(
            vec![0],
            vec![473_977_418.0], // 47.3977418°
            vec![85_503_580.0],  //  8.5503580°
            vec![408_000.0],     // 408 m, stored as mm
        );
        let (ll, am) = PosMapping::gps_unit_scales(true, true);
        let (rlat, rlon, ralt) = resolve_gps_ref(&snap, lat, lon, alt, ll, am, (0, 0)).unwrap();
        assert!(
            (rlat.to_degrees() - 47.397_741_8).abs() < 1e-6,
            "{}",
            rlat.to_degrees()
        );
        assert!(
            (rlon.to_degrees() - 8.550_358_0).abs() < 1e-6,
            "{}",
            rlon.to_degrees()
        );
        assert!((ralt - 408.0).abs() < 1e-3, "{ralt}");
    }

    #[test]
    fn gps_reference_without_flags_uses_raw_degrees_and_metres() {
        let (snap, [lat, lon, alt]) =
            gps_snapshot(vec![0], vec![47.397_741_8], vec![8.550_358_0], vec![408.0]);
        let (ll, am) = PosMapping::gps_unit_scales(false, false);
        let (rlat, _, ralt) = resolve_gps_ref(&snap, lat, lon, alt, ll, am, (0, 0)).unwrap();
        assert!((rlat.to_degrees() - 47.397_741_8).abs() < 1e-6);
        assert!((ralt - 408.0).abs() < 1e-3);
    }

    fn gps_config(fields: [FieldId; 3], alt_offset_m: f64) -> VehicleConfig {
        VehicleConfig {
            source: SourceId(0),
            label: "v".into(),
            show: true,
            pos: PosMapping::Gps {
                lat: fields[0],
                lon: fields[1],
                alt: fields[2],
                lat_lon_dege7: false,
                alt_mm: false,
                alt_offset_m,
            },
            ori: OriMapping::Static,
            model: ModelKind::Cone,
            color: Color32::WHITE,
            path_color: Color32::WHITE,
            scale: 1.0,
        }
    }

    #[test]
    fn gps_alt_offset_raises_render_height_and_positive_is_up() {
        // A +100 m alt offset must raise render +Y: altitude is up-positive, in metres.
        let (snap, f) = gps_snapshot(vec![0], vec![47.0], vec![8.0], vec![400.0]);
        let pose = pose_at(&snap, &gps_config(f, 100.0), 0).unwrap();
        assert!(
            pose.pos.x.abs() < 1e-3 && pose.pos.z.abs() < 1e-3,
            "{:?}",
            pose.pos
        );
        assert!(
            (pose.pos.y - 100.0).abs() < 1e-3,
            "up offset, got {:?}",
            pose.pos
        );
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
        // Each offset is a proper rotation: mesh-up (+Y) → body-up (−Z), authored
        // nose → body +X (nose is mesh −Z for Quad/Delta, mesh +X for the rest).
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
        // Static = level attitude: mesh-up ends render-up (+Y), nose ends render north (−Z).
        let (snap, f) = ned_snapshot(vec![0], vec![0.0], vec![0.0], vec![0.0]);
        for kind in [ModelKind::Quad, ModelKind::FixedWing, ModelKind::Cone] {
            let config = VehicleConfig {
                model: kind.clone(),
                ..ned_config(f)
            };
            let rot = pose_at(&snap, &config, 0).unwrap().rot;
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
        let traj = build_trajectory(&snap, &ned_config(f)).points;
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

    #[test]
    fn trajectory_emits_one_timestamp_per_point_in_nondecreasing_order() {
        let (snap, f) = ned_snapshot(
            vec![0, 1_000_000, 2_000_000],
            vec![0.0, 50.0, 100.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
        );
        let traj = build_trajectory(&snap, &ned_config(f));
        assert_eq!(
            traj.points.len(),
            traj.times_us.len(),
            "1:1 point/time alignment"
        );
        assert_eq!(traj.times_us, vec![0, 1_000_000, 2_000_000]);
        assert!(
            traj.times_us.windows(2).all(|w| w[0] <= w[1]),
            "row-order path has non-decreasing timestamps",
        );
    }

    #[test]
    fn trajectory_partition_point_clips_prefix_at_a_mid_time() {
        let (snap, f) = ned_snapshot(
            vec![0, 1_000_000, 2_000_000],
            vec![0.0, 50.0, 100.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
        );
        let traj = build_trajectory(&snap, &ned_config(f));
        // Playhead exactly on the middle sample includes the first two points.
        let visible = traj.times_us.partition_point(|&t| t <= 1_000_000);
        assert_eq!(visible, 2);
        // Before the first sample: nothing flown yet.
        assert_eq!(traj.times_us.partition_point(|&t| t <= -1), 0);
        // At/after the last sample: the full path.
        assert_eq!(
            traj.times_us.partition_point(|&t| t <= 2_000_000),
            traj.points.len()
        );
    }

    #[test]
    fn trajectory_uses_position_rows_without_time_resampling_duplicates() {
        // A 10 Hz live position topic should contribute its actual rows once.
        // Resampling at a fixed time interval duplicates Prev samples and makes
        // long live paths increasingly expensive to rebuild as chunks accrue.
        let (snap, f) = ned_snapshot(
            vec![0, 100_000, 200_000],
            vec![0.0, 10.0, 20.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
        );
        let traj = build_trajectory(&snap, &ned_config(f)).points;
        assert_eq!(traj.len(), 3);
        assert!((traj[0][2] - 0.0).abs() < 1e-4, "{traj:?}");
        assert!((traj[1][2] + 10.0).abs() < 1e-4, "{traj:?}");
        assert!((traj[2][2] + 20.0).abs() < 1e-4, "{traj:?}");
    }
}

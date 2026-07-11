//! KML (Google Earth) export of vehicle trajectories: one `<Placemark>` with a
//! `<MultiGeometry>` of `<LineString>`s per vehicle (no timestamps).

use egui::Color32;

use delog_core::snapshot::StoreSnapshot;

use crate::vehicle::{self, GeodeticSample, PosMapping, VehicleConfig, VehicleTrajectory};

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// KML colors are `aabbggrr` hex — reversed byte order from RGBA.
fn kml_color(color: Color32) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        color.a(),
        color.b(),
        color.g(),
        color.r()
    )
}

/// Split a gap-marked point stream into contiguous segments, dropping gaps.
fn split_segments(points: Vec<Option<GeodeticSample>>) -> Vec<Vec<GeodeticSample>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    for p in points {
        match p {
            Some(p) => current.push(p),
            None if !current.is_empty() => segments.push(std::mem::take(&mut current)),
            None => {}
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Geodetic segments for one vehicle, or `None` when it can't be geolocated.
fn vehicle_geo_segments(
    snapshot: &StoreSnapshot,
    config: &VehicleConfig,
    trajectory: Option<&VehicleTrajectory>,
) -> Option<Vec<Vec<GeodeticSample>>> {
    let points: Vec<Option<GeodeticSample>> = match &config.pos {
        PosMapping::Gps { .. } => vehicle::geodetic_rows(snapshot, config)?,
        PosMapping::Ned { .. } => {
            let (rlat, rlon, ralt) = vehicle::ned_reference_origin(snapshot, config)?;
            let traj = trajectory?;
            traj.points
                .iter()
                .zip(&traj.times_us)
                .map(|(p, &t_us)| {
                    if p.iter().any(|c| !c.is_finite()) {
                        return None;
                    }
                    let ned = glam::DVec3::new(
                        -f64::from(p[2]),
                        f64::from(p[0]),
                        -f64::from(p[1]),
                    );
                    let (lat, lon, alt) = crate::geo::ned_to_geodetic(ned, rlat, rlon, ralt);
                    Some(GeodeticSample {
                        t_us,
                        lat_deg: lat.to_degrees(),
                        lon_deg: lon.to_degrees(),
                        alt_m: alt,
                    })
                })
                .collect()
        }
    };
    let segments = split_segments(points);
    (!segments.is_empty()).then_some(segments)
}

pub struct KmlExport {
    pub xml: String,
    pub exported: usize,
    /// Labels of visible vehicles that couldn't be geolocated.
    pub skipped: Vec<String>,
}

/// One styled `<Placemark>`/`<MultiGeometry>` per visible, geolocatable
/// vehicle. `trajectories` is parallel to `vehicles` (same as the 3D view).
pub fn build_kml(
    snapshot: &StoreSnapshot,
    vehicles: &[VehicleConfig],
    trajectories: &[VehicleTrajectory],
) -> KmlExport {
    use std::fmt::Write;

    let mut body = String::new();
    let mut exported = 0usize;
    let mut skipped = Vec::new();
    for (i, v) in vehicles.iter().enumerate() {
        if !v.show {
            continue;
        }
        let Some(segments) = vehicle_geo_segments(snapshot, v, trajectories.get(i)) else {
            skipped.push(v.label.clone());
            continue;
        };
        let _ = writeln!(
            body,
            "  <Style id=\"veh{i}\"><LineStyle><color>{}</color><width>2</width></LineStyle></Style>",
            kml_color(v.path_color)
        );
        let _ = writeln!(
            body,
            "  <Placemark>\n    <name>{}</name>\n    <styleUrl>#veh{i}</styleUrl>\n    <MultiGeometry>",
            xml_escape(&v.label)
        );
        for segment in &segments {
            body.push_str(
                "      <LineString>\n        <altitudeMode>absolute</altitudeMode>\n        <coordinates>\n",
            );
            for p in segment {
                let _ = writeln!(body, "          {},{},{}", p.lon_deg, p.lat_deg, p.alt_m);
            }
            body.push_str("        </coordinates>\n      </LineString>\n");
        }
        body.push_str("    </MultiGeometry>\n  </Placemark>\n");
        exported += 1;
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <kml xmlns=\"http://www.opengis.net/kml/2.2\">\n\
         <Document>\n  <name>DeLOG trajectories</name>\n{body}</Document>\n</kml>\n"
    );
    KmlExport {
        xml,
        exported,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{FieldId, IdentityRegistry, SourceId};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use crate::vehicle::{
        GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig,
        VehicleTrajectory,
    };

    use super::*;

    fn sample(t_us: i64) -> GeodeticSample {
        GeodeticSample {
            t_us,
            lat_deg: 47.5,
            lon_deg: 8.25,
            alt_m: 400.0,
        }
    }

    /// GPS topic in plain degrees/metres.
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
        let fields = vec![
            FieldSchema::new("Lat", DataType::Float64, Some("deg"), 1.0).unwrap(),
            FieldSchema::new("Lng", DataType::Float64, Some("deg"), 1.0).unwrap(),
            FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap(),
        ];
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Float64Array::from(lat)),
            Arc::new(Float64Array::from(lon)),
            Arc::new(Float64Array::from(alt)),
        ];
        let schema = Arc::new(TopicSchema::new("GPS", fields).unwrap());
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();
        (snap, [flat, flon, falt])
    }

    fn gps_vehicle(fields: [FieldId; 3]) -> VehicleConfig {
        VehicleConfig {
            source: SourceId(0),
            label: "Drone <1>".into(),
            show: true,
            pos: PosMapping::Gps {
                lat: fields[0],
                lon: fields[1],
                alt: fields[2],
                lat_lon_dege7: false,
                alt_mm: false,
                alt_offset_m: 0.0,
            },
            ori: OriMapping::Static,
            model: ModelKind::Cone,
            color: Color32::WHITE,
            path_color: Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0xff),
            scale: 1.0,
        }
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn xml_escape_replaces_markup_characters() {
        assert_eq!(xml_escape("<A & \"B's\">"), "&lt;A &amp; &quot;B&apos;s&quot;&gt;");
    }

    #[test]
    fn kml_color_is_aabbggrr() {
        let c = Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0xff);
        assert_eq!(kml_color(c), "ff332211");
    }

    #[test]
    fn split_segments_breaks_on_gaps_and_drops_empty_runs() {
        let pts = vec![None, Some(sample(0)), None, Some(sample(1)), Some(sample(2)), None];
        let segs = split_segments(pts);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].len(), 1);
        assert_eq!(segs[1].len(), 2);
        assert!(split_segments(vec![None, None]).is_empty());
    }

    #[test]
    fn build_kml_emits_styled_line_string_for_gps_vehicle() {
        let (snap, f) = gps_snapshot(
            vec![0, 1_000_000, 2_000_000],
            vec![47.5, 47.5, 47.5],
            vec![8.25, 8.25, 8.25],
            vec![400.0, 401.0, 402.0],
        );
        let traj = VehicleTrajectory::default();
        let out = build_kml(&snap, &[gps_vehicle(f)], std::slice::from_ref(&traj));
        assert_eq!(out.exported, 1);
        assert!(out.skipped.is_empty());
        assert!(out.xml.contains("<name>Drone &lt;1&gt;</name>"), "{}", out.xml);
        assert!(out.xml.contains("ff332211"));
        assert!(out.xml.contains("<MultiGeometry>"), "{}", out.xml);
        assert_eq!(count(&out.xml, "<LineString>"), 1);
        assert!(out.xml.contains("<altitudeMode>absolute</altitudeMode>"));
        assert_eq!(count(&out.xml, "8.25,47.5,400"), 1, "{}", out.xml);
        assert_eq!(count(&out.xml, "8.25,47.5,402"), 1);
        assert!(!out.xml.contains("gx:"), "{}", out.xml);
        assert!(!out.xml.contains("<when>"));
    }

    #[test]
    fn build_kml_splits_line_strings_at_gaps() {
        let (snap, f) = gps_snapshot(
            vec![0, 1_000_000, 2_000_000],
            vec![47.5, 0.0, 47.5],
            vec![8.25, 0.0, 8.25],
            vec![400.0, 0.0, 402.0],
        );
        let traj = VehicleTrajectory::default();
        let out = build_kml(&snap, &[gps_vehicle(f)], std::slice::from_ref(&traj));
        assert_eq!(count(&out.xml, "<LineString>"), 2, "{}", out.xml);
    }

    #[test]
    fn build_kml_skips_hidden_vehicles_silently() {
        let (snap, f) = gps_snapshot(vec![0], vec![47.5], vec![8.25], vec![400.0]);
        let mut v = gps_vehicle(f);
        v.show = false;
        let traj = VehicleTrajectory::default();
        let out = build_kml(&snap, &[v], std::slice::from_ref(&traj));
        assert_eq!(out.exported, 0);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn build_kml_reports_ned_vehicle_without_reference_as_skipped() {
        let (snap, f) = gps_snapshot(vec![0], vec![47.5], vec![8.25], vec![400.0]);
        let mut v = gps_vehicle(f);
        v.pos = PosMapping::Ned {
            north: f[0],
            east: f[1],
            down: f[2],
            reference: None,
        };
        let traj = VehicleTrajectory::default();
        let out = build_kml(&snap, &[v], std::slice::from_ref(&traj));
        assert_eq!(out.exported, 0);
        assert_eq!(out.skipped, vec!["Drone <1>".to_string()]);
    }

    #[test]
    fn build_kml_converts_ned_trajectory_via_manual_reference() {
        let (snap, f) = gps_snapshot(vec![0], vec![47.5], vec![8.25], vec![400.0]);
        let mut v = gps_vehicle(f);
        v.pos = PosMapping::Ned {
            north: f[0],
            east: f[1],
            down: f[2],
            reference: Some(NedReference::Manual(GeoRef {
                lat_deg: 47.5,
                lon_deg: 8.25,
                alt_m: 400.0,
            })),
        };
        let traj = VehicleTrajectory {
            points: vec![[0.0, 50.0, -100.0], [f32::NAN; 3], [0.0, 50.0, -100.0]],
            times_us: vec![0, 1_000_000, 2_000_000],
        };
        let out = build_kml(&snap, &[v], std::slice::from_ref(&traj));
        assert_eq!(out.exported, 1);
        assert_eq!(count(&out.xml, "<LineString>"), 2, "NaN splits the line");
        assert!(out.xml.contains("47.500"), "{}", out.xml);
    }
}

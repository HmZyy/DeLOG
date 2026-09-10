use delog_core::field_view::SampleMode;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use super::vehicle::{
    PosMapping, PositionRows, field_topic_range, open, position_row_source, read_eng, row_value,
};

const REFERENCE_SETTLE_RADIUS_M: f64 = 50.0;
const REFERENCE_SETTLE_WINDOW_US: i64 = 30_000_000;
const REFERENCE_SETTLE_MAX_SAMPLES: u32 = 8192;
const EARTH_RADIUS_M: f64 = 6_371_000.0;

struct FixScan {
    candidate: Option<(i64, f64, f64, f64)>,
    prev_alt_m: f64,
    scanned: u32,
    alt_step_m: Option<f64>,
}

impl FixScan {
    fn new(alt_step_m: Option<f64>) -> Self {
        Self {
            candidate: None,
            prev_alt_m: 0.0,
            scanned: 0,
            alt_step_m,
        }
    }

    fn push(&mut self, t_us: i64, lat_deg: f64, lon_deg: f64, alt_m: f64) -> bool {
        if !lat_deg.is_finite()
            || !lon_deg.is_finite()
            || !alt_m.is_finite()
            || lat_deg == 0.0
            || lon_deg == 0.0
        {
            return false;
        }
        let Some((start_us, start_lat, start_lon, _)) = self.candidate else {
            self.candidate = Some((t_us, lat_deg, lon_deg, alt_m));
            self.prev_alt_m = alt_m;
            return self.alt_step_m.is_none();
        };
        let stepped = self
            .alt_step_m
            .is_some_and(|step| (alt_m - self.prev_alt_m).abs() > step);
        self.prev_alt_m = alt_m;
        if stepped {
            self.candidate = Some((t_us, lat_deg, lon_deg, alt_m));
            self.scanned = 0;
            return false;
        }
        self.scanned += 1;
        self.scanned >= REFERENCE_SETTLE_MAX_SAMPLES
            || t_us.saturating_sub(start_us) > REFERENCE_SETTLE_WINDOW_US
            || horizontal_distance_m(start_lat, start_lon, lat_deg, lon_deg)
                > REFERENCE_SETTLE_RADIUS_M
    }

    fn finish(self) -> Option<(i64, (f64, f64, f64))> {
        self.candidate
            .map(|(t_us, lat, lon, alt)| (t_us, (lat.to_radians(), lon.to_radians(), alt)))
    }
}

fn horizontal_distance_m(lat0_deg: f64, lon0_deg: f64, lat_deg: f64, lon_deg: f64) -> f64 {
    let dlat = (lat_deg - lat0_deg).to_radians();
    let dlon = (lon_deg - lon0_deg).to_radians() * lat0_deg.to_radians().cos();
    dlat.hypot(dlon) * EARTH_RADIUS_M
}

pub(super) fn first_valid_gps_fix(
    snapshot: &StoreSnapshot,
    pos: &PosMapping,
    alt_step_m: Option<f64>,
) -> Option<(i64, (f64, f64, f64))> {
    let PosMapping::Gps {
        lat,
        lon,
        alt,
        lat_lon_dege7,
        alt_mm,
        ..
    } = pos
    else {
        return None;
    };
    let (ll_scale, alt_scale) = PosMapping::gps_unit_scales(*lat_lon_dege7, *alt_mm);
    match position_row_source(snapshot, pos) {
        Some(rows) => first_valid_fix_from_rows(&rows, ll_scale, alt_scale, alt_step_m),
        None => {
            first_valid_fix_by_time(snapshot, *lat, *lon, *alt, ll_scale, alt_scale, alt_step_m)
        }
    }
}

fn first_valid_fix_from_rows(
    rows: &PositionRows<'_>,
    ll_scale: f64,
    alt_scale: f64,
    alt_step_m: Option<f64>,
) -> Option<(i64, (f64, f64, f64))> {
    let mut scan = FixScan::new(alt_step_m);
    for chunk in rows.store.chunks.iter() {
        for row in 0..chunk.len() {
            let (Some(lat), Some(lon), Some(alt)) = (
                row_value(rows, chunk, 0, row),
                row_value(rows, chunk, 1, row),
                row_value(rows, chunk, 2, row),
            ) else {
                continue;
            };
            let Some(t_us) = chunk.t.value(row).checked_add(rows.offset_us) else {
                continue;
            };
            if scan.push(t_us, lat * ll_scale, lon * ll_scale, alt * alt_scale) {
                return scan.finish();
            }
        }
    }
    scan.finish()
}

pub(super) fn first_valid_fix_by_time(
    snapshot: &StoreSnapshot,
    lat: FieldId,
    lon: FieldId,
    alt: FieldId,
    ll_scale: f64,
    alt_scale: f64,
    alt_step_m: Option<f64>,
) -> Option<(i64, (f64, f64, f64))> {
    let (lat_v, lat_m) = open(snapshot, lat)?;
    let (lon_v, lon_m) = open(snapshot, lon)?;
    let (alt_v, alt_mult) = open(snapshot, alt)?;
    let mut t = [lat, lon, alt]
        .into_iter()
        .filter_map(|field| field_topic_range(snapshot, field).map(|range| range.0))
        .min()?;
    let mut scan = FixScan::new(alt_step_m);
    loop {
        if let (Some(la), Some(lo), Some(al)) = (
            read_eng(&lat_v, lat_m * ll_scale, t),
            read_eng(&lon_v, lon_m * ll_scale, t),
            read_eng(&alt_v, alt_mult * alt_scale, t),
        ) && scan.push(t, la, lo, al)
        {
            return scan.finish();
        }
        let Some(next) = t.checked_add(1) else {
            return scan.finish();
        };
        let Some(step) = [&lat_v, &lon_v, &alt_v]
            .into_iter()
            .filter_map(|view| view.sample_at(next, SampleMode::Next))
            .map(|sample| sample.effective_time_us)
            .min()
        else {
            return scan.finish();
        };
        t = step;
    }
}

use std::{cmp::Ordering, f64::consts::PI, ops::RangeInclusive};

use glam::{DMat4, DVec3, DVec4};

use super::provider::TileId;

const MAX_LAT_RAD: f64 = 85.051_128_78_f64 * PI / 180.0;
const TILE_LIMIT: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct VisibleSet {
    pub tiles: Vec<TileId>,
}

fn world_size(zoom: u8) -> u32 {
    1_u32.checked_shl(zoom.into()).unwrap_or(u32::MAX)
}

fn normalized_xy(lat_rad: f64, lon_rad: f64) -> (f64, f64) {
    let lat = lat_rad.clamp(-MAX_LAT_RAD, MAX_LAT_RAD);
    let lon = (lon_rad + PI).rem_euclid(2.0 * PI) - PI;
    let x = (lon + PI) / (2.0 * PI);
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0;
    (x, y.clamp(0.0, 1.0))
}

pub fn lat_lon_to_tile(lat_rad: f64, lon_rad: f64, zoom: u8) -> TileId {
    let size = world_size(zoom);
    let (x, y) = normalized_xy(lat_rad, lon_rad);
    TileId {
        zoom,
        x: ((x * size as f64).floor() as u32).min(size - 1),
        y: ((y * size as f64).floor() as u32).min(size - 1),
    }
}

/// Returns `[north, west, south, east]` in radians.
pub fn tile_bounds(tile: TileId) -> Option<[f64; 4]> {
    let size = world_size(tile.zoom);
    if tile.x >= size || tile.y >= size {
        return None;
    }
    let lon = |x: u32| x as f64 / size as f64 * 2.0 * PI - PI;
    let lat = |y: u32| (PI * (1.0 - 2.0 * y as f64 / size as f64)).sinh().atan();
    Some([lat(tile.y), lon(tile.x), lat(tile.y + 1), lon(tile.x + 1)])
}

fn ground_hit(inv_vp: DMat4, x: f64, y: f64) -> Option<DVec3> {
    let unproject = |z| {
        let point = inv_vp * DVec4::new(x, y, z, 1.0);
        (point.w.abs() > f64::EPSILON).then(|| point.truncate() / point.w)
    };
    let near = unproject(0.0)?;
    let far = unproject(1.0)?;
    let direction = far - near;
    if direction.y.abs() < 1e-10 {
        return None;
    }
    let t = -near.y / direction.y;
    (t >= 0.0).then(|| near + t * direction)
}

pub fn visible_tiles(
    inv_vp: DMat4,
    viewport_px: [u32; 2],
    anchor_rad: [f64; 2],
    zoom: RangeInclusive<u8>,
) -> VisibleSet {
    let min_zoom = *zoom.start();
    let max_zoom = (*zoom.end()).min(31);
    if min_zoom > max_zoom {
        return VisibleSet::default();
    }
    let samples = [
        (-1.0, -1.0),
        (-1.0, 1.0),
        (1.0, -1.0),
        (1.0, 1.0),
        (0.0, 0.0),
    ];
    let hits: Vec<_> = samples
        .into_iter()
        .filter_map(|(x, y)| ground_hit(inv_vp, x, y))
        .collect();
    let span = hits.iter().map(|p| p.x.hypot(p.z)).fold(0.0_f64, f64::max) * 2.0;
    let pixels = viewport_px[0].max(viewport_px[1]).max(1) as f64;
    let earth_circumference = 2.0 * PI * 6_378_137.0;
    let ideal = if span > 0.0 {
        (earth_circumference * pixels / (span * 256.0))
            .log2()
            .floor() as i32
    } else {
        max_zoom as i32
    };
    let selected_zoom = ideal.clamp(min_zoom as i32, max_zoom as i32) as u8;
    let center = lat_lon_to_tile(anchor_rad[0], anchor_rad[1], selected_zoom);
    let size = world_size(selected_zoom) as i64;
    let mut radius = 1_i64;
    if span > 0.0 {
        let metres_per_tile =
            earth_circumference * anchor_rad[0].cos().abs().max(0.01) / size as f64;
        radius = ((span / metres_per_tile / 2.0).ceil() as i64 + 1).max(1);
    }
    let mut candidates = Vec::new();
    for dy in -radius..=radius {
        let y = center.y as i64 + dy;
        if !(0..size).contains(&y) {
            continue;
        }
        for dx in -radius..=radius {
            candidates.push((
                dx * dx + dy * dy,
                TileId {
                    zoom: selected_zoom,
                    x: (center.x as i64 + dx).rem_euclid(size) as u32,
                    y: y as u32,
                },
            ));
        }
    }
    candidates.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.y.cmp(&b.1.y))
            .then_with(|| a.1.x.cmp(&b.1.x))
            .then(Ordering::Equal)
    });
    candidates.dedup_by_key(|(_, tile)| *tile);
    VisibleSet {
        tiles: candidates
            .into_iter()
            .take(TILE_LIMIT)
            .map(|(_, tile)| tile)
            .collect(),
    }
}

pub fn tile_corners_render(tile: TileId, anchor_rad_alt: [f64; 3]) -> [[f32; 3]; 4] {
    let Some([north, west, south, east]) = tile_bounds(tile) else {
        return [[0.0; 3]; 4];
    };
    [[north, west], [north, east], [south, east], [south, west]].map(|[lat, lon]| {
        let ned = crate::geo::geodetic_to_ned(
            lat,
            lon,
            0.0,
            anchor_rad_alt[0],
            anchor_rad_alt[1],
            anchor_rad_alt[2],
        );
        [ned.y as f32, -ned.z as f32, -ned.x as f32]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_LAT: f64 = 85.051_128_78_f64;

    #[test]
    fn latitude_is_clamped_to_web_mercator_limits() {
        assert_eq!(
            lat_lon_to_tile(MAX_LAT.to_radians() + 1.0, 0.0, 4),
            lat_lon_to_tile(MAX_LAT.to_radians(), 0.0, 4)
        );
        assert_eq!(
            lat_lon_to_tile(-MAX_LAT.to_radians() - 1.0, 0.0, 4),
            lat_lon_to_tile(-MAX_LAT.to_radians(), 0.0, 4)
        );
    }

    #[test]
    fn longitude_wraps_across_antimeridian() {
        assert_eq!(
            lat_lon_to_tile(0.0, 190_f64.to_radians(), 3),
            lat_lon_to_tile(0.0, -170_f64.to_radians(), 3)
        );
    }

    #[test]
    fn invalid_tile_y_is_rejected() {
        assert!(
            tile_bounds(TileId {
                zoom: 2,
                x: 0,
                y: 4
            })
            .is_none()
        );
    }

    #[test]
    fn zoom_one_bounds_are_known() {
        let [north, west, south, east] = tile_bounds(TileId {
            zoom: 1,
            x: 0,
            y: 0,
        })
        .unwrap();
        assert!((north.to_degrees() - MAX_LAT).abs() < 1e-8);
        assert!((west.to_degrees() + 180.0).abs() < 1e-10);
        assert!(south.abs() < 1e-12);
        assert!(east.abs() < 1e-12);
    }

    #[test]
    fn horizon_selection_is_bounded() {
        let set = visible_tiles(glam::DMat4::IDENTITY, [3840, 2160], [0.0, 0.0], 0..=23);
        assert!(set.tiles.len() <= 256);
    }

    #[test]
    fn tile_corner_at_anchor_maps_near_render_origin() {
        let tile = TileId {
            zoom: 1,
            x: 1,
            y: 1,
        };
        let corners = tile_corners_render(tile, [0.0, 0.0, 0.0]);
        assert!(
            corners
                .iter()
                .any(|corner| glam::Vec3::from_array(*corner).length() < 0.01)
        );
    }
}

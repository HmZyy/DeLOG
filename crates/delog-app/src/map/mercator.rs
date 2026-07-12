use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet},
    f64::consts::PI,
    ops::RangeInclusive,
};

use glam::{DMat4, DVec3, DVec4};

use super::provider::TileId;

const MAX_LAT_RAD: f64 = 85.051_128_78_f64 * PI / 180.0;
const TILE_LIMIT: usize = 256;
const SAMPLE_COLS: usize = 9;
const SAMPLE_ROWS: usize = 17;

#[derive(Clone, Copy, Debug)]
struct ScreenHit {
    col: usize,
    row: usize,
    world: DVec3,
}

#[derive(Clone, Debug, Default)]
pub struct VisibleSet {
    pub tiles: Vec<TileId>,
}

fn world_size(zoom: u8) -> u32 {
    assert!(zoom <= 31, "Web Mercator zoom must be at most 31");
    1_u32 << zoom
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
    if tile.zoom > 31 {
        return None;
    }
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

fn sample_ground(inv_vp: DMat4) -> Vec<Option<ScreenHit>> {
    (0..SAMPLE_ROWS)
        .flat_map(|row| {
            (0..SAMPLE_COLS).map(move |col| {
                let x = -1.0 + 2.0 * col as f64 / (SAMPLE_COLS - 1) as f64;
                let y = -1.0 + 2.0 * row as f64 / (SAMPLE_ROWS - 1) as f64;
                ground_hit(inv_vp, x, y).map(|world| ScreenHit { col, row, world })
            })
        })
        .collect()
}

fn select_zoom(
    samples: &[Option<ScreenHit>],
    viewport_px: [u32; 2],
    anchor_rad: [f64; 2],
    min_zoom: u8,
    max_zoom: u8,
) -> u8 {
    let pixel_x = viewport_px[0] as f64 / (SAMPLE_COLS - 1) as f64;
    let pixel_y = viewport_px[1] as f64 / (SAMPLE_ROWS - 1) as f64;
    let mut values = Vec::new();
    for row in 0..SAMPLE_ROWS {
        for col in 0..SAMPLE_COLS {
            let Some(hit) = samples[row * SAMPLE_COLS + col] else {
                continue;
            };
            for (other, separation) in [
                (
                    (col + 1 < SAMPLE_COLS)
                        .then(|| samples[row * SAMPLE_COLS + col + 1])
                        .flatten(),
                    pixel_x,
                ),
                (
                    (row + 1 < SAMPLE_ROWS)
                        .then(|| samples[(row + 1) * SAMPLE_COLS + col])
                        .flatten(),
                    pixel_y,
                ),
            ] {
                if let Some(other) = other {
                    let metres_per_pixel = hit.world.distance(other.world) / separation;
                    if metres_per_pixel.is_finite() && metres_per_pixel > 0.0 {
                        values.push(metres_per_pixel);
                    }
                }
            }
        }
    }
    if values.is_empty() {
        return max_zoom;
    }
    values.sort_by(f64::total_cmp);
    let metres_per_pixel = values[values.len() / 4];
    let mut latitudes = samples
        .iter()
        .flatten()
        .map(|hit| {
            crate::geo::ned_to_geodetic(
                DVec3::new(-hit.world.z, hit.world.x, 0.0),
                anchor_rad[0],
                anchor_rad[1],
                0.0,
            )
            .0
            .clamp(-MAX_LAT_RAD, MAX_LAT_RAD)
        })
        .collect::<Vec<_>>();
    latitudes.sort_by(f64::total_cmp);
    let representative_latitude = latitudes[latitudes.len() / 2];
    let earth_circumference = 2.0 * PI * 6_378_137.0 * representative_latitude.cos();
    let ideal = (earth_circumference / (metres_per_pixel * 256.0))
        .log2()
        .floor() as i32;
    ideal.clamp(min_zoom as i32, max_zoom as i32) as u8
}

fn ranked_tiles(
    samples: &[Option<ScreenHit>],
    selected_zoom: u8,
    anchor_rad: [f64; 2],
) -> Vec<TileId> {
    let size = i64::from(world_size(selected_zoom));
    let anchor_tile = lat_lon_to_tile(anchor_rad[0], anchor_rad[1], selected_zoom);
    let mut coordinates = vec![None; samples.len()];
    let mut reference_x = None;
    for (index, sample) in samples.iter().enumerate() {
        let Some(hit) = sample else { continue };
        let (lat, lon, _) = crate::geo::ned_to_geodetic(
            DVec3::new(-hit.world.z, hit.world.x, 0.0),
            anchor_rad[0],
            anchor_rad[1],
            0.0,
        );
        let tile = lat_lon_to_tile(lat, lon, selected_zoom);
        let reference = *reference_x.get_or_insert(i64::from(tile.x));
        let x = reference + (i64::from(tile.x) - reference + size / 2).rem_euclid(size) - size / 2;
        coordinates[index] = Some((x, i64::from(tile.y)));
    }
    let Some(bottom_index) = samples
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| hit.map(|hit| (index, hit)))
        .min_by(|(_, a), (_, b)| {
            a.row.cmp(&b.row).then_with(|| {
                let center = (SAMPLE_COLS - 1) as i64;
                let da = (2 * a.col as i64 - center).abs();
                let db = (2 * b.col as i64 - center).abs();
                da.cmp(&db)
            })
        })
        .map(|(index, _)| index)
    else {
        return vec![anchor_tile];
    };
    let bottom = coordinates[bottom_index].unwrap();
    let mut priorities: HashMap<TileId, usize> = HashMap::new();
    let mut insert_rect = |min_x, max_x, min_y, max_y, priority| {
        for tile in nearest_tiles(selected_zoom, min_x, max_x, min_y, max_y) {
            priorities
                .entry(tile)
                .and_modify(|old| *old = (*old).max(priority))
                .or_insert(priority);
        }
    };
    for row in 0..SAMPLE_ROWS - 1 {
        for col in 0..SAMPLE_COLS - 1 {
            let indices = [
                row * SAMPLE_COLS + col,
                row * SAMPLE_COLS + col + 1,
                (row + 1) * SAMPLE_COLS + col,
                (row + 1) * SAMPLE_COLS + col + 1,
            ];
            let Some(corners) = indices
                .map(|index| coordinates[index])
                .into_iter()
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let min_x = corners.iter().map(|p| p.0).min().unwrap() - 1;
            let max_x = corners.iter().map(|p| p.0).max().unwrap() + 1;
            let min_y = (corners.iter().map(|p| p.1).min().unwrap() - 1).max(0);
            let max_y = (corners.iter().map(|p| p.1).max().unwrap() + 1).min(size - 1);
            insert_rect(min_x, max_x, min_y, max_y, SAMPLE_ROWS - 1 - row);
        }
    }
    for (index, hit) in samples.iter().enumerate() {
        if let (Some(hit), Some((x, y))) = (hit, coordinates[index]) {
            insert_rect(
                x - 1,
                x + 1,
                (y - 1).max(0),
                (y + 1).min(size - 1),
                SAMPLE_ROWS - hit.row,
            );
            insert_rect(x, x, y, y, SAMPLE_ROWS + 1);
        }
    }
    let mut ranked: Vec<_> = priorities.into_iter().collect();
    ranked.sort_by(|(a, pa), (b, pb)| {
        let distance = |tile: &TileId| {
            let dx = (i64::from(tile.x) - bottom.0 + size / 2).rem_euclid(size) - size / 2;
            let dy = i64::from(tile.y) - bottom.1;
            dx * dx + dy * dy
        };
        pb.cmp(pa)
            .then_with(|| distance(a).cmp(&distance(b)))
            .then_with(|| a.y.cmp(&b.y).then_with(|| a.x.cmp(&b.x)))
    });
    ranked
        .into_iter()
        .take(TILE_LIMIT)
        .map(|(tile, _)| tile)
        .collect()
}

pub fn visible_tiles(
    inv_vp: DMat4,
    viewport_px: [u32; 2],
    anchor_rad: [f64; 2],
    zoom: RangeInclusive<u8>,
) -> VisibleSet {
    if *zoom.start() > 31 || *zoom.end() > 31 {
        return VisibleSet::default();
    }
    let min_zoom = *zoom.start();
    let max_zoom = *zoom.end();
    if min_zoom > max_zoom {
        return VisibleSet::default();
    }
    let samples = sample_ground(inv_vp);
    let selected_zoom = select_zoom(&samples, viewport_px, anchor_rad, min_zoom, max_zoom);
    VisibleSet {
        tiles: ranked_tiles(&samples, selected_zoom, anchor_rad),
    }
}

fn nearest_tiles(zoom: u8, min_x: i64, max_x: i64, min_y: i64, max_y: i64) -> Vec<TileId> {
    let size = i64::from(world_size(zoom));
    let center_x = (min_x + max_x) / 2;
    let center_y = (min_y + max_y) / 2;
    let mut frontier = BinaryHeap::new();
    let mut visited = HashSet::new();
    let mut emitted = HashSet::with_capacity(TILE_LIMIT);
    let mut tiles = Vec::with_capacity(TILE_LIMIT);
    let key = |x: i64, y: i64| Reverse(rectangle_distance_key(min_x, max_x, min_y, max_y, x, y));
    frontier.push(key(center_x, center_y));
    visited.insert((center_x, center_y));

    while let Some(Reverse((_, y, x))) = frontier.pop() {
        let tile = TileId {
            zoom,
            x: x.rem_euclid(size) as u32,
            y: y as u32,
        };
        if emitted.insert(tile) {
            tiles.push(tile);
            if tiles.len() == TILE_LIMIT {
                break;
            }
        }
        for (next_x, next_y) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            if next_x >= min_x
                && next_x <= max_x
                && next_y >= min_y
                && next_y <= max_y
                && visited.insert((next_x, next_y))
            {
                frontier.push(key(next_x, next_y));
            }
        }
    }
    tiles
}

fn rectangle_distance_key(
    min_x: i64,
    max_x: i64,
    min_y: i64,
    max_y: i64,
    x: i64,
    y: i64,
) -> (i128, i64, i64) {
    // Doubled coordinates preserve exact distances from half-tile midpoints.
    // i128 keeps the full legal zoom-31 rectangle range overflow-free.
    let dx = 2 * i128::from(x) - (i128::from(min_x) + i128::from(max_x));
    let dy = 2 * i128::from(y) - (i128::from(min_y) + i128::from(max_y));
    (dx * dx + dy * dy, y, x)
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
        [ned.y as f32, 0.0, -ned.x as f32]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_LAT: f64 = 85.051_128_78_f64;

    fn inverse_view_projection(eye: DVec3, target: DVec3, aspect: f64) -> DMat4 {
        let projection = DMat4::perspective_rh(60_f64.to_radians(), aspect, 1.0, 1_000_000.0);
        let view = DMat4::look_at_rh(eye, target, DVec3::Y);
        (projection * view).inverse()
    }

    fn ground_tile(inv_vp: DMat4, ndc: [f64; 2], anchor: [f64; 2], zoom: u8) -> Option<TileId> {
        let hit = ground_hit(inv_vp, ndc[0], ndc[1])?;
        let (lat, lon, _) =
            crate::geo::ned_to_geodetic(DVec3::new(-hit.z, hit.x, 0.0), anchor[0], anchor[1], 0.0);
        Some(lat_lon_to_tile(lat, lon, zoom))
    }

    fn screen_hit_at_tile(col: usize, row: usize, tile: TileId) -> ScreenHit {
        let [north, west, south, east] = tile_bounds(tile).unwrap();
        let ned = crate::geo::geodetic_to_ned(
            (north + south) / 2.0,
            (west + east) / 2.0,
            0.0,
            0.0,
            0.0,
            0.0,
        );
        ScreenHit {
            col,
            row,
            world: DVec3::new(ned.y, 0.0, -ned.x),
        }
    }

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
    fn asymmetric_footprint_includes_intersected_ground_and_border() {
        let anchor = [0.0, 0.0];
        let inv_vp = inverse_view_projection(
            DVec3::new(500_000.0, 20_000.0, 10_000.0),
            DVec3::new(500_000.0, 0.0, 0.0),
            16.0 / 9.0,
        );
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 13..=13);
        assert!(!set.tiles.is_empty());
        let zoom = set.tiles[0].zoom;
        let size = i64::from(world_size(zoom));

        for (clip_x, clip_y) in [(-1.0, -1.0), (1.0, -1.0)] {
            let hit = ground_hit(inv_vp, clip_x, clip_y).expect("bottom corner hits ground");
            let (lat, lon, _) = crate::geo::ned_to_geodetic(
                DVec3::new(-hit.z, hit.x, 0.0),
                anchor[0],
                anchor[1],
                0.0,
            );
            let tile = lat_lon_to_tile(lat, lon, zoom);
            assert!(
                set.tiles.contains(&tile),
                "missing ground-hit tile {tile:?}"
            );
            let border_x =
                (i64::from(tile.x) + if clip_x < 0.0 { -1 } else { 1 }).rem_euclid(size) as u32;
            assert!(
                set.tiles.contains(&TileId {
                    x: border_x,
                    ..tile
                }),
                "missing one-tile footprint border"
            );
        }
    }

    #[test]
    fn near_horizon_selection_is_nonempty_unique_and_bounded() {
        let inv_vp = inverse_view_projection(
            DVec3::new(0.0, 2_000.0, 0.0),
            DVec3::new(0.0, 0.0, -200_000.0),
            16.0 / 9.0,
        );
        let set = visible_tiles(inv_vp, [3840, 2160], [0.0, 179.9_f64.to_radians()], 0..=0);
        assert!(!set.tiles.is_empty());
        assert!(set.tiles.len() <= TILE_LIMIT);
        let unique: std::collections::HashSet<_> = set.tiles.iter().copied().collect();
        assert_eq!(unique.len(), set.tiles.len());
    }

    #[test]
    fn nonzero_zoom_antimeridian_selection_wraps_both_edges() {
        let zoom = 10;
        let inv_vp = inverse_view_projection(
            DVec3::new(0.0, 20_000.0, 10_000.0),
            DVec3::new(0.0, 0.0, 0.0),
            16.0 / 9.0,
        );
        let set = visible_tiles(
            inv_vp,
            [1920, 1080],
            [0.0, 179.99_f64.to_radians()],
            zoom..=zoom,
        );
        let max_x = world_size(zoom) - 1;
        assert!(set.tiles.iter().any(|tile| tile.x == 0), "{set:?}");
        assert!(set.tiles.iter().any(|tile| tile.x == max_x), "{set:?}");
    }

    #[test]
    fn partial_and_all_sky_samples_remain_bounded_and_nonempty() {
        let mut partial = vec![None; SAMPLE_COLS * SAMPLE_ROWS];
        for (col, row, world) in [
            (0, 0, DVec3::ZERO),
            (1, 0, DVec3::new(100.0, 0.0, 0.0)),
            (0, 1, DVec3::new(0.0, 0.0, -100.0)),
        ] {
            partial[row * SAMPLE_COLS + col] = Some(ScreenHit { col, row, world });
        }
        let partial_tiles = ranked_tiles(&partial, 12, [0.0, 0.0]);
        assert!(!partial_tiles.is_empty());
        assert!(partial_tiles.len() <= 27);

        let all_sky = vec![None; SAMPLE_COLS * SAMPLE_ROWS];
        assert_eq!(
            ranked_tiles(&all_sky, 12, [0.0, 0.0]),
            vec![lat_lon_to_tile(0.0, 0.0, 12)]
        );
    }

    #[test]
    fn horizontal_view_prioritizes_detailed_lower_screen_ground() {
        let anchor = [0.0, 0.0];
        let inv_vp = inverse_view_projection(
            DVec3::new(0.0, 10_000.0, 0.0),
            DVec3::new(0.0, 10_000.0, -900_000.0),
            32.0 / 9.0,
        );
        let set = visible_tiles(inv_vp, [15360, 4320], anchor, 0..=19);
        let zoom = set.tiles.first().expect("horizontal ground selection").zoom;

        assert!(zoom >= 11, "near-horizon outliers degraded zoom to {zoom}");
        for ndc in [
            [-1.0, -1.0],
            [0.0, -1.0],
            [1.0, -1.0],
            [-0.5, -0.5],
            [0.5, -0.5],
        ] {
            let tile = ground_tile(inv_vp, ndc, anchor, zoom).expect("lower ray hits ground");
            assert!(
                set.tiles.contains(&tile),
                "missing visible lower-screen tile {tile:?} at {ndc:?}"
            );
        }
        assert!(
            ground_hit(inv_vp, 0.0, 0.5).is_none(),
            "sky ray must remain sky"
        );
        assert!(set.tiles.len() <= TILE_LIMIT);
        assert_eq!(
            set.tiles.iter().copied().collect::<HashSet<_>>().len(),
            set.tiles.len()
        );
    }

    #[test]
    fn high_latitude_uses_local_web_mercator_scale_for_zoom() {
        let inv_vp = inverse_view_projection(
            DVec3::new(0.0, 10_000.0, 0.0),
            DVec3::new(0.0, 0.0, -20_000.0),
            16.0 / 9.0,
        );
        let equator = visible_tiles(inv_vp, [1920, 1080], [0.0, 0.0], 0..=19)
            .tiles
            .first()
            .unwrap()
            .zoom;
        let high_latitude = visible_tiles(inv_vp, [1920, 1080], [70_f64.to_radians(), 0.0], 0..=19)
            .tiles
            .first()
            .unwrap()
            .zoom;

        assert!(high_latitude < equator, "{high_latitude} vs {equator}");
    }

    #[test]
    fn cap_pressure_preserves_foreground_cell_interior() {
        let zoom = 19;
        let samples = (0..SAMPLE_ROWS)
            .flat_map(|row| {
                (0..SAMPLE_COLS).map(move |col| {
                    Some(screen_hit_at_tile(
                        col,
                        row,
                        TileId {
                            zoom,
                            x: 262_000 + col as u32 * 10,
                            y: 262_000 + row as u32 * 10,
                        },
                    ))
                })
            })
            .collect::<Vec<_>>();

        let tiles = ranked_tiles(&samples, zoom, [0.0, 0.0]);
        let foreground_interior = TileId {
            zoom,
            x: 262_042,
            y: 262_002,
        };

        assert_eq!(tiles.len(), TILE_LIMIT);
        assert!(
            tiles.contains(&foreground_interior),
            "sample borders exhausted the cap before {foreground_interior:?}"
        );
    }

    #[test]
    fn truncated_selection_contains_the_256_nearest_tiles() {
        let tiles = nearest_tiles(10, 0, 39, 0, 39);
        let mut expected: Vec<_> = (0_i64..=39)
            .flat_map(|y| {
                (0_i64..=39).map(move |x| ((2 * x - 39).pow(2) + (2 * y - 39).pow(2), y, x))
            })
            .collect();
        expected.sort_unstable();
        let expected: Vec<_> = expected
            .into_iter()
            .take(TILE_LIMIT)
            .map(|(_, y, x)| TileId {
                zoom: 10,
                x: x.rem_euclid(1 << 10) as u32,
                y: y as u32,
            })
            .collect();

        assert_eq!(tiles, expected);
    }

    #[test]
    fn zoom_31_rectangle_distance_key_does_not_overflow() {
        let size = i64::from(world_size(31));
        let key = rectangle_distance_key(-size / 2 - 1, size / 2, 0, size - 1, size / 2, size - 1);
        assert!(key.0 > i128::from(i64::MAX));
    }

    #[test]
    fn extreme_zoom_31_rectangle_selection_is_bounded() {
        let size = i64::from(world_size(31));
        let tiles = nearest_tiles(31, -size / 2 - 1, size / 2, 0, size - 1);
        assert_eq!(tiles.len(), TILE_LIMIT);
        assert_eq!(
            tiles.iter().copied().collect::<HashSet<_>>().len(),
            TILE_LIMIT
        );
        assert!(
            tiles
                .iter()
                .all(|tile| tile.zoom == 31 && tile.y < world_size(31))
        );
    }

    #[test]
    #[should_panic(expected = "Web Mercator zoom must be at most 31")]
    fn lat_lon_to_tile_rejects_zoom_32() {
        let _ = lat_lon_to_tile(0.0, 0.0, 32);
    }

    #[test]
    fn tile_bounds_rejects_zoom_32() {
        assert!(
            tile_bounds(TileId {
                zoom: 32,
                x: 0,
                y: 0
            })
            .is_none()
        );
    }

    #[test]
    fn visible_tiles_rejects_a_range_containing_zoom_32() {
        assert!(
            visible_tiles(DMat4::IDENTITY, [1, 1], [0.0, 0.0], 31..=32)
                .tiles
                .is_empty()
        );
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

    #[test]
    fn tile_corners_stay_on_render_ground_for_elevated_anchor() {
        let corners = tile_corners_render(
            TileId {
                zoom: 12,
                x: 2048,
                y: 2048,
            },
            [0.0, 0.0, 400.0],
        );
        assert!(corners.iter().all(|corner| corner[1] == 0.0), "{corners:?}");
    }
}

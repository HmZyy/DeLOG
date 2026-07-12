use std::{cmp::Ordering, collections::BinaryHeap, f64::consts::PI, ops::RangeInclusive};

use glam::{DMat4, DVec2, DVec3, DVec4};

use super::provider::TileId;

const MAX_LAT_RAD: f64 = 85.051_128_78_f64 * PI / 180.0;
const SAMPLE_COLS: usize = 9;
const SAMPLE_ROWS: usize = 17;
const EARTH_CIRCUMFERENCE_M: f64 = 2.0 * PI * 6_378_137.0;
const TEXELS_PER_PIXEL: f64 = 2.0;
const OBLIQUITY_LIMIT: f64 = 4.0;
const FOOTPRINT_PAD: f64 = 0.25;
const SEED_SPAN_LIMIT: i64 = 64;

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

fn sample_ground(inv_vp: DMat4) -> Vec<Option<DVec3>> {
    (0..SAMPLE_ROWS)
        .flat_map(|row| {
            (0..SAMPLE_COLS).map(move |col| {
                let x = -1.0 + 2.0 * col as f64 / (SAMPLE_COLS - 1) as f64;
                let y = -1.0 + 2.0 * row as f64 / (SAMPLE_ROWS - 1) as f64;
                ground_hit(inv_vp, x, y)
            })
        })
        .collect()
}

fn merc_lat(y: f64) -> f64 {
    (PI * (1.0 - 2.0 * y.clamp(0.0, 1.0))).sinh().atan()
}

fn unwrap_x(x: f64, reference: f64) -> f64 {
    reference + (x - reference + 0.5).rem_euclid(1.0) - 0.5
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    min: DVec2,
    max: DVec2,
}

impl Rect {
    fn intersects(&self, other: &Rect) -> bool {
        self.min.x <= other.max.x
            && other.min.x <= self.max.x
            && self.min.y <= other.max.y
            && other.min.y <= self.max.y
    }

    fn union(&self, other: &Rect) -> Rect {
        Rect {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

fn tile_rect(zoom: u8, x: i64, y: i64) -> Rect {
    let size = f64::from(world_size(zoom));
    Rect {
        min: DVec2::new(x as f64 / size, y as f64 / size),
        max: DVec2::new((x + 1) as f64 / size, (y + 1) as f64 / size),
    }
}

struct GroundView {
    bands: Vec<Rect>,
    bounds: Rect,
    eye: DVec2,
    eye_height_m: f64,
    radians_per_pixel: f64,
}

impl GroundView {
    fn new(inv_vp: DMat4, viewport_px: [u32; 2], anchor_rad: [f64; 2]) -> Option<Self> {
        let to_mercator = |world: DVec3, reference: &mut Option<f64>| {
            let (lat, lon, _) = crate::geo::ned_to_geodetic(
                DVec3::new(-world.z, world.x, 0.0),
                anchor_rad[0],
                anchor_rad[1],
                0.0,
            );
            let (x, y) = normalized_xy(lat, lon);
            let reference = *reference.get_or_insert(x);
            DVec2::new(unwrap_x(x, reference), y)
        };
        let mut reference = None;
        let mercs: Vec<Option<DVec2>> = sample_ground(inv_vp)
            .into_iter()
            .map(|hit| hit.map(|world| to_mercator(world, &mut reference)))
            .collect();
        reference?;

        let mut bands = Vec::new();
        for row in 0..SAMPLE_ROWS - 1 {
            let points = mercs[row * SAMPLE_COLS..(row + 2) * SAMPLE_COLS]
                .iter()
                .flatten();
            let band = points.fold(None::<Rect>, |band, point| {
                let point_rect = Rect {
                    min: *point,
                    max: *point,
                };
                Some(band.map_or(point_rect, |band| band.union(&point_rect)))
            });
            if let Some(band) = band {
                let pad = (band.max - band.min) * FOOTPRINT_PAD;
                bands.push(Rect {
                    min: DVec2::new(band.min.x - pad.x, (band.min.y - pad.y).max(0.0)),
                    max: DVec2::new(band.max.x + pad.x, (band.max.y + pad.y).min(1.0)),
                });
            }
        }
        let bounds = bands.iter().copied().reduce(|a, b| a.union(&b))?;

        let unproject = |x, y, z| {
            let point = inv_vp * DVec4::new(x, y, z, 1.0);
            (point.w.abs() > f64::EPSILON).then(|| point.truncate() / point.w)
        };
        let near = unproject(0.0, 0.0, 0.0)?;
        let far = unproject(0.0, 0.0, 1.0)?;
        let edge_near = unproject(0.0, 1.0, 0.0)?;
        let edge_far = unproject(0.0, 1.0, 1.0)?;
        let center_dir = (far - near).normalize();
        let edge_dir = (edge_far - edge_near).normalize();
        let half_fov = center_dir.dot(edge_dir).clamp(-1.0, 1.0).acos();
        let viewport_h = f64::from(viewport_px[1].max(1));
        let radians_per_pixel = (2.0 * half_fov / viewport_h).max(1e-9);

        let mut eye_reference = Some(bounds.min.x);
        Some(Self {
            bands,
            bounds,
            eye: to_mercator(near, &mut eye_reference),
            eye_height_m: near.y.abs(),
            radians_per_pixel,
        })
    }

    fn intersects_footprint(&self, zoom: u8, x: i64, y: i64) -> bool {
        let rect = tile_rect(zoom, x, y);
        self.bands.iter().any(|band| band.intersects(&rect))
    }

    fn node(&self, zoom: u8, x: i64, y: i64) -> Node {
        let rect = tile_rect(zoom, x, y);
        let nearest = self.eye.clamp(rect.min, rect.max);
        let metres_per_unit = EARTH_CIRCUMFERENCE_M * merc_lat(nearest.y).cos();
        let horizontal_m = (self.eye - nearest).length() * metres_per_unit;
        let distance_m = horizontal_m.hypot(self.eye_height_m).max(1.0);
        let pixel_m = distance_m * self.radians_per_pixel;
        let texel_m = metres_per_unit / (f64::from(world_size(zoom)) * 256.0);
        let obliquity = (distance_m / self.eye_height_m.max(1.0))
            .sqrt()
            .clamp(1.0, OBLIQUITY_LIMIT);
        Node {
            zoom,
            x,
            y,
            distance_m,
            error: texel_m / (TEXELS_PER_PIXEL * obliquity * pixel_m),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Node {
    zoom: u8,
    x: i64,
    y: i64,
    distance_m: f64,
    error: f64,
}

impl Node {
    fn tile_id(&self) -> TileId {
        let size = i64::from(world_size(self.zoom));
        TileId {
            zoom: self.zoom,
            x: self.x.rem_euclid(size) as u32,
            y: self.y as u32,
        }
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Node {}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        self.error
            .total_cmp(&other.error)
            .then_with(|| (other.zoom, other.y, other.x).cmp(&(self.zoom, self.y, self.x)))
    }
}

fn seed_nodes(view: &GroundView, min_zoom: u8, budget: usize) -> Vec<Node> {
    let size = i64::from(world_size(min_zoom));
    let tile_span = |low: f64, high: f64| {
        let low = (low * size as f64).floor() as i64;
        let high = (high * size as f64).floor() as i64;
        (low, high)
    };
    let (mut min_x, mut max_x) = tile_span(view.bounds.min.x, view.bounds.max.x);
    if max_x - min_x + 1 > size {
        (min_x, max_x) = (0, size - 1);
    }
    let (min_y, max_y) = tile_span(view.bounds.min.y, view.bounds.max.y);
    let (min_y, max_y) = (min_y.clamp(0, size - 1), max_y.clamp(0, size - 1));

    let eye_tile = |coordinate: f64| (coordinate * size as f64).floor() as i64;
    let clamp_span = |low: i64, high: i64, center: i64| {
        if high - low + 1 > 2 * SEED_SPAN_LIMIT {
            let center = center.clamp(low, high);
            (center - SEED_SPAN_LIMIT, center + SEED_SPAN_LIMIT)
        } else {
            (low, high)
        }
    };
    let (min_x, max_x) = clamp_span(min_x, max_x, eye_tile(view.eye.x));
    let (min_y, max_y) = clamp_span(min_y, max_y, eye_tile(view.eye.y));
    let (min_y, max_y) = (min_y.clamp(0, size - 1), max_y.clamp(0, size - 1));

    let mut seeds: Vec<Node> = (min_y..=max_y)
        .flat_map(|y| (min_x..=max_x).map(move |x| (x, y)))
        .filter(|&(x, y)| view.intersects_footprint(min_zoom, x, y))
        .map(|(x, y)| view.node(min_zoom, x, y))
        .collect();
    seeds.sort_by(order_near_to_far);
    seeds.truncate(budget);
    seeds
}

fn order_near_to_far(a: &Node, b: &Node) -> Ordering {
    a.distance_m
        .total_cmp(&b.distance_m)
        .then_with(|| (a.zoom, a.y, a.x).cmp(&(b.zoom, b.y, b.x)))
}

pub fn visible_tiles(
    inv_vp: DMat4,
    viewport_px: [u32; 2],
    anchor_rad: [f64; 2],
    zoom: RangeInclusive<u8>,
    budget: usize,
) -> VisibleSet {
    if *zoom.start() > 31 || *zoom.end() > 31 || zoom.is_empty() || budget == 0 {
        return VisibleSet::default();
    }
    let (min_zoom, max_zoom) = (*zoom.start(), *zoom.end());
    let Some(view) = GroundView::new(inv_vp, viewport_px, anchor_rad) else {
        return VisibleSet {
            tiles: vec![lat_lon_to_tile(anchor_rad[0], anchor_rad[1], min_zoom)],
        };
    };

    let mut heap: BinaryHeap<Node> = seed_nodes(&view, min_zoom, budget).into();
    let mut done: Vec<Node> = Vec::new();
    while let Some(node) = heap.pop() {
        if node.zoom >= max_zoom || node.error <= 1.0 {
            done.push(node);
            continue;
        }
        let children: Vec<Node> = [(0, 0), (1, 0), (0, 1), (1, 1)]
            .into_iter()
            .map(|(dx, dy)| (2 * node.x + dx, 2 * node.y + dy))
            .filter(|&(x, y)| view.intersects_footprint(node.zoom + 1, x, y))
            .map(|(x, y)| view.node(node.zoom + 1, x, y))
            .collect();
        if children.is_empty() || done.len() + heap.len() + children.len() > budget {
            done.push(node);
        } else {
            heap.extend(children);
        }
    }
    done.sort_by(order_near_to_far);
    VisibleSet {
        tiles: done.iter().map(Node::tile_id).collect(),
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
        [ned.y as f32, 0.0, -ned.x as f32]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_LAT: f64 = 85.051_128_78_f64;
    const BUDGET: usize = 128;

    fn inverse_view_projection(eye: DVec3, target: DVec3, aspect: f64) -> DMat4 {
        let projection = DMat4::perspective_rh(60_f64.to_radians(), aspect, 1.0, 1_000_000.0);
        let view = DMat4::look_at_rh(eye, target, DVec3::Y);
        (projection * view).inverse()
    }

    fn tilted_inv_vp(distance: f64, pitch: f64) -> DMat4 {
        inverse_view_projection(
            DVec3::new(0.0, distance * pitch.sin(), distance * pitch.cos()),
            DVec3::ZERO,
            16.0 / 9.0,
        )
    }

    fn ground_geodetic(inv_vp: DMat4, ndc: [f64; 2], anchor: [f64; 2]) -> Option<(f64, f64)> {
        let hit = ground_hit(inv_vp, ndc[0], ndc[1])?;
        let (lat, lon, _) =
            crate::geo::ned_to_geodetic(DVec3::new(-hit.z, hit.x, 0.0), anchor[0], anchor[1], 0.0);
        Some((lat, lon))
    }

    fn containing_tiles(set: &VisibleSet, lat: f64, lon: f64) -> Vec<TileId> {
        set.tiles
            .iter()
            .copied()
            .filter(|tile| lat_lon_to_tile(lat, lon, tile.zoom) == *tile)
            .collect()
    }

    fn containing_zoom(set: &VisibleSet, inv_vp: DMat4, ndc: [f64; 2], anchor: [f64; 2]) -> u8 {
        let (lat, lon) = ground_geodetic(inv_vp, ndc, anchor).expect("ray must hit ground");
        let tiles = containing_tiles(set, lat, lon);
        assert_eq!(
            tiles.len(),
            1,
            "point must be covered exactly once at {ndc:?}"
        );
        tiles[0].zoom
    }

    fn tiles_overlap(a: TileId, b: TileId) -> bool {
        let (coarse, fine) = if a.zoom <= b.zoom { (a, b) } else { (b, a) };
        let delta = u32::from(fine.zoom - coarse.zoom);
        fine.x >> delta == coarse.x && fine.y >> delta == coarse.y
    }

    fn texel_metres(zoom: u8, lat: f64) -> f64 {
        2.0 * PI * 6_378_137.0 * lat.cos() / (256.0 * f64::from(world_size(zoom)))
    }

    #[test]
    fn tilted_view_fits_budget_with_finer_foreground() {
        let anchor = [0.0, 0.0];
        let inv_vp = tilted_inv_vp(400.0, 0.35);
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);

        assert!(!set.tiles.is_empty());
        assert!(set.tiles.len() <= BUDGET, "{} tiles", set.tiles.len());

        let eye = DVec3::new(0.0, 400.0 * 0.35_f64.sin(), 400.0 * 0.35_f64.cos());
        let hit = ground_hit(inv_vp, 0.0, -1.0).unwrap();
        let pixel_m = 2.0 * hit.distance(eye) * 30_f64.to_radians().tan() / 1080.0;
        let near_zoom = containing_zoom(&set, inv_vp, [0.0, -1.0], anchor);
        assert!(
            texel_metres(near_zoom, 0.0) <= 2.05 * pixel_m,
            "foreground texel {} vs pixel {}",
            texel_metres(near_zoom, 0.0),
            pixel_m
        );

        let far_zoom = containing_zoom(&set, inv_vp, [0.0, 0.3], anchor);
        assert!(
            near_zoom > far_zoom,
            "near {near_zoom} must be finer than far {far_zoom}"
        );
    }

    #[test]
    fn tilted_selection_is_a_partition_covering_sampled_ground() {
        let anchor = [48.1_f64.to_radians(), 11.6_f64.to_radians()];
        let inv_vp = tilted_inv_vp(400.0, 0.35);
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);

        for (index, a) in set.tiles.iter().enumerate() {
            for b in &set.tiles[index + 1..] {
                assert!(!tiles_overlap(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
        for row in 0..SAMPLE_ROWS {
            for col in 0..SAMPLE_COLS {
                let x = -1.0 + 2.0 * col as f64 / (SAMPLE_COLS - 1) as f64;
                let y = -1.0 + 2.0 * row as f64 / (SAMPLE_ROWS - 1) as f64;
                let Some((lat, lon)) = ground_geodetic(inv_vp, [x, y], anchor) else {
                    continue;
                };
                assert_eq!(
                    containing_tiles(&set, lat, lon).len(),
                    1,
                    "ground sample at ({x}, {y}) must be covered exactly once"
                );
            }
        }
    }

    #[test]
    fn small_pitch_changes_keep_foreground_zoom_stable() {
        let anchor = [0.0, 0.0];
        let mut zooms = Vec::new();
        for step in 0..11 {
            let pitch = 0.30 + step as f64 * 0.01;
            let inv_vp = tilted_inv_vp(400.0, pitch);
            let set = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);
            assert!(set.tiles.len() <= BUDGET);
            zooms.push(containing_zoom(&set, inv_vp, [0.0, -1.0], anchor));
        }
        let (min, max) = (zooms.iter().min().unwrap(), zooms.iter().max().unwrap());
        assert!(max - min <= 1, "foreground zoom unstable: {zooms:?}");
    }

    #[test]
    fn straight_down_view_selects_uniform_legacy_zoom() {
        let anchor = [0.0, 0.0];
        let inv_vp =
            inverse_view_projection(DVec3::new(0.0, 400.0, 0.001), DVec3::ZERO, 16.0 / 9.0);
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);
        assert!(!set.tiles.is_empty());
        assert_eq!(containing_zoom(&set, inv_vp, [0.0, 0.0], anchor), 18);
        assert!(set.tiles.iter().all(|tile| tile.zoom >= 17), "{set:?}");
    }

    #[test]
    fn high_latitude_selects_coarser_zoom_than_equator() {
        let inv_vp =
            inverse_view_projection(DVec3::new(0.0, 400.0, 0.001), DVec3::ZERO, 16.0 / 9.0);
        let equator = visible_tiles(inv_vp, [1920, 1080], [0.0, 0.0], 1..=20, BUDGET);
        let polar = visible_tiles(
            inv_vp,
            [1920, 1080],
            [70_f64.to_radians(), 0.0],
            1..=20,
            BUDGET,
        );
        let max_zoom = |set: &VisibleSet| set.tiles.iter().map(|t| t.zoom).max().unwrap();
        assert!(max_zoom(&polar) < max_zoom(&equator));
    }

    #[test]
    fn sky_only_view_returns_anchor_tile_at_min_zoom() {
        let anchor = [0.0, 0.0];
        let inv_vp = inverse_view_projection(
            DVec3::new(0.0, 50.0, 0.0),
            DVec3::new(0.0, 120.0, -50.0),
            16.0 / 9.0,
        );
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 3..=20, BUDGET);
        assert_eq!(set.tiles, vec![lat_lon_to_tile(anchor[0], anchor[1], 3)]);
    }

    #[test]
    fn antimeridian_tilted_view_wraps_and_covers_ground() {
        let anchor = [0.0, 179.99_f64.to_radians()];
        let inv_vp = tilted_inv_vp(400.0, 0.5);
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);

        assert!(!set.tiles.is_empty());
        for tile in &set.tiles {
            assert!(tile.x < world_size(tile.zoom));
            assert!(tile.y < world_size(tile.zoom));
        }
        for ndc in [[-0.9, -0.9], [0.9, -0.9], [0.0, -0.5], [0.0, 0.0]] {
            let (lat, lon) = ground_geodetic(inv_vp, ndc, anchor).unwrap();
            assert_eq!(containing_tiles(&set, lat, lon).len(), 1, "{ndc:?}");
        }
    }

    #[test]
    fn selection_is_deterministic() {
        let anchor = [48.1_f64.to_radians(), 11.6_f64.to_radians()];
        let inv_vp = tilted_inv_vp(250.0, 0.4);
        let first = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);
        let second = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);
        assert_eq!(first.tiles, second.tiles);
    }

    #[test]
    fn priority_order_is_near_to_far() {
        let anchor = [0.0, 0.0];
        let inv_vp = tilted_inv_vp(400.0, 0.35);
        let set = visible_tiles(inv_vp, [1920, 1080], anchor, 1..=20, BUDGET);
        let first = set.tiles.first().unwrap();
        let last = set.tiles.last().unwrap();
        assert!(first.zoom >= last.zoom, "{first:?} vs {last:?}");
    }

    #[test]
    fn budget_of_one_returns_a_single_tile() {
        let inv_vp = tilted_inv_vp(400.0, 0.35);
        let set = visible_tiles(inv_vp, [1920, 1080], [0.0, 0.0], 1..=20, 1);
        assert_eq!(set.tiles.len(), 1);
    }

    #[test]
    fn zero_budget_returns_nothing() {
        let inv_vp = tilted_inv_vp(400.0, 0.35);
        let set = visible_tiles(inv_vp, [1920, 1080], [0.0, 0.0], 1..=20, 0);
        assert!(set.tiles.is_empty());
    }

    #[test]
    fn visible_tiles_rejects_a_range_containing_zoom_32() {
        assert!(
            visible_tiles(DMat4::IDENTITY, [1, 1], [0.0, 0.0], 31..=32, BUDGET)
                .tiles
                .is_empty()
        );
    }

    #[test]
    fn visible_tiles_rejects_an_inverted_zoom_range() {
        let inv_vp = tilted_inv_vp(400.0, 0.35);
        #[allow(clippy::reversed_empty_ranges)]
        let set = visible_tiles(inv_vp, [1920, 1080], [0.0, 0.0], 5..=4, BUDGET);
        assert!(set.tiles.is_empty());
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

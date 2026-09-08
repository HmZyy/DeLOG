// Infinite ground grid + colored world axes. A full-screen triangle is
// unprojected per-pixel into a world-space ray that intersects the y = 0 ground
// plane; grid lines are drawn with derivative-based (screen-constant) width and
// faded by distance, so the grid is "infinite" with no tessellated geometry.
// Per the `(E, −D, −N)` render mapping: X = East (red axis), Z = South (blue
// axis).

struct Grid {
    view_proj: mat4x4<f32>,
    // Clip → CAMERA-RELATIVE world (maps a clip point to `world − cam_pos`).
    // We unproject relative to the camera so every f32 operand stays small even
    // when the vehicle is kilometres from the render origin; `cam_pos` is added
    // back after the ground intersection. This keeps the world-anchored grid
    // from crawling while zooming/following a distant vehicle. See
    // `OrbitCamera::view_proj_and_inverse`.
    inv_vp_rel: mat4x4<f32>,
    cam_pos: vec4<f32>,  // xyz world, w = LOD blend on
    params: vec4<f32>,   // x = cell size, y = fade start, z = fade end, w = fog on
};

@group(0) @binding(0) var<uniform> g: Grid;

// Half-widths in pixels: grid lines stay a constant weight on screen, principal
// axes are drawn a little heavier so they read as reference lines.
const LINE_HALF_PX: f32 = 1.0;
const AXIS_HALF_PX: f32 = 1.5;

// A level's screen footprint `w` is measured in cell units, so `w = 0.5` is one
// cell per two pixels - the Nyquist limit for the line pattern. Point-sampling a
// level past that point does not draw a fine grid; it beats against the pixel
// lattice and produces moire that swims whenever the camera moves. So each level
// dissolves *before* it becomes unresolvable and the next coarser one carries
// the image. The window is per axis: under a grazing view the lines running away
// from the camera stay resolvable long after the ones crossing them have bunched
// up, and fading per axis keeps those "rails" while dropping the "ties".
const NYQUIST_FADE_START: f32 = 0.12; // ~8 px per cell: begin dissolving
const NYQUIST_FADE_END: f32 = 0.35;   // ~3 px per cell: fully gone

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Near/far ray points in CAMERA-RELATIVE world space (world − cam_pos).
    @location(0) near: vec3<f32>,
    @location(1) far: vec3<f32>,
};

// Unproject a clip point to camera-relative world (world − cam_pos).
fn unproject(ndc: vec3<f32>) -> vec3<f32> {
    let p = g.inv_vp_rel * vec4<f32>(ndc, 1.0);
    return p.xyz / p.w;
}

// Anti-aliased grid coverage for a single cell size: 1.0 on a line, 0.0 in the
// empty space between lines, feathered to ~1 px, and faded out entirely once the
// cell can no longer be resolved (see NYQUIST_FADE_START).
//
// `deriv_xz` is the ground footprint of one pixel in world units. It is passed
// in rather than taken with fwidth() here so the caller can derive it from
// camera-relative operands.
fn grid_line(coord_xz: vec2<f32>, deriv_xz: vec2<f32>, cell: f32) -> f32 {
    let c = coord_xz / cell;
    // Screen footprint of this level, in cell units.
    let w = max(deriv_xz / cell, vec2<f32>(1e-8));
    // 0 on a line, 0.5 midway between two.
    let dist = abs(fract(c - 0.5) - 0.5);
    let cov = vec2<f32>(1.0) - min(dist / (w * LINE_HALF_PX), vec2<f32>(1.0));
    let resolvable = vec2<f32>(1.0)
        - smoothstep(vec2<f32>(NYQUIST_FADE_START), vec2<f32>(NYQUIST_FADE_END), w);
    return max(cov.x * resolvable.x, cov.y * resolvable.y);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Oversized triangle covering the whole NDC square.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = corners[vi];
    var out: VsOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    // wgpu NDC depth is [0, 1]: z = 0 is the near plane, z = 1 the far plane.
    out.near = unproject(vec3<f32>(p, 0.0));
    out.far = unproject(vec3<f32>(p, 1.0));
    return out;
}

struct FsOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fs_main(in: VsOut) -> FsOut {
    // `in.near`/`in.far` are camera-relative (world − cam_pos), so the ray and
    // its direction are built from small f32 operands.
    let dir = in.far - in.near;
    // Intersect the ray with the ground plane y = 0. In camera-relative space
    // the ground sits at y = −cam_pos.y. Guard against rays parallel to it.
    if (abs(dir.y) < 1e-6) {
        discard;
    }
    let t = (-g.cam_pos.y - in.near.y) / dir.y;
    if (t < 0.0 || t > 1.0) {
        // Ground hit is behind the camera or beyond the far plane.
        discard;
    }
    // Ground hit relative to the camera (small), then lift to absolute world by
    // adding the clean `cam_pos` uniform. world.y is exactly 0 by construction.
    let rel = in.near + t * dir;
    let world = vec3<f32>(g.cam_pos.x + rel.x, 0.0, g.cam_pos.z + rel.z);

    // Ground footprint of one pixel. Taken from the camera-relative hit:
    // `cam_pos` is constant across the quad, so this is exactly fwidth(world.xz)
    // but evaluated on small operands - `world` itself loses the low bits of the
    // derivative once the camera is kilometres from the render origin, which
    // made both the line widths and the axis widths shimmer there.
    let deriv = max(vec2<f32>(fwidth(rel.x), fwidth(rel.z)), vec2<f32>(1e-8));

    // Grid coverage. With LOD blend on (cam_pos.w) the requested `cell` is a
    // continuous value; we draw the two bracketing power-of-ten grids and fade
    // the finer one in/out so the grid never *pops* between sizes as the camera
    // height changes, yet every line stays anchored to world coordinates. The
    // per-fragment Nyquist fade inside `grid_line` handles the rest: this global
    // blend picks the density near the camera, and each level then dissolves on
    // its own wherever it runs out of pixels.
    let cell = g.params.x;
    var grid_alpha: f32;
    if (g.cam_pos.w > 0.5) {
        let level = log(max(cell, 1e-6)) / log(10.0);
        let lo = pow(10.0, floor(level)); // finer grid
        let hi = lo * 10.0;               // coarser grid (10× lo)
        let blend = fract(level);         // 0 at `lo` → 1 toward `hi`
        let a_lo = grid_line(world.xz, deriv, lo) * (1.0 - blend); // fades out as we rise
        let a_hi = grid_line(world.xz, deriv, hi);                 // always present
        grid_alpha = max(a_lo, a_hi);
    } else {
        grid_alpha = grid_line(world.xz, deriv, cell);
    }

    // Distance fade from the camera (fog). Disabled (w == 0) keeps the grid
    // crisp all the way to the far plane. `rel` is already the hit measured from
    // the camera, so this needs no large-magnitude subtraction.
    let dist = length(rel);
    let fade = select(1.0, 1.0 - smoothstep(g.params.y, g.params.z, dist), g.params.w > 0.5);

    // Principal axes: world.z == 0 is the X (East) axis → red;
    // world.x == 0 is the Z (South) axis → blue. Each is a single line rather
    // than a periodic pattern, so it cannot alias the way the grid does and
    // carries its own coverage - that keeps the reference axes readable out
    // where the grid levels themselves have dissolved.
    let on_east = 1.0 - smoothstep(AXIS_HALF_PX - 1.0, AXIS_HALF_PX, abs(world.z) / deriv.y);
    let on_south = 1.0 - smoothstep(AXIS_HALF_PX - 1.0, AXIS_HALF_PX, abs(world.x) / deriv.x);

    // Base grid color (cool grey).
    var color = vec3<f32>(0.55, 0.58, 0.62);
    if (on_east > 0.0) {
        color = vec3<f32>(0.90, 0.20, 0.20); // East → red
    }
    if (on_south > 0.0) {
        color = vec3<f32>(0.20, 0.35, 0.95); // South → blue
    }

    let alpha = max(grid_alpha, max(on_east, on_south)) * fade;
    if (alpha < 0.02) {
        // Empty ground between lines: leave the background untouched and do
        // not write depth.
        discard;
    }

    // Write true depth so later meshes/trajectories occlude the grid.
    let clip = g.view_proj * vec4<f32>(world, 1.0);
    var out: FsOut;
    out.color = vec4<f32>(color, alpha);
    out.depth = clip.z / clip.w;
    return out;
}

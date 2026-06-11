// Infinite ground grid + colored world axes (PLAN.md §9.2 `grid3d`, §12.3,
// GPU-21). A full-screen triangle is unprojected per-pixel into a world-space
// ray that intersects the y = 0 ground plane; grid lines are drawn with
// derivative-based (screen-constant) width and faded by distance, so the grid
// is "infinite" with no tessellated geometry. Per the §12.3 render mapping
// `(E, −D, −N)`: X = East (red axis), Z = South (blue axis).

struct Grid {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,  // xyz world
    params: vec4<f32>,   // x = cell size, y = fade start, z = fade end
};

@group(0) @binding(0) var<uniform> g: Grid;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) near: vec3<f32>,
    @location(1) far: vec3<f32>,
};

fn unproject(ndc: vec3<f32>) -> vec3<f32> {
    let p = g.inv_view_proj * vec4<f32>(ndc, 1.0);
    return p.xyz / p.w;
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
    let dir = in.far - in.near;
    // Intersect the ray with the ground plane y = 0.
    // Guard against rays parallel to the ground.
    if (abs(dir.y) < 1e-6) {
        discard;
    }
    let t = -in.near.y / dir.y;
    if (t < 0.0 || t > 1.0) {
        // Ground hit is behind the camera or beyond the far plane.
        discard;
    }
    let world = in.near + t * dir;

    let cell = g.params.x;
    let coord = world.xz / cell;
    let deriv = fwidth(coord);

    // Anti-aliased grid lines: distance (in pixels) to the nearest line.
    let grid_uv = abs(fract(coord - 0.5) - 0.5) / deriv;
    let line = min(grid_uv.x, grid_uv.y);
    let grid_alpha = 1.0 - min(line, 1.0);

    // Distance fade from the camera.
    let dist = length(world - g.cam_pos.xyz);
    let fade = 1.0 - smoothstep(g.params.y, g.params.z, dist);

    // Base grid color (cool grey).
    var color = vec3<f32>(0.55, 0.58, 0.62);

    // Principal axes: world.z == 0 is the X (East) axis → red;
    // world.x == 0 is the Z (South) axis → blue. Use the derivative so the
    // axis highlight is a constant ~1.5 px wide regardless of zoom.
    let axis_x = abs(world.z) / deriv.y; // proximity to the East axis line
    let axis_z = abs(world.x) / deriv.x; // proximity to the South axis line
    if (axis_x < 1.5) {
        color = vec3<f32>(0.90, 0.20, 0.20); // East → red
    }
    if (axis_z < 1.5) {
        color = vec3<f32>(0.20, 0.35, 0.95); // South → blue
    }

    let alpha = grid_alpha * fade;
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

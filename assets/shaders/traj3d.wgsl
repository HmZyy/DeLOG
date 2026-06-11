// 3D trajectory polyline (PLAN.md §9.2 `traj3d`, §12.3, GPU-23). Vertex-pulled
// line-list: for N points we draw (N-1)*2 vertices, segment `vi/2` connecting
// point `seg` to `seg+1`. Either endpoint non-finite collapses the segment to a
// clipped (zero-area) line, so NaN acts as a gap marker exactly like the 2D
// line path (§9.4). Width is 1 px in v1; thick/joined lines are GPU-25.

const MAX_F32: f32 = 3.4028235e38;

struct Traj {
    view_proj: mat4x4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> pts: array<vec4<f32>>;
@group(0) @binding(1) var<uniform> u: Traj;

fn finite3(p: vec3<f32>) -> bool {
    return p.x == p.x && p.y == p.y && p.z == p.z
        && abs(p.x) <= MAX_F32 && abs(p.y) <= MAX_F32 && abs(p.z) <= MAX_F32;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let seg = vi / 2u;
    let end = vi % 2u;
    let p0 = pts[seg].xyz;
    let p1 = pts[seg + 1u].xyz;
    // A gap (or a corrupt sample) on either end drops the whole segment:
    // w = 0 yields a degenerate, clipped primitive that rasterizes nothing.
    if (!(finite3(p0) && finite3(p1))) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let p = select(p0, p1, end == 1u);
    return u.view_proj * vec4<f32>(p, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return u.color;
}

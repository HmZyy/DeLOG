// Vertex-pulled scatter pipeline (PLAN.md GPU-07).
//
// Each `[x,y]` sample emits one screen-space quad centred on the transformed
// sample. `u.view.z` carries the point size in pixels, sharing PlotUniform with
// the line and min/max pipelines.

struct PlotUniform {
    transform: vec4<f32>,
    view: vec4<f32>,
    color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> xy: array<vec2<f32>>;
@group(0) @binding(1) var<uniform> u: PlotUniform;

fn finite2(p: vec2<f32>) -> bool {
    let max_f32 = 3.402823e38;
    return p.x == p.x && p.y == p.y && abs(p.x) <= max_f32 && abs(p.y) <= max_f32;
}

fn data_to_clip(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        p.x * u.transform.x + u.transform.y,
        p.y * u.transform.z + u.transform.w,
    );
}

fn clip_to_screen(p: vec2<f32>, viewport: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        (p.x * 0.5 + 0.5) * viewport.x,
        (0.5 - p.y * 0.5) * viewport.y,
    );
}

fn screen_to_clip(p: vec2<f32>, viewport: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        p.x / viewport.x * 2.0 - 1.0,
        1.0 - p.y / viewport.y * 2.0,
    );
}

fn degenerate() -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    out.color = vec4<f32>(0.0);
    return out;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let sample = vi / 6u;
    let corner = vi % 6u;
    let p = xy[sample];

    if (!finite2(p)) {
        return degenerate();
    }

    let viewport = max(u.view.xy, vec2<f32>(1.0, 1.0));
    let center = clip_to_screen(data_to_clip(p), viewport);
    let half = max(u.view.z, 0.0) * 0.5;

    if (half <= 0.0) {
        return degenerate();
    }

    var ox = -half;
    if (corner == 2u || corner == 3u || corner == 5u) {
        ox = half;
    }
    var oy = -half;
    if (corner == 1u || corner == 4u || corner == 5u) {
        oy = half;
    }

    var out: VsOut;
    out.pos = vec4<f32>(screen_to_clip(center + vec2<f32>(ox, oy), viewport), 0.0, 1.0);
    out.color = u.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

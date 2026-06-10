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
    let seg = vi / 6u;
    let corner = vi % 6u;
    let p0 = xy[seg];
    let p1 = xy[seg + 1u];

    if (!(finite2(p0) && finite2(p1))) {
        return degenerate();
    }

    let viewport = max(u.view.xy, vec2<f32>(1.0, 1.0));
    let a = clip_to_screen(data_to_clip(p0), viewport);
    let b = clip_to_screen(data_to_clip(p1), viewport);
    let delta = b - a;
    let len = length(delta);
    let width_px = max(u.view.z, 0.0);

    if (len <= 0.0001 || width_px <= 0.0) {
        return degenerate();
    }

    let n = vec2<f32>(-delta.y, delta.x) * (width_px * 0.5 / len);

    var base = b;
    if (corner == 0u || corner == 1u || corner == 4u) {
        base = a;
    }

    var offset = -n;
    if (corner == 1u || corner == 4u || corner == 5u) {
        offset = n;
    }

    var out: VsOut;
    out.pos = vec4<f32>(screen_to_clip(base + offset, viewport), 0.0, 1.0);
    out.color = u.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

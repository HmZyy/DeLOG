struct PlotUniform {
    transform: vec4<f32>,
    view: vec4<f32>,
    color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Signed perpendicular distance from the segment centreline, in pixels,
    // and the line's half-width — together they drive the edge AA ramp.
    @location(1) dist: f32,
    @location(2) half_w: f32,
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
    out.dist = 0.0;
    out.half_w = 0.0;
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

    // Unit perpendicular, then offset to the feathered edge (half-width + AA).
    // The AA feather (px) is configurable via the uniform (view.w).
    let aa = max(u.view.w, 0.0);
    let perp = vec2<f32>(-delta.y, delta.x) / len;
    let half_w = width_px * 0.5;
    let off_mag = half_w + aa;

    var base = b;
    if (corner == 0u || corner == 1u || corner == 4u) {
        base = a;
    }

    var signed = -off_mag;
    if (corner == 1u || corner == 4u || corner == 5u) {
        signed = off_mag;
    }

    var out: VsOut;
    out.pos = vec4<f32>(screen_to_clip(base + perp * signed, viewport), 0.0, 1.0);
    out.color = u.color;
    out.dist = signed;
    out.half_w = half_w;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Coverage falls from 1 inside the core to 0 across a ~1px edge ramp.
    let cov = clamp(in.half_w + 0.5 - abs(in.dist), 0.0, 1.0);
    return vec4<f32>(in.color.rgb, in.color.a * cov);
}

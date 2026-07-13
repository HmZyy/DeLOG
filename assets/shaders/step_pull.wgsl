// Vertex-pulled stepped trace pipeline.
//
// Each adjacent `[x,y]` sample pair emits two thick screen-space segments:
// horizontal at the previous value, then vertical at the next timestamp.

struct PlotUniform {
    transform: vec4<f32>,
    view: vec4<f32>,
    color: vec4<f32>,
    // x: gap mode (0 connect, 1 cut, 2 dotted, 3 force-dash), y: x-units gap threshold (0 = off).
    gap: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Unit segment direction (screen space) and the dash flag, both constant
    // per quad; the dash phase is the fragment position projected on the
    // direction so the pattern stays anchored to the screen while zooming.
    @location(1) dir: vec2<f32>,
    @location(2) dash: f32,
};

const DASH_PERIOD_PX: f32 = 8.0;
const DASH_ON_PX: f32 = 4.0;

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
    out.dir = vec2<f32>(0.0, 0.0);
    out.dash = 0.0;
    return out;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let segment = vi / 6u;
    let pair = segment / 2u;
    let second_leg = (segment % 2u) == 1u;
    let corner = vi % 6u;

    let p0 = xy[pair];
    let p1 = xy[pair + 1u];
    if (!(finite2(p0) && finite2(p1))) {
        return degenerate();
    }

    let gap_mode = u32(u.gap.x);
    let is_gap = u.gap.y > 0.0 && (p1.x - p0.x) > u.gap.y;
    if (gap_mode == 1u && is_gap) {
        return degenerate();
    }
    var dash = 0.0;
    if (gap_mode == 3u) {
        dash = 1.0;
    }

    let mid = vec2<f32>(p1.x, p0.y);
    var a_data = p0;
    var b_data = mid;
    if (second_leg) {
        a_data = mid;
        b_data = p1;
    }
    if (gap_mode == 2u && is_gap) {
        // A dotted gap replaces the two step legs with one straight dashed
        // bridge (drawn as the first leg; the second collapses).
        if (second_leg) {
            return degenerate();
        }
        a_data = p0;
        b_data = p1;
        dash = 1.0;
    }

    let viewport = max(u.view.xy, vec2<f32>(1.0, 1.0));
    let a = clip_to_screen(data_to_clip(a_data), viewport);
    let b = clip_to_screen(data_to_clip(b_data), viewport);
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
    out.dir = delta / len;
    out.dash = dash;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let phase = dot(in.pos.xy, in.dir) / DASH_PERIOD_PX;
    if (in.dash > 0.5 && fract(phase) >= DASH_ON_PX / DASH_PERIOD_PX) {
        return vec4<f32>(0.0);
    }
    return in.color;
}

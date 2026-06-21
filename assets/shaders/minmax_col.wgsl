// Decimated min/max column pipeline.
//
// For zoomed-out views the plot asks the pyramid for per-pixel-column (min,max)
// and draws one vertical span per column. Columns are packed as [x, min, max]
// f32 triples; x is in cache seconds, min/max in data-y. The plot transform
// (shared with line_pull) maps them to clip space. A column whose span is
// thinner than the line width is expanded to that width so flat signals stay
// visible — min/max decimation never hides a transient.

struct PlotUniform {
    transform: vec4<f32>,
    view: vec4<f32>,
    color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Fragment screen-y and the column's core top/bottom (pixels) for the
    // top/bottom AA ramp. Left/right edges stay hard so columns tile seamlessly.
    @location(1) y_px: f32,
    @location(2) core_top: f32,
    @location(3) core_bot: f32,
};

@group(0) @binding(0) var<storage, read> cols: array<f32>;
@group(0) @binding(1) var<uniform> u: PlotUniform;

fn finite1(v: f32) -> bool {
    let max_f32 = 3.402823e38;
    return v == v && abs(v) <= max_f32;
}

fn data_to_clip(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        p.x * u.transform.x + u.transform.y,
        p.y * u.transform.z + u.transform.w,
    );
}

fn clip_to_screen(p: vec2<f32>, viewport: vec2<f32>) -> vec2<f32> {
    return vec2<f32>((p.x * 0.5 + 0.5) * viewport.x, (0.5 - p.y * 0.5) * viewport.y);
}

fn screen_to_clip(p: vec2<f32>, viewport: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(p.x / viewport.x * 2.0 - 1.0, 1.0 - p.y / viewport.y * 2.0);
}

fn degenerate() -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    out.color = vec4<f32>(0.0);
    out.y_px = 0.0;
    out.core_top = 0.0;
    out.core_bot = 0.0;
    return out;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let col = vi / 6u;
    let corner = vi % 6u;
    let base = col * 3u;
    let x = cols[base];
    let y_min = cols[base + 1u];
    let y_max = cols[base + 2u];

    if (!(finite1(x) && finite1(y_min) && finite1(y_max))) {
        return degenerate();
    }

    let viewport = max(u.view.xy, vec2<f32>(1.0, 1.0));
    let top = clip_to_screen(data_to_clip(vec2<f32>(x, y_max)), viewport);
    let bot = clip_to_screen(data_to_clip(vec2<f32>(x, y_min)), viewport);
    let width = max(u.view.z, 1.0);
    let half_w = width * 0.5;

    // Expand a sub-width span so flat columns remain at least `width` tall.
    var top_y = min(top.y, bot.y);
    var bot_y = max(top.y, bot.y);
    if (bot_y - top_y < width) {
        let mid = (top_y + bot_y) * 0.5;
        top_y = mid - half_w;
        bot_y = mid + half_w;
    }

    let left = top.x - half_w;
    let right = top.x + half_w;
    var px = left;
    if (corner == 2u || corner == 3u || corner == 5u) {
        px = right;
    }
    // Grow the drawn span by the feather (configurable via view.w); the core
    // edges drive the AA ramp.
    let aa = max(u.view.w, 0.0);
    var py = top_y - aa;
    if (corner == 1u || corner == 4u || corner == 5u) {
        py = bot_y + aa;
    }

    var out: VsOut;
    out.pos = vec4<f32>(screen_to_clip(vec2<f32>(px, py), viewport), 0.0, 1.0);
    out.color = u.color;
    out.y_px = py;
    out.core_top = top_y;
    out.core_bot = bot_y;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Top/bottom coverage ramp; left/right are hard (columns tile).
    let cov = clamp(min(in.y_px - in.core_top, in.core_bot - in.y_px) + 0.5, 0.0, 1.0);
    return vec4<f32>(in.color.rgb, in.color.a * cov);
}

struct Sky {
    inv_vp_rel: mat4x4<f32>,
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    ground: vec4<f32>,
};

@group(0) @binding(0) var<uniform> s: Sky;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) far: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = corners[vi];
    let unprojected = s.inv_vp_rel * vec4<f32>(p, 1.0, 1.0);

    var out: VsOut;
    out.clip = vec4<f32>(p, 1.0, 1.0);
    out.far = unprojected.xyz / unprojected.w;
    return out;
}

const ZENITH_FALLOFF: f32 = 0.42;
const GROUND_FADE: f32 = 0.06;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let up = normalize(in.far).y;
    if (up >= 0.0) {
        let t = pow(min(up, 1.0), ZENITH_FALLOFF);
        return vec4<f32>(mix(s.horizon.rgb, s.zenith.rgb, t), 1.0);
    }
    let t = smoothstep(0.0, GROUND_FADE, -up);
    return vec4<f32>(mix(s.horizon.rgb, s.ground.rgb, t), 1.0);
}

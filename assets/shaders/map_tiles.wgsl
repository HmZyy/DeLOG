struct ViewUniform {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(0) @binding(1) var tile_texture: texture_2d_array<f32>;
@group(0) @binding(2) var tile_sampler: sampler;

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) layer: u32,
) -> VertexOut {
    var out: VertexOut;
    out.clip_position = view.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    out.layer = layer;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(tile_texture, tile_sampler, in.uv, i32(in.layer)).rgb, 1.0);
}

// Vehicle mesh shading. "PBR-lite": a single directional light with Lambert
// N·L plus a constant ambient term, which is plenty to read a vehicle's
// orientation against the grid. Unlike the trace pipelines this uses a real
// vertex+index buffer — meshes are small, static geometry, not data that scales
// with sample count, so the no-vertex-buffer rule doesn't apply.

struct MeshU {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // Upper 3×3 is the normal matrix (inverse-transpose of model); stored as a
    // mat4 for uniform alignment.
    normal_mat: mat4x4<f32>,
    light_dir: vec4<f32>, // direction TO the light (world), xyz; normalized
    color: vec4<f32>,
    cam_pos: vec4<f32>, // camera world position, xyz
    params: vec4<f32>, // x = ambient factor (0..1)
};

@group(0) @binding(0) var<uniform> u: MeshU;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world = u.model * vec4<f32>(in.pos, 1.0);
    out.clip = u.view_proj * world;
    out.world_normal = (u.normal_mat * vec4<f32>(in.normal, 0.0)).xyz;
    out.world_pos = world.xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Two-sided shading: the meshes render double-sided (cull_mode None), and a
    // surface seen from behind has its normal pointing away from the viewer, so
    // a naive N·L collapses it to the dark ambient term. Where interpenetrating
    // sub-meshes (fuselage / wings / nacelles) z-fight at their seams, such a
    // face that wins the depth test renders as a dark blotch on the model.
    // Orient the normal into the viewer's hemisphere so it is lit like the
    // surrounding surface. This keys off the view direction, NOT `front_facing`:
    // some models (and the cone fallback) have winding that disagrees with their
    // normals, and flipping by winding would darken correctly-lit front faces.
    var n = normalize(in.world_normal);
    let to_cam = u.cam_pos.xyz - in.world_pos;
    if (dot(n, to_cam) < 0.0) {
        n = -n;
    }
    let ndl = max(dot(n, normalize(u.light_dir.xyz)), 0.0);
    let ambient = u.params.x;
    let lit = ambient + (1.0 - ambient) * ndl;
    return vec4<f32>(u.color.rgb * lit, u.color.a);
}

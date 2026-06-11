// Vehicle mesh shading (PLAN.md §9.2 `mesh`, §12.4, GPU-22). "PBR-lite":
// a single directional light with Lambert N·L plus a constant ambient term,
// which is plenty to read a vehicle's orientation against the grid. Unlike the
// trace pipelines this uses a real vertex+index buffer — meshes are small,
// static geometry, not data that scales with sample count, so the §9.4
// no-vertex-buffer rule doesn't apply.

struct MeshU {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // Upper 3×3 is the normal matrix (inverse-transpose of model); stored as a
    // mat4 for uniform alignment.
    normal_mat: mat4x4<f32>,
    light_dir: vec4<f32>, // direction TO the light (world), xyz; normalized
    color: vec4<f32>,
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
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * u.model * vec4<f32>(in.pos, 1.0);
    out.world_normal = (u.normal_mat * vec4<f32>(in.normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let ndl = max(dot(n, normalize(u.light_dir.xyz)), 0.0);
    let ambient = u.params.x;
    let lit = ambient + (1.0 - ambient) * ndl;
    return vec4<f32>(u.color.rgb * lit, u.color.a);
}

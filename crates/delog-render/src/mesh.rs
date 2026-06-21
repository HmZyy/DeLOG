//! Vehicle mesh pipeline + GLB upload path.
//!
//! A small Lambert-plus-ambient pipeline draws vehicle models against the 3D
//! scene. Meshes are static geometry, so this uses a real vertex+index buffer
//! (the no-vertex-buffer rule is about sample data that scales, not models).
//!
//! [`MeshCpu`] is decoded from a GLB ([`load_glb`]) or generated procedurally
//! ([`MeshCpu::cone`] — the unconditional fallback so a missing/!broken asset
//! never blanks the scene), then uploaded to a [`MeshGpu`]. Matrices arrive as
//! raw `[[f32; 4]; 4]`, keeping the crate math-library-free.

use crate::context::RenderContext;
use wgpu::util::DeviceExt;

/// Errors decoding a GLB into a [`MeshCpu`].
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("glTF decode failed: {0}")]
    Gltf(#[from] gltf::Error),
    #[error("GLB contains no mesh primitive")]
    NoMesh,
    #[error("mesh primitive has no POSITION attribute")]
    NoPositions,
}

/// One interleaved vertex: position + normal, both world-space model units.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

/// CPU-side mesh: interleaved vertices + a triangle index list.
#[derive(Clone, Debug, Default)]
pub struct MeshCpu {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl MeshCpu {
    /// Build from parallel position/normal/index arrays, computing smooth
    /// normals when none are supplied.
    pub fn new(
        positions: Vec<[f32; 3]>,
        normals: Option<Vec<[f32; 3]>>,
        indices: Vec<u32>,
    ) -> Self {
        let normals = normals.unwrap_or_else(|| smooth_normals(&positions, &indices));
        let vertices = positions
            .into_iter()
            .zip(normals)
            .map(|(pos, normal)| Vertex { pos, normal })
            .collect();
        Self { vertices, indices }
    }

    /// A procedural half-cone — the unconditional model fallback.
    ///
    /// The nose is on +X and the axis runs along the x-axis; the cone is sliced
    /// through its axis by the `y = 0` plane, keeping the upper half. After the
    /// mesh→body correction (`rot_x −90°`) +X stays body-forward and +Y maps to
    /// body-up, so it renders as a rounded dome on top with a flat cut on the
    /// bottom (`−Y`), pointing the way it travels. Flat-shaded (per-face
    /// normals); surfaces are the lateral dome, the flat bottom, and the
    /// half-disc tail cap.
    pub fn cone(segments: u32, radius: f32, height: f32) -> Self {
        let segments = segments.max(3);
        let apex = [height, 0.0, 0.0];
        let center = [0.0, 0.0, 0.0];
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let pi = std::f32::consts::PI;
        // Upper semicircle (y ≥ 0) of the base ring, in the x = 0 plane.
        let ring = |a: f32| [0.0, radius * a.sin(), radius * a.cos()];
        for i in 0..segments {
            let a0 = i as f32 / segments as f32 * pi;
            let a1 = (i + 1) as f32 / segments as f32 * pi;
            let p0 = ring(a0);
            let p1 = ring(a1);
            // Lateral dome facet with an outward flat normal (winding
            // apex→p1→p0 so the normal points nose-and-out, away from the axis).
            let n = face_normal(apex, p1, p0);
            let base = vertices.len() as u32;
            vertices.push(Vertex {
                pos: apex,
                normal: n,
            });
            vertices.push(Vertex { pos: p0, normal: n });
            vertices.push(Vertex { pos: p1, normal: n });
            indices.extend([base, base + 1, base + 2]);
            // Half-disc tail cap (center, p0, p1), facing −X.
            let tail = [-1.0, 0.0, 0.0];
            let cap = vertices.len() as u32;
            vertices.push(Vertex {
                pos: center,
                normal: tail,
            });
            vertices.push(Vertex {
                pos: p0,
                normal: tail,
            });
            vertices.push(Vertex {
                pos: p1,
                normal: tail,
            });
            indices.extend([cap, cap + 1, cap + 2]);
        }
        // Flat bottom: the axial cut, a single triangle from the nose to the
        // base diameter, facing −Y (down). Winding nose→ring(0)→ring(π).
        let down = [0.0, -1.0, 0.0];
        let b = vertices.len() as u32;
        vertices.push(Vertex {
            pos: apex,
            normal: down,
        });
        vertices.push(Vertex {
            pos: ring(0.0),
            normal: down,
        });
        vertices.push(Vertex {
            pos: ring(pi),
            normal: down,
        });
        indices.extend([b, b + 1, b + 2]);
        Self { vertices, indices }
    }
}

fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-12 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// Per-vertex smooth normals: sum each triangle's face normal into its three
/// vertices, then normalize.
fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![[0.0f32; 3]; positions.len()];
    for tri in indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let n = face_normal(positions[i0], positions[i1], positions[i2]);
        for &i in &[i0, i1, i2] {
            acc[i][0] += n[0];
            acc[i][1] += n[1];
            acc[i][2] += n[2];
        }
    }
    for n in &mut acc {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-12 {
            *n = [n[0] / len, n[1] / len, n[2] / len];
        } else {
            *n = [0.0, 1.0, 0.0];
        }
    }
    acc
}

/// Decode a GLB into a single [`MeshCpu`]: every triangle primitive in
/// the default scene is baked into one mesh by its node's world transform, so
/// multi-part models (e.g. a quad's arms) land in the right place. Missing
/// normals are computed smooth; missing indices become a flat `0..n` list.
pub fn load_glb(bytes: &[u8]) -> Result<MeshCpu, MeshError> {
    let (doc, buffers, _images) = gltf::import_slice(bytes)?;
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or(MeshError::NoMesh)?;
    let mut out = MeshCpu::default();
    for node in scene.nodes() {
        add_node(&node, IDENTITY4, &buffers, &mut out);
    }
    if out.vertices.is_empty() {
        return Err(MeshError::NoMesh);
    }
    Ok(out)
}

/// Column-major 4×4 identity.
const IDENTITY4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Column-major 4×4 multiply (`a * b`).
fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut m = [[0.0f32; 4]; 4];
    for (c, col) in m.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    m
}

/// Transform a point by a column-major 4×4 (w = 1).
fn mat4_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

/// Transform a direction by the upper 3×3 (no translation), renormalized later.
fn mat4_dir(m: &[[f32; 4]; 4], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

fn add_node(
    node: &gltf::Node,
    parent: [[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    out: &mut MeshCpu,
) {
    let world = mat4_mul(parent, node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.map(|p| mat4_point(&world, p)).collect();
            let base = out.vertices.len() as u32;
            let indices: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().map(|i| i + base).collect(),
                None => (base..base + positions.len() as u32).collect(),
            };
            match reader.read_normals() {
                Some(normals) => {
                    for (pos, n) in positions.iter().zip(normals) {
                        out.vertices.push(Vertex {
                            pos: *pos,
                            normal: normalize(mat4_dir(&world, n)),
                        });
                    }
                }
                None => {
                    // No normals: push placeholders, fix up smooth below.
                    let start = out.vertices.len();
                    for pos in &positions {
                        out.vertices.push(Vertex {
                            pos: *pos,
                            normal: [0.0, 1.0, 0.0],
                        });
                    }
                    let local: Vec<[f32; 3]> = positions.clone();
                    let local_idx: Vec<u32> = indices.iter().map(|i| i - base).collect();
                    let sm = smooth_normals(&local, &local_idx);
                    for (v, n) in out.vertices[start..].iter_mut().zip(sm) {
                        v.normal = n;
                    }
                }
            }
            out.indices.extend(indices);
        }
    }
    for child in node.children() {
        add_node(&child, world, buffers, out);
    }
}

fn normalize(n: [f32; 3]) -> [f32; 3] {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-12 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// GPU-resident mesh: vertex + index buffers and the index count to draw.
pub struct MeshGpu {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

impl MeshGpu {
    pub fn upload(ctx: &RenderContext, mesh: &MeshCpu) -> Self {
        let vertices = ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("delog-mesh-vertices"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let indices = ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("delog-mesh-indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        Self {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
        }
    }
}

/// Per-mesh uniform. Matrices are raw column-major arrays; `normal_mat` is the
/// inverse-transpose of `model` (upper 3×3 used).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshUniform {
    pub view_proj: [[f32; 4]; 4],
    pub model: [[f32; 4]; 4],
    pub normal_mat: [[f32; 4]; 4],
    /// Direction TO the light (world), xyz; should be normalized.
    pub light_dir: [f32; 4],
    pub color: [f32; 4],
    /// Camera world position (xyz). Used for two-sided shading: the normal is
    /// flipped into the viewer's hemisphere so back-facing surfaces are lit
    /// rather than collapsing to the dark ambient term.
    pub cam_pos: [f32; 4],
    /// x = ambient factor (0..1).
    pub params: [f32; 4],
}

impl MeshUniform {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        view_proj: [[f32; 4]; 4],
        model: [[f32; 4]; 4],
        normal_mat: [[f32; 4]; 4],
        light_dir: [f32; 3],
        color: [f32; 4],
        cam_pos: [f32; 3],
        ambient: f32,
    ) -> Self {
        Self {
            view_proj,
            model,
            normal_mat,
            light_dir: [light_dir[0], light_dir[1], light_dir[2], 0.0],
            color,
            cam_pos: [cam_pos[0], cam_pos[1], cam_pos[2], 0.0],
            params: [ambient, 0.0, 0.0, 0.0],
        }
    }
}

/// Render pipeline + bind layout for vehicle meshes.
pub struct MeshPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl MeshPipeline {
    pub fn new(
        ctx: &RenderContext,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let device = ctx.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("delog-mesh.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../assets/shaders/mesh.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("delog-mesh-bind-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<MeshUniform>() as u64
                    ),
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("delog-mesh-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 12,
                    shader_location: 1,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("delog-mesh-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[vertex_layout],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Double-sided in v1: a winding mismatch in a user GLB should
                // dim a face, never make the model vanish.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group_layout,
        }
    }

    pub fn bind_group(&self, ctx: &RenderContext, uniform: &wgpu::Buffer) -> wgpu::BindGroup {
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-mesh-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        })
    }

    /// Draw an uploaded mesh with the given bind group.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        mesh: &MeshGpu,
    ) {
        if mesh.index_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scene3dTarget;
    use glam::{Mat4, Vec3};

    /// Build a minimal single-buffer GLB with a POSITION accessor + indices and
    /// no normals — exercises the decode + smooth-normal path.
    fn tiny_glb(positions: &[[f32; 3]], indices: &[u32]) -> Vec<u8> {
        // BIN: indices (u32) then positions (f32 vec3).
        let mut bin = Vec::new();
        for &i in indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let pos_offset = bin.len();
        for p in positions {
            for &c in p {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let idx_len = indices.len() * 4;
        let pos_len = positions.len() * 12;
        let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in positions {
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":1}},"indices":0,"mode":4}}]}}],"buffers":[{{"byteLength":{bl}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{idx_len},"target":34963}},{{"buffer":0,"byteOffset":{pos_offset},"byteLength":{pos_len},"target":34962}}],"accessors":[{{"bufferView":0,"componentType":5125,"count":{nidx},"type":"SCALAR"}},{{"bufferView":1,"componentType":5126,"count":{npos},"type":"VEC3","min":[{mn0},{mn1},{mn2}],"max":[{mx0},{mx1},{mx2}]}}]}}"#,
            bl = bin.len(),
            nidx = indices.len(),
            npos = positions.len(),
            mn0 = mn[0],
            mn1 = mn[1],
            mn2 = mn[2],
            mx0 = mx[0],
            mx1 = mx[1],
            mx2 = mx[2],
        );
        let mut json_bytes = json.into_bytes();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }

        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(&0x46546C67u32.to_le_bytes()); // "glTF"
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F534Au32.to_le_bytes()); // "JSON"
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E4942u32.to_le_bytes()); // "BIN\0"
        glb.extend_from_slice(&bin);
        glb
    }

    #[test]
    fn cone_has_consistent_geometry_and_unit_normals() {
        let cone = MeshCpu::cone(16, 1.0, 2.0);
        // dome + tail-cap tri per segment, plus one flat-bottom tri.
        assert_eq!(cone.indices.len(), 16 * 6 + 3);
        assert_eq!(cone.vertices.len(), 16 * 6 + 3);
        for v in &cone.vertices {
            let n = v.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "normal not unit: {len}");
        }
    }

    #[test]
    fn cone_nose_points_along_plus_x() {
        // The fallback model's nose sits on +X so the mesh→body correction
        // (rot_x −90°) leaves it pointing body-forward, like the
        // fixed-wing model — not straight up (+Y).
        let (h, r) = (2.0_f32, 1.0_f32);
        let cone = MeshCpu::cone(16, r, h);
        // Exactly one extreme apex vertex per segment, all at [h, 0, 0].
        let apex = cone
            .vertices
            .iter()
            .map(|v| v.pos)
            .find(|p| p[0] > h - 1e-3);
        assert_eq!(apex, Some([h, 0.0, 0.0]), "apex should be on +X");
        // The whole cone lies between the tail (x = 0) and the nose (x = h),
        // and only in the upper half (y ≥ 0) — it's a half-cone whose flat cut
        // sits on the bottom (−Y); nothing dips below the y = 0 plane.
        for v in &cone.vertices {
            assert!(
                (0.0..=h + 1e-4).contains(&v.pos[0]),
                "vertex outside [tail, nose] span: {:?}",
                v.pos
            );
            assert!(
                v.pos[1] >= -1e-4,
                "vertex below the flat bottom: {:?}",
                v.pos
            );
        }
        // The flat-bottom triangle's far corners are the base diameter ends.
        let near = |p: [f32; 3], q: [f32; 3]| {
            (p[0] - q[0]).abs() < 1e-4 && (p[1] - q[1]).abs() < 1e-4 && (p[2] - q[2]).abs() < 1e-4
        };
        assert!(
            cone.vertices.iter().any(|v| near(v.pos, [0.0, 0.0, r]))
                && cone.vertices.iter().any(|v| near(v.pos, [0.0, 0.0, -r])),
            "expected the base diameter ends on the flat bottom"
        );
    }

    #[test]
    fn load_glb_decodes_positions_indices_and_computes_normals() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let indices = [0u32, 1, 2];
        let glb = tiny_glb(&positions, &indices);

        let mesh = load_glb(&glb).expect("GLB should decode");
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        // The triangle lies in the z=0 plane → computed normal is ±Z.
        let n = mesh.vertices[0].normal;
        assert!(n[2].abs() > 0.99, "expected a ±Z normal, got {n:?}");
    }

    #[test]
    fn cone_renders_shaded_with_lit_and_shadowed_facets() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping mesh render test");
            return;
        };
        let (w, h) = (96u32, 96u32);
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let pipe = MeshPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        let gpu = MeshGpu::upload(&ctx, &MeshCpu::cone(24, 1.2, 2.5));

        // Camera looking at the cone from the side; light from the +X/+Y/+Z
        // front so some facets face it and others fall to ambient.
        let eye = Vec3::new(4.0, 2.5, 4.0);
        let proj = Mat4::perspective_rh(0.9, w as f32 / h as f32, 0.1, 100.0);
        let view = Mat4::look_at_rh(eye, Vec3::new(0.0, 1.0, 0.0), Vec3::Y);
        let model = Mat4::IDENTITY;
        let uni_data = MeshUniform::new(
            (proj * view).to_cols_array_2d(),
            model.to_cols_array_2d(),
            model.inverse().transpose().to_cols_array_2d(),
            Vec3::new(1.0, 1.0, 1.0).normalize().to_array(),
            [0.85, 0.85, 0.9, 1.0],
            eye.to_array(),
            0.2,
        );
        let uni = ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh-test-uniform"),
                contents: bytemuck::bytes_of(&uni_data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = pipe.bind_group(&ctx, &uni);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, wgpu::Color::BLACK);
            pipe.draw(&mut pass, &bind, &gpu);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        // Non-background (lit) pixels: brighter than pure black.
        let mut lit: Vec<u8> = Vec::new();
        for x in 0..w {
            for y in 0..h {
                let g = img.pixel(x, y)[1];
                if g > 12 {
                    lit.push(g);
                }
            }
        }
        assert!(
            lit.len() > 100,
            "cone should cover a chunk of pixels, got {}",
            lit.len()
        );
        let (min, max) = (*lit.iter().min().unwrap(), *lit.iter().max().unwrap());
        // N·L shading ⇒ a real brightness range (not flat-shaded).
        assert!(
            max as i32 - min as i32 > 50,
            "expected shading gradient, got min={min} max={max}"
        );
    }

    /// Meshes render double-sided, so a back face whose authored normal
    /// points away from the light must still be lit (two-sided shading flips the
    /// normal toward the viewer) — otherwise it collapses to the dark ambient
    /// term and shows up as black blotches where interpenetrating sub-meshes
    /// z-fight. Here a single triangle is viewed from its back side with the
    /// light on the camera's side: with two-sided shading the visible face is
    /// bright; the old single-sided shader left it at ambient (~0.2).
    #[test]
    fn back_face_is_lit_not_dark() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping back-face mesh test");
            return;
        };
        let (w, h) = (64u32, 64u32);
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let pipe = MeshPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );

        // Triangle in the z=0 plane, wound CCW as seen from +Z, with an explicit
        // +Z normal (its "front"). We view it from −Z, so the visible side is the
        // back face (front_facing == false).
        let mesh = MeshCpu::new(
            vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
            Some(vec![[0.0, 0.0, 1.0]; 3]),
            vec![0, 1, 2],
        );
        let gpu = MeshGpu::upload(&ctx, &mesh);

        let eye = Vec3::new(0.0, 0.0, -4.0);
        let proj = Mat4::perspective_rh(0.9, w as f32 / h as f32, 0.1, 100.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let model = Mat4::IDENTITY;
        // Light comes from −Z (the camera's side). The visible face's normal
        // points +Z (away from the viewer), so two-sided shading flips it toward
        // the camera, where it faces the light and is fully lit; without the flip
        // it faces away → dark ambient only.
        let uni_data = MeshUniform::new(
            (proj * view).to_cols_array_2d(),
            model.to_cols_array_2d(),
            model.inverse().transpose().to_cols_array_2d(),
            [0.0, 0.0, -1.0],
            [0.8, 0.8, 0.9, 1.0],
            eye.to_array(),
            0.2,
        );
        let uni = ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh-backface-uniform"),
                contents: bytemuck::bytes_of(&uni_data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = pipe.bind_group(&ctx, &uni);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, wgpu::Color::BLACK);
            pipe.draw(&mut pass, &bind, &gpu);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        // The brightest covered pixel must be well above the ambient-only level
        // (ambient 0.2 × 0.8 × 255 ≈ 41); two-sided lighting drives it toward the
        // fully-lit ~0.8 × 255 ≈ 204.
        let max_g = (0..w)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .map(|(x, y)| img.pixel(x, y)[1])
            .max()
            .unwrap();
        assert!(
            max_g > 120,
            "back face should be lit by two-sided shading, got max green {max_g} (ambient ≈ 41)"
        );
    }
}

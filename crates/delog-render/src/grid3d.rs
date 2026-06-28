//! Infinite ground grid + world axes.
//!
//! A single full-screen triangle whose fragment shader unprojects each pixel to
//! a ray, intersects the `y = 0` plane, and draws a derivative-antialiased grid
//! that fades with distance — "infinite" without tessellated geometry. It writes
//! per-fragment depth so later meshes/trajectories occlude it correctly.
//! Ground axes follow the render mapping `(E, −D, −N)`: X = East → red,
//! Z = South → blue.

use crate::context::RenderContext;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GridUniform {
    /// World → clip.
    pub view_proj: [[f32; 4]; 4],
    /// Clip → camera-relative world (`world − cam_pos`), built from the
    /// rotation-only view: `(proj · view_without_translation)⁻¹`. Camera-relative
    /// keeps the shader's f32 unprojection precise far from the render origin
    /// (otherwise the grid crawls while zooming/following).
    pub inv_vp_rel: [[f32; 4]; 4],
    /// Camera world position (xyz); `w` = LOD blend (1.0 = cross-fade two
    /// bracketing power-of-ten grids around `cell`, 0.0 = single level).
    pub cam_pos: [f32; 4],
    /// `x` = cell size (world units), `y` = fade start, `z` = fade end,
    /// `w` = fog enabled (1.0 = fade with distance, 0.0 = crisp to far plane).
    pub params: [f32; 4],
}

impl GridUniform {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        view_proj: [[f32; 4]; 4],
        inv_vp_rel: [[f32; 4]; 4],
        cam_pos: [f32; 3],
        cell: f32,
        fade_start: f32,
        fade_end: f32,
        fog: bool,
        lod: bool,
    ) -> Self {
        Self {
            view_proj,
            inv_vp_rel,
            cam_pos: [
                cam_pos[0],
                cam_pos[1],
                cam_pos[2],
                if lod { 1.0 } else { 0.0 },
            ],
            params: [cell, fade_start, fade_end, if fog { 1.0 } else { 0.0 }],
        }
    }
}

const UNIFORM_BINDING: u32 = 0;

pub struct Grid3dPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
}

impl Grid3dPipeline {
    /// Formats and sample count must match the [`crate::Scene3dTarget`] it draws into.
    pub fn new(
        ctx: &RenderContext,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let device = ctx.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("delog-grid3d.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../assets/shaders/grid3d.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("delog-grid3d-bind-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: UNIFORM_BINDING,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<GridUniform>() as u64
                    ),
                },
                count: None,
            }],
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-grid3d-uniform"),
            size: std::mem::size_of::<GridUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-grid3d-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: UNIFORM_BINDING,
                resource: uniform.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("delog-grid3d-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("delog-grid3d-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
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
            bind_group,
            uniform,
        }
    }

    pub fn set_uniform(&self, ctx: &RenderContext, uniform: &GridUniform) {
        ctx.queue()
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(uniform));
    }

    /// Call [`Self::set_uniform`] first; the pass must have a depth attachment
    /// matching `depth_format`.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scene3dTarget;
    use glam::{Mat4, Vec3};

    fn any_pixel(img: &crate::target::RgbaImage, pred: impl Fn([u8; 4]) -> bool) -> bool {
        (0..img.width).any(|x| (0..img.height).any(|y| pred(img.pixel(x, y))))
    }

    #[test]
    fn grid_draws_faded_lines_and_colored_axes() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping grid3d test");
            return;
        };
        let (w, h) = (128u32, 128u32);
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let grid = Grid3dPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );

        let eye = Vec3::new(3.0, 5.0, 8.0);
        let proj = Mat4::perspective_rh(60f32.to_radians(), w as f32 / h as f32, 0.1, 200.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let view_proj = proj * view;
        // inv_vp_rel is built from the rotation-only view (translation zeroed).
        let mut view_rot = view;
        view_rot.w_axis = glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
        let inv = (proj * view_rot).inverse();
        grid.set_uniform(
            &ctx,
            &GridUniform::new(
                view_proj.to_cols_array_2d(),
                inv.to_cols_array_2d(),
                eye.to_array(),
                1.0,   // cell size
                12.0,  // fade start
                60.0,  // fade end
                true,  // fog
                false, // lod
            ),
        );

        let clear = wgpu::Color {
            r: 10.0 / 255.0,
            g: 12.0 / 255.0,
            b: 16.0 / 255.0,
            a: 1.0,
        };
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, clear);
            grid.draw(&mut pass);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();

        assert!(
            any_pixel(&img, |p| p[0] > 150 && p[1] < 90 && p[2] < 90),
            "expected a red X (East) axis pixel"
        );
        assert!(
            any_pixel(&img, |p| p[2] > 150 && p[0] < 90 && p[1] < 110),
            "expected a blue Z (South) axis pixel"
        );
        let total = (w * h) as usize;
        let bg = img.count_matching([10, 12, 16, 255], 6);
        assert!(
            bg > total / 4 && bg < total * 95 / 100,
            "grid should draw lines yet leave background, got {bg} bg pixels of {total}"
        );
        // Top of frame is above the horizon (rays miss the ground).
        assert!(
            (0..w).all(|x| img.matches(x, 0, [10, 12, 16, 255], 6)),
            "top row should be background (above horizon)"
        );
    }
}

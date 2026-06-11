//! 3D trajectory polyline pipeline (PLAN.md §9.2 `traj3d`, §12.3, GPU-23).
//!
//! Vertex-pulled line-list: trajectory points live in a `vec4<f32>` storage
//! buffer (xyz + pad), and the pipeline draws `(N-1) * 2` vertices reading two
//! endpoints per segment — no vertex buffer, no CPU tessellation. A non-finite
//! endpoint collapses its segment, so NaN marks a gap (§9.4). Lines are 1 px in
//! v1 (thick/joined lines are GPU-25). The same pipeline draws the vertical
//! world axis line that the ground-plane grid (GPU-21) cannot.
//!
//! Like the rest of `delog-render` this is pure wgpu: matrices arrive as raw
//! `[[f32; 4]; 4]` and a points buffer + uniform buffer are supplied by the
//! caller, so it backs headless golden-image tests.

use crate::context::RenderContext;

/// Per-trajectory uniform: world→clip transform and the line color.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Traj3dUniform {
    pub view_proj: [[f32; 4]; 4],
    pub color: [f32; 4],
}

impl Traj3dUniform {
    pub fn new(view_proj: [[f32; 4]; 4], color: [f32; 4]) -> Self {
        Self { view_proj, color }
    }
}

const POINTS_BINDING: u32 = 0;
const UNIFORM_BINDING: u32 = 1;

/// Render pipeline + bind layout for trajectory polylines.
pub struct Traj3dPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl Traj3dPipeline {
    /// Build for a target with the given color/depth formats and MSAA sample
    /// count (match the [`crate::Scene3dTarget`] it draws into).
    pub fn new(
        ctx: &RenderContext,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let device = ctx.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("delog-traj3d.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../assets/shaders/traj3d.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("delog-traj3d-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: POINTS_BINDING,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: UNIFORM_BINDING,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<Traj3dUniform>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("delog-traj3d-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("delog-traj3d-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
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

    /// Bind group for one trajectory: its `vec4` points buffer and uniform.
    pub fn bind_group(
        &self,
        ctx: &RenderContext,
        points: &wgpu::Buffer,
        uniform: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-traj3d-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: POINTS_BINDING,
                    resource: points.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: UNIFORM_BINDING,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }

    /// Draw a polyline of `point_count` points as `(point_count - 1)` line
    /// segments. Fewer than two points draws nothing.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        point_count: u32,
    ) {
        if point_count < 2 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..(point_count - 1) * 2, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scene3dTarget;
    use glam::{Mat4, Vec3};

    /// Upload `pts` (xyz triples) as a `vec4` storage buffer.
    fn points_buffer(ctx: &RenderContext, pts: &[[f32; 3]]) -> wgpu::Buffer {
        let data: Vec<[f32; 4]> = pts.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
        let buf = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("traj-test-points"),
            size: std::mem::size_of_val(data.as_slice()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue()
            .write_buffer(&buf, 0, bytemuck::cast_slice(&data));
        buf
    }

    fn uniform_buffer(ctx: &RenderContext, u: &Traj3dUniform) -> wgpu::Buffer {
        let buf = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("traj-test-uniform"),
            size: std::mem::size_of::<Traj3dUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue().write_buffer(&buf, 0, bytemuck::bytes_of(u));
        buf
    }

    /// Top-down camera so world X→screen X and world Z→screen Y; a line along
    /// X at z=0 lands on the horizontal center row.
    fn topdown(w: u32, h: u32) -> Mat4 {
        let proj = Mat4::perspective_rh(0.9, w as f32 / h as f32, 0.1, 200.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 20.0, 0.0), Vec3::ZERO, Vec3::Z);
        proj * view
    }

    fn render(ctx: &RenderContext, w: u32, h: u32, pts: &[[f32; 3]]) -> crate::target::RgbaImage {
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let pipe = Traj3dPipeline::new(
            ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        let points = points_buffer(ctx, pts);
        let uni = uniform_buffer(
            ctx,
            &Traj3dUniform::new(topdown(w, h).to_cols_array_2d(), [1.0, 1.0, 0.0, 1.0]),
        );
        let bind = pipe.bind_group(ctx, &points, &uni);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, wgpu::Color::BLACK);
            pipe.draw(&mut pass, &bind, pts.len() as u32);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        target.read_rgba()
    }

    /// GPU-23: a polyline along world X renders a yellow line across the view.
    #[test]
    fn polyline_renders_a_visible_line() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping traj3d test");
            return;
        };
        let (w, h) = (96u32, 96u32);
        let img = render(&ctx, w, h, &[[-6.0, 0.0, 0.0], [6.0, 0.0, 0.0]]);

        let yellow = |p: [u8; 4]| p[0] > 30 && p[1] > 30 && p[2] < 80;
        let lit = (0..w)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .filter(|&(x, y)| yellow(img.pixel(x, y)))
            .count();
        // The line drew, but only a thin band of pixels (not a fill).
        assert!(
            lit > 8,
            "trajectory line should be visible, got {lit} lit px"
        );
        assert!(lit < (w * h) as usize / 8, "line should be thin, got {lit}");
    }

    /// GPU-23: a NaN endpoint drops its segment (gap), so a two-point polyline
    /// with a NaN end draws nothing.
    #[test]
    fn nan_endpoint_makes_a_gap() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let img = render(&ctx, w, h, &[[-6.0, 0.0, 0.0], [f32::NAN, 0.0, 0.0]]);
        let yellow = |p: [u8; 4]| p[0] > 30 && p[1] > 30 && p[2] < 80;
        let lit = (0..w)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .filter(|&(x, y)| yellow(img.pixel(x, y)))
            .count();
        assert_eq!(lit, 0, "NaN segment must not rasterize, got {lit} lit px");
    }
}

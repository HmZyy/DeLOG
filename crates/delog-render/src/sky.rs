use crate::context::RenderContext;

pub const ZENITH_RGB: [f32; 3] = [0.14, 0.26, 0.48];

pub const HORIZON_RGB: [f32; 3] = [0.62, 0.71, 0.80];

pub const GROUND_RGB: [f32; 3] = [0.10, 0.11, 0.13];

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkyUniform {
    pub inv_vp_rel: [[f32; 4]; 4],
    pub zenith: [f32; 4],
    pub horizon: [f32; 4],
    pub ground: [f32; 4],
}

impl SkyUniform {
    pub fn new(inv_vp_rel: [[f32; 4]; 4]) -> Self {
        Self {
            inv_vp_rel,
            zenith: rgba(ZENITH_RGB),
            horizon: rgba(HORIZON_RGB),
            ground: rgba(GROUND_RGB),
        }
    }
}

fn rgba(rgb: [f32; 3]) -> [f32; 4] {
    [rgb[0], rgb[1], rgb[2], 1.0]
}

const UNIFORM_BINDING: u32 = 0;

pub struct SkyPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
}

impl SkyPipeline {
    pub fn new(
        ctx: &RenderContext,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let device = ctx.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("delog-sky.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../assets/shaders/sky.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("delog-sky-bind-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: UNIFORM_BINDING,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<SkyUniform>() as u64
                    ),
                },
                count: None,
            }],
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-sky-uniform"),
            size: std::mem::size_of::<SkyUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-sky-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: UNIFORM_BINDING,
                resource: uniform.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("delog-sky-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("delog-sky-pipeline"),
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
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
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
                    blend: None,
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

    pub fn set_uniform(&self, ctx: &RenderContext, uniform: &SkyUniform) {
        ctx.queue()
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(uniform));
    }

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
    use crate::target::RgbaImage;
    use glam::{Mat4, Vec3, Vec4};

    const MAGENTA: wgpu::Color = wgpu::Color {
        r: 1.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };

    fn level_view(w: u32, h: u32, eye: Vec3) -> [[f32; 4]; 4] {
        let proj = Mat4::perspective_rh(60f32.to_radians(), w as f32 / h as f32, 0.1, 200.0);
        let view = Mat4::look_at_rh(eye, eye + Vec3::new(0.0, 0.0, -1.0), Vec3::Y);
        let mut rot = view;
        rot.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0);
        (proj * rot).inverse().to_cols_array_2d()
    }

    fn render(ctx: &RenderContext, w: u32, h: u32, inv_vp_rel: [[f32; 4]; 4]) -> RgbaImage {
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let sky = SkyPipeline::new(
            ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        sky.set_uniform(ctx, &SkyUniform::new(inv_vp_rel));

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, MAGENTA);
            sky.draw(&mut pass);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        target.read_rgba()
    }

    fn luma(p: [u8; 4]) -> f32 {
        0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
    }

    fn blue_excess(p: [u8; 4]) -> f32 {
        f32::from(p[2]) - f32::from(p[0])
    }

    fn row_mean(img: &RgbaImage, y: u32, f: impl Fn([u8; 4]) -> f32) -> f32 {
        let sum: f32 = (0..img.width).map(|x| f(img.pixel(x, y))).sum();
        sum / img.width as f32
    }

    #[test]
    fn sky_brightens_from_the_zenith_down_to_the_horizon_haze() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter - skipping sky test");
            return;
        };
        let (w, h) = (96u32, 96u32);
        let img = render(&ctx, w, h, level_view(w, h, Vec3::ZERO));

        let top = row_mean(&img, 0, luma);
        let above_horizon = row_mean(&img, h / 2 - 2, luma);
        assert!(
            above_horizon > top + 20.0,
            "haze at the horizon should be brighter than the zenith, got top {top} vs horizon {above_horizon}"
        );
    }

    #[test]
    fn zenith_is_bluer_than_the_horizon_haze() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let img = render(&ctx, w, h, level_view(w, h, Vec3::ZERO));

        let top = row_mean(&img, 0, blue_excess);
        let above_horizon = row_mean(&img, h / 2 - 2, blue_excess);
        assert!(
            top > above_horizon + 10.0,
            "the upper sky should be bluer than the pale horizon, got top {top} vs horizon {above_horizon}"
        );
    }

    #[test]
    fn below_the_horizon_is_darker_than_the_sky() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let img = render(&ctx, w, h, level_view(w, h, Vec3::ZERO));

        let top = row_mean(&img, 0, luma);
        let bottom = row_mean(&img, h - 1, luma);
        assert!(
            bottom < top - 10.0,
            "below the horizon should be darker than the zenith, got bottom {bottom} vs top {top}"
        );
    }

    #[test]
    fn the_horizon_sits_at_the_center_of_a_level_view() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let img = render(&ctx, w, h, level_view(w, h, Vec3::ZERO));

        let brightest = (0..h)
            .max_by(|&a, &b| {
                row_mean(&img, a, luma)
                    .partial_cmp(&row_mean(&img, b, luma))
                    .unwrap()
            })
            .unwrap();
        let center = h / 2;
        assert!(
            brightest.abs_diff(center) <= 2,
            "the haze band should sit on the horizon at row {center}, got row {brightest}"
        );
    }

    #[test]
    fn sky_fills_every_pixel() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (64u32, 64u32);
        let img = render(&ctx, w, h, level_view(w, h, Vec3::ZERO));

        assert_eq!(
            img.count_matching([255, 0, 255, 255], 2),
            0,
            "the sky must cover the whole target, leaving no clear color behind"
        );
    }

    fn render_with_optional_line(ctx: &RenderContext, w: u32, h: u32, line: bool) -> RgbaImage {
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let sky = SkyPipeline::new(
            ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        let traj = crate::Traj3dPipeline::new(
            ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );

        let eye = Vec3::new(0.0, 20.0, 0.0);
        let proj = Mat4::perspective_rh(0.9, w as f32 / h as f32, 0.1, 200.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z);
        let mut rot = view;
        rot.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0);
        sky.set_uniform(
            ctx,
            &SkyUniform::new((proj * rot).inverse().to_cols_array_2d()),
        );

        let pts: [[f32; 4]; 2] = [[-6.0, 0.0, 0.0, 1.0], [6.0, 0.0, 0.0, 1.0]];
        let points = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of_val(&pts) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue()
            .write_buffer(&points, 0, bytemuck::cast_slice(&pts));
        let uniform = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<crate::Traj3dUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue().write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&crate::Traj3dUniform::new(
                (proj * view).to_cols_array_2d(),
                [1.0, 1.0, 0.0, 1.0],
            )),
        );
        let bind = traj.bind_group(ctx, &points, &uniform);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, MAGENTA);
            sky.draw(&mut pass);
            if line {
                traj.draw(&mut pass, &bind, pts.len() as u32);
            }
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        target.read_rgba()
    }

    #[test]
    fn sky_does_not_occlude_scene_geometry() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let sky_only = render_with_optional_line(&ctx, w, h, false);
        let with_line = render_with_optional_line(&ctx, w, h, true);

        let changed = (0..w)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .filter(|&(x, y)| sky_only.pixel(x, y) != with_line.pixel(x, y))
            .count();
        assert!(
            changed > 8,
            "geometry drawn after the sky must still pass the depth test, got {changed} changed px"
        );
    }
}

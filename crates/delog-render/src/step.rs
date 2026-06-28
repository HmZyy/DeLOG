//! Vertex-pulled stepped trace pipeline.
//!
//! Previous-sample/hold variant of `line_pull`: each adjacent pair emits a
//! horizontal hold then a vertical transition.

use crate::context::RenderContext;
use crate::uniforms::UniformRing;

const XY_BINDING: u32 = 0;
const UNIFORM_BINDING: u32 = 1;

pub struct StepPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl StepPipeline {
    pub fn new(ctx: &RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let shader = ctx
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("delog-step-pull.wgsl"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../../../assets/shaders/step_pull.wgsl").into(),
                ),
            });

        let bind_group_layout =
            ctx.device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("delog-step-pull-bind-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: XY_BINDING,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(8),
                            },
                            count: None,
                        },
                        UniformRing::layout_entry(UNIFORM_BINDING),
                    ],
                });

        let pipeline_layout =
            ctx.device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("delog-step-pull-pipeline-layout"),
                    bind_group_layouts: &[Some(&bind_group_layout)],
                    immediate_size: 0,
                });

        let pipeline = ctx
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("delog-step-pull-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
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

    pub fn bind_group(
        &self,
        ctx: &RenderContext,
        xy: &wgpu::Buffer,
        uniforms: &UniformRing,
    ) -> wgpu::BindGroup {
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-step-pull-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: XY_BINDING,
                    resource: xy.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: UNIFORM_BINDING,
                    resource: uniforms.binding_resource(),
                },
            ],
        })
    }

    pub fn bind(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
    }

    /// Pipeline must already be bound via [`Self::bind`].
    pub fn draw_trace(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        uniform_offset: u32,
        sample_count: u32,
    ) {
        if sample_count < 2 {
            return;
        }

        let vertex_count = sample_count.saturating_sub(1).saturating_mul(12);
        pass.set_bind_group(0, bind_group, &[uniform_offset]);
        pass.draw(0..vertex_count, 0..1);
    }

    pub fn encode_trace(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        uniform_offset: u32,
        sample_count: u32,
    ) {
        if sample_count < 2 {
            return;
        }
        self.bind(pass);
        self.draw_trace(pass, bind_group, uniform_offset, sample_count);
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    pub fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::OffscreenTarget;
    use crate::uniforms::PlotUniform;

    #[test]
    fn stepped_trace_renders_horizontal_hold_then_vertical_transition() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping step pipeline test");
            return;
        };
        let (w, h) = (64u32, 64u32);
        let target = OffscreenTarget::new(ctx.clone(), w, h);
        let pipeline = StepPipeline::new(&ctx, target.format());

        let xy = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("step-test-xy"),
            size: 4 * std::mem::size_of::<f32>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue()
            .write_buffer(&xy, 0, bytemuck::cast_slice(&[0.0_f32, 0.0, 10.0, 10.0]));

        let blue = [0u8, 0, 255, 255];
        let uniforms = UniformRing::new(ctx.clone(), 1);
        uniforms.write(
            0,
            &PlotUniform::from_view(
                (0.0, 10.0),
                (0.0, 10.0),
                [w as f32, h as f32],
                4.0,
                [0.0, 0.0, 1.0, 1.0],
            ),
        );
        let bind = pipeline.bind_group(&ctx, &xy, &uniforms);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("step-test-encoder"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("step-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pipeline.encode_trace(&mut pass, &bind, uniforms.dynamic_offset(0), 2);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        assert!(
            img.matches(w / 2, h - 1, blue, 16),
            "horizontal hold should render along the bottom"
        );
        assert!(
            img.matches(w - 1, h / 2, blue, 16),
            "vertical transition should render at the right edge"
        );
        assert!(
            img.matches(w / 2, h / 2, [0, 0, 0, 255], 16),
            "diagonal midpoint should remain clear"
        );
    }
}

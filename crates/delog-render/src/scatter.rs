//! Vertex-pulled scatter pipeline (PLAN.md GPU-07).
//!
//! Trace samples stay in the `BufferManager`'s interleaved `[x, y]` STORAGE
//! buffer. This pipeline emits one screen-space quad per sample, with point
//! size carried in `PlotUniform::view.z` so it can share the uniform ring used
//! by the line and min/max paths.

use crate::context::RenderContext;
use crate::uniforms::UniformRing;

const XY_BINDING: u32 = 0;
const UNIFORM_BINDING: u32 = 1;

/// Render pipeline and bind layout for scatter points.
pub struct ScatterPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl ScatterPipeline {
    pub fn new(ctx: &RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let shader = ctx
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("delog-scatter-pull.wgsl"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../../../assets/shaders/scatter_pull.wgsl").into(),
                ),
            });

        let bind_group_layout =
            ctx.device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("delog-scatter-pull-bind-layout"),
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
                    label: Some("delog-scatter-pull-pipeline-layout"),
                    bind_group_layouts: &[Some(&bind_group_layout)],
                    immediate_size: 0,
                });

        let pipeline = ctx
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("delog-scatter-pull-pipeline"),
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

    /// Create a bind group for one trace buffer plus the shared uniform ring.
    pub fn bind_group(
        &self,
        ctx: &RenderContext,
        xy: &wgpu::Buffer,
        uniforms: &UniformRing,
    ) -> wgpu::BindGroup {
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-scatter-pull-bind-group"),
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

    /// Encode one trace. `sample_count` is the number of `[x,y]` pairs resident
    /// in the storage buffer; each sample emits one six-vertex quad.
    pub fn encode_trace(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        uniform_offset: u32,
        sample_count: u32,
    ) {
        if sample_count == 0 {
            return;
        }

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[uniform_offset]);
        pass.draw(0..sample_count.saturating_mul(6), 0..1);
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
    fn points_render_as_screen_space_quads() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping scatter pipeline test");
            return;
        };
        let (w, h) = (64u32, 64u32);
        let target = OffscreenTarget::new(ctx.clone(), w, h);
        let pipeline = ScatterPipeline::new(&ctx, target.format());

        let xy = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("scatter-test-xy"),
            size: 4 * std::mem::size_of::<f32>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue()
            .write_buffer(&xy, 0, bytemuck::cast_slice(&[0.0_f32, 0.0, 10.0, 10.0]));

        let green = [0u8, 255, 0, 255];
        let uniforms = UniformRing::new(ctx.clone(), 1);
        uniforms.write(
            0,
            &PlotUniform::from_view(
                (0.0, 10.0),
                (0.0, 10.0),
                [w as f32, h as f32],
                6.0,
                [0.0, 1.0, 0.0, 1.0],
            ),
        );
        let bind = pipeline.bind_group(&ctx, &xy, &uniforms);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scatter-test-encoder"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scatter-test-pass"),
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
            img.matches(0, h - 1, green, 16),
            "origin point should render"
        );
        assert!(
            img.matches(w - 1, 0, green, 16),
            "opposite corner point should render"
        );
        assert!(
            img.matches(w / 2, h / 2, [0, 0, 0, 255], 16),
            "space between points should remain clear"
        );
    }
}

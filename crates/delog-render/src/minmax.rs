//! Decimated min/max column pipeline (PLAN.md §9.5, GPU-09).
//!
//! For zoomed-out views (`samples/px > 8`) the plot draws one vertical span per
//! pixel column instead of every segment — min/max so no transient is ever
//! hidden (the §9.5 "not LTTB" decision). Columns are `[x, min, max]` f32
//! triples in a STORAGE buffer; each emits a six-vertex quad via vertex pulling,
//! sharing the [`PlotUniform`](crate::uniforms::PlotUniform) transform with
//! `line_pull`.

use crate::context::RenderContext;
use crate::uniforms::UniformRing;

const COLS_BINDING: u32 = 0;
const UNIFORM_BINDING: u32 = 1;

/// Floats per column triple `[x, min, max]`.
pub const COLUMN_STRIDE: usize = 3;

/// Render pipeline + bind layout for decimated min/max columns.
pub struct MinMaxColPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl MinMaxColPipeline {
    pub fn new(ctx: &RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let shader = ctx
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("delog-minmax-col.wgsl"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../../../assets/shaders/minmax_col.wgsl").into(),
                ),
            });

        let bind_group_layout =
            ctx.device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("delog-minmax-col-bind-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: COLS_BINDING,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(4),
                            },
                            count: None,
                        },
                        UniformRing::layout_entry(UNIFORM_BINDING),
                    ],
                });

        let pipeline_layout =
            ctx.device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("delog-minmax-col-pipeline-layout"),
                    bind_group_layouts: &[Some(&bind_group_layout)],
                    immediate_size: 0,
                });

        let pipeline = ctx
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("delog-minmax-col-pipeline"),
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

    /// Bind group for one column buffer plus the shared uniform ring.
    pub fn bind_group(
        &self,
        ctx: &RenderContext,
        columns: &wgpu::Buffer,
        uniforms: &UniformRing,
    ) -> wgpu::BindGroup {
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-minmax-col-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: COLS_BINDING,
                    resource: columns.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: UNIFORM_BINDING,
                    resource: uniforms.binding_resource(),
                },
            ],
        })
    }

    /// Encode `column_count` vertical spans (six vertices each).
    pub fn encode(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        uniform_offset: u32,
        column_count: u32,
    ) {
        if column_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[uniform_offset]);
        pass.draw(0..column_count * 6, 0..1);
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

    /// GPU-09: a column band renders as a filled vertical region.
    #[test]
    fn columns_render_a_minmax_band() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping minmax_col test");
            return;
        };
        let (w, h) = (64u32, 64u32);
        let target = OffscreenTarget::new(ctx.clone(), w, h);
        let pipeline = MinMaxColPipeline::new(&ctx, target.format());

        // 64 columns across x∈[0,63]; each spans y∈[20,80] within a [0,100] view
        // → a horizontal band across the middle of the image.
        let mut cols: Vec<f32> = Vec::new();
        for i in 0..64 {
            cols.extend_from_slice(&[i as f32, 20.0, 80.0]);
        }
        let buf = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("cols"),
            size: std::mem::size_of_val(cols.as_slice()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue()
            .write_buffer(&buf, 0, bytemuck::cast_slice(&cols));

        let red = [255u8, 0, 0, 255];
        let uniforms = UniformRing::new(ctx.clone(), 1);
        uniforms.write(
            0,
            &PlotUniform::from_view(
                (0.0, 63.0),
                (0.0, 100.0),
                [w as f32, h as f32],
                1.0,
                [1.0, 0.0, 0.0, 1.0],
            ),
        );
        let bind = pipeline.bind_group(&ctx, &buf, &uniforms);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("minmax-pass"),
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
            pipeline.encode(&mut pass, &bind, uniforms.dynamic_offset(0), 64);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        // Band centre (y∈[20,80] of 100 → screen rows ~13..51) is filled red;
        // the very top (y≈100 data) and bottom (y≈0) are clear.
        assert!(
            img.matches(w / 2, h / 2, red, 16),
            "band centre should be filled"
        );
        assert!(
            img.matches(w / 2, 2, [0, 0, 0, 255], 16),
            "top should be clear"
        );
        assert!(
            img.matches(w / 2, h - 3, [0, 0, 0, 255], 16),
            "bottom should be clear"
        );
    }
}

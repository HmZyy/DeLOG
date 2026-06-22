//! Offscreen 3D render target.
//!
//! The 3D view cannot draw into egui's main pass — that pass has no depth
//! attachment, so meshes would z-fight in painter order. Instead the scene
//! renders into a dedicated **4×MSAA color + depth** target in the paint
//! callback's `prepare()` phase, resolving the multisampled color into a
//! single-sample texture which `delog-app` composites as an egui image
//! (the actual `ui.image` wiring rides with the scene pane).
//!
//! Confining MSAA to this one offscreen target keeps the antialiasing cost on
//! the view that benefits, and avoids both per-widget GPU contexts and a
//! fullscreen extra pass for the 2D plots.
//!
//! This module is pure wgpu, so the same target backs headless
//! golden-image tests with no window.

use crate::context::RenderContext;
use crate::target::{RgbaImage, read_texture_rgba};

/// Multisample count for the scene target (4×MSAA).
pub const SAMPLE_COUNT: u32 = 4;

/// Color format of the resolved scene texture handed to egui / readback.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Depth format for the scene's depth attachment.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// A 4×MSAA color+depth offscreen target that resolves to a single-sample
/// color texture for compositing or readback.
pub struct Scene3dTarget {
    ctx: RenderContext,
    color_msaa: wgpu::TextureView,
    depth_msaa: wgpu::TextureView,
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Scene3dTarget {
    /// A 4×MSAA color+depth scene target of the given size. `width`/`height`
    /// are clamped to at least 1 so a zero-area widget rect cannot create an
    /// invalid texture.
    pub fn new(ctx: RenderContext, width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        let (color_msaa, depth_msaa, resolve, resolve_view) =
            Self::create_textures(&ctx, width, height);
        Self {
            ctx,
            color_msaa,
            depth_msaa,
            resolve,
            resolve_view,
            width,
            height,
        }
    }

    fn create_textures(
        ctx: &RenderContext,
        width: u32,
        height: u32,
    ) -> (
        wgpu::TextureView,
        wgpu::TextureView,
        wgpu::Texture,
        wgpu::TextureView,
    ) {
        let device = ctx.device();
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let color_msaa = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("delog-scene-color-msaa"),
                size,
                mip_level_count: 1,
                sample_count: SAMPLE_COUNT,
                dimension: wgpu::TextureDimension::D2,
                format: COLOR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth_msaa = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("delog-scene-depth-msaa"),
                size,
                mip_level_count: 1,
                sample_count: SAMPLE_COUNT,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Single-sample resolve: egui samples it (TEXTURE_BINDING) and golden
        // tests read it back (COPY_SRC); it is the MSAA resolve destination
        // (RENDER_ATTACHMENT).
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("delog-scene-resolve"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let resolve_view = resolve.create_view(&wgpu::TextureViewDescriptor::default());
        (color_msaa, depth_msaa, resolve, resolve_view)
    }

    /// Recreate the textures if the requested size changed (e.g. the scene
    /// pane was resized). No-op when the size is unchanged so steady-state
    /// frames allocate nothing.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if width == self.width && height == self.height {
            return;
        }
        let (color_msaa, depth_msaa, resolve, resolve_view) =
            Self::create_textures(&self.ctx, width, height);
        self.color_msaa = color_msaa;
        self.depth_msaa = depth_msaa;
        self.resolve = resolve;
        self.resolve_view = resolve_view;
        self.width = width;
        self.height = height;
    }

    /// Begin a render pass into the MSAA color+depth attachments, resolving
    /// the color into the single-sample texture. Clears color to `clear` and
    /// depth to `1.0` (the far plane, so any `Less` comparison passes). The
    /// caller draws the scene into the returned pass.
    pub fn begin_pass<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
        clear: wgpu::Color,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("delog-scene-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.color_msaa,
                depth_slice: None,
                resolve_target: Some(&self.resolve_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    // The MSAA color is transient — only the resolve is read —
                    // so it need not be stored.
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_msaa,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// The resolved single-sample color view — what `delog-app` registers with
    /// egui to composite the scene as an image.
    pub fn resolve_view(&self) -> &wgpu::TextureView {
        &self.resolve_view
    }

    pub fn sample_count(&self) -> u32 {
        SAMPLE_COUNT
    }

    pub fn color_format(&self) -> wgpu::TextureFormat {
        COLOR_FORMAT
    }

    pub fn depth_format(&self) -> wgpu::TextureFormat {
        DEPTH_FORMAT
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Read the resolved color texture back to CPU (blocking) — the headless
    /// golden-image path.
    pub fn read_rgba(&self) -> RgbaImage {
        read_texture_rgba(&self.ctx, &self.resolve, self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-contained test pipeline that draws one solid-color triangle at a
    /// fixed clip-space depth, multisampled to match the scene target. Used to
    /// prove depth testing and MSAA resolve on real hardware.
    struct TriPipeline {
        pipeline: wgpu::RenderPipeline,
        layout: wgpu::BindGroupLayout,
    }

    /// Per-draw uniform: three clip-space xy corners, a depth, and a color.
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct TriUniform {
        // Three corners as xy pairs, padded to vec4 for std140 alignment.
        p0: [f32; 4],
        p1: [f32; 4],
        p2: [f32; 4],
        depth: [f32; 4], // x = depth, rest padding
        color: [f32; 4],
    }

    const TRI_WGSL: &str = r#"
struct Tri {
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    depth: vec4<f32>,
    color: vec4<f32>,
};
@group(0) @binding(0) var<uniform> tri: Tri;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var xy = tri.p0.xy;
    if (vi == 1u) { xy = tri.p1.xy; }
    if (vi == 2u) { xy = tri.p2.xy; }
    return vec4<f32>(xy, tri.depth.x, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return tri.color;
}
"#;

    impl TriPipeline {
        fn new(ctx: &RenderContext) -> Self {
            let device = ctx.device();
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("test-tri"),
                source: wgpu::ShaderSource::Wgsl(TRI_WGSL.into()),
            });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("test-tri-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("test-tri-pl"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("test-tri-pipeline"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: SAMPLE_COUNT,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            });
            Self { pipeline, layout }
        }

        fn draw(&self, ctx: &RenderContext, pass: &mut wgpu::RenderPass<'_>, u: &TriUniform) {
            let buf = ctx.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("test-tri-uniform"),
                size: std::mem::size_of::<TriUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ctx.queue().write_buffer(&buf, 0, bytemuck::bytes_of(u));
            let bind = ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test-tri-bind"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
            // `bind`/`buf` drop here, after the encoded draw — wgpu retains the
            // resources it references until submission, so this is sound.
        }
    }

    /// Clearing the MSAA target and resolving yields the clear color in
    /// every pixel of the single-sample resolve texture — proves the resolve
    /// path and readback are wired.
    #[test]
    fn clear_resolves_through_msaa() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping scene-target test");
            return;
        };
        let (w, h) = (32u32, 32u32);
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        assert_eq!(target.sample_count(), 4);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let clear = wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            };
            let _pass = target.begin_pass(&mut enc, clear);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        let blue = [0u8, 0, 255, 255];
        assert_eq!(
            img.count_matching(blue, 2),
            (w * h) as usize,
            "every resolved pixel should be the clear color"
        );
    }

    /// Depth testing rejects geometry drawn later but farther away.
    /// A near red triangle is drawn first, then a far green triangle covering
    /// the same area; with depth-compare `Less` the green is rejected, so the
    /// overlap stays red. (Without a working depth buffer, the later green
    /// draw would win.)
    #[test]
    fn depth_test_rejects_farther_geometry() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (32u32, 32u32);
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let tri = TriPipeline::new(&ctx);

        // A big triangle covering the center of the framebuffer.
        let big = ([-1.0f32, -1.0], [3.0f32, -1.0], [-1.0f32, 3.0]);
        let near = TriUniform {
            p0: [big.0[0], big.0[1], 0.0, 0.0],
            p1: [big.1[0], big.1[1], 0.0, 0.0],
            p2: [big.2[0], big.2[1], 0.0, 0.0],
            depth: [0.2, 0.0, 0.0, 0.0],
            color: [1.0, 0.0, 0.0, 1.0], // red, near
        };
        let far = TriUniform {
            depth: [0.8, 0.0, 0.0, 0.0],
            color: [0.0, 1.0, 0.0, 1.0], // green, far
            ..near
        };

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, wgpu::Color::BLACK);
            tri.draw(&ctx, &mut pass, &near);
            tri.draw(&ctx, &mut pass, &far);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        let (cx, cy) = (w / 2, h / 2);
        assert!(
            img.matches(cx, cy, [255, 0, 0, 255], 4),
            "near red should survive; far green must be depth-rejected, got {:?}",
            img.pixel(cx, cy)
        );
    }

    /// 4×MSAA antialiases a slanted triangle edge — a pixel straddling
    /// the edge resolves to partial coverage (strictly between the clear color
    /// and the fill color), which a single-sample target cannot produce.
    #[test]
    fn msaa_smooths_a_triangle_edge() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (64u32, 64u32);
        let target = Scene3dTarget::new(ctx.clone(), w, h);
        let tri = TriPipeline::new(&ctx);

        // A triangle whose hypotenuse runs along the main diagonal of clip
        // space: covers the lower-left half, leaving a slanted edge.
        let u = TriUniform {
            p0: [-1.0, -1.0, 0.0, 0.0],
            p1: [1.0, -1.0, 0.0, 0.0],
            p2: [-1.0, 1.0, 0.0, 0.0],
            depth: [0.5, 0.0, 0.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0], // white fill on black clear
        };

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = target.begin_pass(&mut enc, wgpu::Color::BLACK);
            tri.draw(&ctx, &mut pass, &u);
        }
        ctx.queue().submit([enc.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let img = target.read_rgba();
        // Scan the anti-diagonal (the edge) for a partially-covered pixel:
        // not pure black, not pure white.
        let mut found_blend = false;
        for k in 1..(w - 1) {
            let (x, y) = (k, k); // row index from top; edge runs corner-to-corner
            let p = img.pixel(x, y);
            let g = p[1];
            if g > 16 && g < 239 {
                found_blend = true;
                break;
            }
        }
        assert!(
            found_blend,
            "expected at least one partially-covered (anti-aliased) edge pixel"
        );
    }
}

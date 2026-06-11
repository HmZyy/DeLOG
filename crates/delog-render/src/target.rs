//! Offscreen render target + readback — the headless golden-image rig
//! (PLAN.md §20.3, GPU-13).
//!
//! Lets tests, benches and (later) image export drive the renderer with no
//! window: render into an RGBA texture, then read the pixels back to CPU. Row
//! readback honours wgpu's 256-byte `bytes_per_row` alignment and unpads to a
//! tight RGBA buffer.

use crate::context::RenderContext;

/// Bytes wgpu requires each copied texture row to align to.
const COPY_ALIGN: u32 = 256;

fn align_up(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

/// A tight, row-major RGBA8 image read back from the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// The RGBA bytes of one pixel.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Whether `pixel(x, y)` matches `rgba` within `±tol` per channel.
    pub fn matches(&self, x: u32, y: u32, rgba: [u8; 4], tol: u8) -> bool {
        let p = self.pixel(x, y);
        (0..4).all(|c| p[c].abs_diff(rgba[c]) <= tol)
    }

    /// Count of pixels matching `rgba` within `±tol` per channel.
    pub fn count_matching(&self, rgba: [u8; 4], tol: u8) -> usize {
        self.pixels
            .chunks_exact(4)
            .filter(|p| (0..4).all(|c| p[c].abs_diff(rgba[c]) <= tol))
            .count()
    }
}

/// An offscreen color texture the renderer draws into.
pub struct OffscreenTarget {
    ctx: RenderContext,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

impl OffscreenTarget {
    /// An `RGBA8Unorm` render target of the given size.
    pub fn new(ctx: RenderContext, width: u32, height: u32) -> Self {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("delog-offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            ctx,
            texture,
            view,
            width,
            height,
            format,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Copy the texture to CPU and return tight RGBA bytes (blocking).
    pub fn read_rgba(&self) -> RgbaImage {
        let padded_bpr = align_up(self.width * 4, COPY_ALIGN);
        let buffer = self.ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-offscreen-readback"),
            size: (padded_bpr * self.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc = self
            .ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue().submit([enc.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.ctx
            .device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let tight_bpr = (self.width * 4) as usize;
        let mut pixels = Vec::with_capacity(tight_bpr * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * padded_bpr as usize;
            pixels.extend_from_slice(&data[start..start + tight_bpr]);
        }
        RgbaImage {
            width: self.width,
            height: self.height,
            pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffers::BufferManager;
    use crate::line::LinePipeline;
    use crate::uniforms::{PlotUniform, UniformRing};
    use delog_core::identity::FieldId;

    /// GPU-13: render a known horizontal trace and verify the rendered pixels.
    /// Tolerance/property based rather than byte-exact so it is portable across
    /// drivers (rasterisation differs subtly between GPUs).
    #[test]
    fn golden_horizontal_line_renders_to_expected_pixels() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping golden-image test");
            return;
        };

        let (w, h) = (64u32, 64u32);
        let target = OffscreenTarget::new(ctx.clone(), w, h);
        let pipeline = LinePipeline::new(&ctx, target.format());

        // A flat trace at y = 0 spanning the full x range, identity transform:
        // x in [-1, 1] → full width; y = 0 → the middle row.
        let mut buffers = BufferManager::new(ctx.clone());
        let field = FieldId(0);
        buffers.sync(field, &[-1.0, 0.0, 1.0, 0.0], false);

        let red = [255u8, 0, 0, 255];
        let uniforms = UniformRing::new(ctx.clone(), 1);
        uniforms.write(
            0,
            &PlotUniform::new(
                1.0,
                0.0,
                1.0,
                0.0,
                [w as f32, h as f32],
                4.0,
                [1.0, 0.0, 0.0, 1.0],
            ),
        );
        let bind = pipeline.bind_group(&ctx, buffers.buffer(field).unwrap(), &uniforms);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("golden-pass"),
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

        // The line crosses the vertical centre; corners stay the clear colour.
        let cx = w / 2;
        assert!(
            img.matches(cx, h / 2, red, 8),
            "centre pixel should be the trace colour, got {:?}",
            img.pixel(cx, h / 2)
        );
        assert!(
            img.matches(0, 0, [0, 0, 0, 255], 8),
            "top-left corner should be the clear colour, got {:?}",
            img.pixel(0, 0)
        );
        // A ~4px line across 64px ≈ 256 red pixels; assert a sane band exists.
        let red_pixels = img.count_matching(red, 8);
        assert!(
            (128..1024).contains(&red_pixels),
            "expected a thin red band, got {red_pixels} red pixels"
        );
    }

    /// A windowed (non-identity) transform places the trace at the expected
    /// screen position — proving `PlotUniform::from_view` on real hardware.
    #[test]
    fn from_view_positions_a_trace_in_the_upper_quarter() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let (w, h) = (64u32, 64u32);
        let target = OffscreenTarget::new(ctx.clone(), w, h);
        let pipeline = LinePipeline::new(&ctx, target.format());

        // Flat trace at data y = 75 with the visible window y ∈ [0, 100]:
        // clip y = 0.5 → screen row ≈ h/4 (upper quarter), x spans the width.
        let mut buffers = BufferManager::new(ctx.clone());
        let field = FieldId(0);
        buffers.sync(field, &[0.0, 75.0, 10.0, 75.0], false);

        let red = [255u8, 0, 0, 255];
        let uniforms = UniformRing::new(ctx.clone(), 1);
        uniforms.write(
            0,
            &PlotUniform::from_view(
                (0.0, 10.0),
                (0.0, 100.0),
                [w as f32, h as f32],
                4.0,
                [1.0, 0.0, 0.0, 1.0],
            ),
        );
        let bind = pipeline.bind_group(&ctx, buffers.buffer(field).unwrap(), &uniforms);

        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("from-view-pass"),
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
        let cx = w / 2;
        assert!(
            img.matches(cx, h / 4, red, 8),
            "trace should sit in the upper quarter, got {:?}",
            img.pixel(cx, h / 4)
        );
        assert!(
            img.matches(cx, h / 2, [0, 0, 0, 255], 8),
            "centre should be clear, got {:?}",
            img.pixel(cx, h / 2)
        );
    }
}

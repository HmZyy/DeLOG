//! Per-plot transform/style uniforms (PLAN.md §9.3, Appendix B, GPU-04).
//!
//! One uniform buffer holds every plot's [`PlotUniform`] at a 256-aligned
//! stride; a draw selects its plot with a **dynamic offset** rather than push
//! constants (which aren't universally supported, §9.3). The shader reads three
//! `vec4`s: the data→clip transform, the viewport + line width, and the colour.

use crate::context::RenderContext;

/// Per-plot uniform, laid out as three `vec4<f32>` (std140-clean, 48 bytes).
///
/// `transform = [x_scale, x_offset, y_scale, y_offset]` maps data to clip space
/// as `clip = data * scale + offset`. `view = [viewport_w, viewport_h, width_px,
/// _pad]`. `color` is linear RGBA.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PlotUniform {
    pub transform: [f32; 4],
    pub view: [f32; 4],
    pub color: [f32; 4],
}

impl PlotUniform {
    pub fn new(
        x_scale: f32,
        x_offset: f32,
        y_scale: f32,
        y_offset: f32,
        viewport: [f32; 2],
        width_px: f32,
        color: [f32; 4],
    ) -> Self {
        Self {
            transform: [x_scale, x_offset, y_scale, y_offset],
            view: [viewport[0], viewport[1], width_px, 0.0],
            color,
        }
    }

    /// Build the uniform from a visible data window. `x`/`y` are the inclusive
    /// data ranges mapped to clip `[-1, 1]` (y increasing upward); a degenerate
    /// range collapses to scale 0 (everything maps to the centre).
    pub fn from_view(
        x: (f32, f32),
        y: (f32, f32),
        viewport: [f32; 2],
        width_px: f32,
        color: [f32; 4],
    ) -> Self {
        let (x_scale, x_offset) = axis(x.0, x.1);
        let (y_scale, y_offset) = axis(y.0, y.1);
        Self::new(
            x_scale, x_offset, y_scale, y_offset, viewport, width_px, color,
        )
    }
}

/// Scale/offset mapping data `[min, max]` to clip `[-1, 1]`
/// (`clip = data * scale + offset`).
fn axis(min: f32, max: f32) -> (f32, f32) {
    let span = max - min;
    if span.abs() <= f32::EPSILON {
        return (0.0, 0.0); // degenerate window → centre
    }
    let scale = 2.0 / span;
    (scale, -1.0 - min * scale)
}

const UNIFORM_SIZE: u64 = std::mem::size_of::<PlotUniform>() as u64;

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

/// A dynamic-offset uniform buffer with one slot per plot.
pub struct UniformRing {
    ctx: RenderContext,
    buf: wgpu::Buffer,
    stride: u64,
    capacity: u32,
}

impl UniformRing {
    /// Allocate room for `capacity` plot slots, each at the device's required
    /// dynamic-offset alignment.
    pub fn new(ctx: RenderContext, capacity: u32) -> Self {
        let align = ctx.device().limits().min_uniform_buffer_offset_alignment as u64;
        let stride = align_up(UNIFORM_SIZE, align.max(1));
        let buf = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-plot-uniforms"),
            size: stride * capacity.max(1) as u64,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        Self {
            ctx,
            buf,
            stride,
            capacity: capacity.max(1),
        }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Write one plot's uniform into `slot` (`slot < capacity`).
    pub fn write(&self, slot: u32, uniform: &PlotUniform) {
        debug_assert!(slot < self.capacity, "uniform slot out of range");
        self.ctx.queue().write_buffer(
            &self.buf,
            slot as u64 * self.stride,
            bytemuck::bytes_of(uniform),
        );
    }

    /// The dynamic offset (bytes) to bind `slot` in a draw call.
    pub fn dynamic_offset(&self, slot: u32) -> u32 {
        (slot as u64 * self.stride) as u32
    }

    /// The bind-group layout entry for the uniform at `binding` (dynamic offset).
    pub fn layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
            },
            count: None,
        }
    }

    /// The binding resource for a bind group: one plot slot's worth, selected
    /// at draw time by the dynamic offset.
    pub fn binding_resource(&self) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &self.buf,
            offset: 0,
            size: wgpu::BufferSize::new(UNIFORM_SIZE),
        })
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buf
    }

    pub fn stride(&self) -> u64 {
        self.stride
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_slot(ctx: &RenderContext, ring: &UniformRing, slot: u32) -> PlotUniform {
        let staging = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("u-readback"),
            size: UNIFORM_SIZE,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(
            ring.buffer(),
            slot as u64 * ring.stride(),
            &staging,
            0,
            UNIFORM_SIZE,
        );
        ctx.queue().submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range();
        *bytemuck::from_bytes::<PlotUniform>(&data)
    }

    #[test]
    fn uniform_is_three_vec4s() {
        assert_eq!(UNIFORM_SIZE, 48);
    }

    #[test]
    fn from_view_maps_window_corners_to_clip() {
        let u = PlotUniform::from_view((0.0, 10.0), (-100.0, 100.0), [1.0, 1.0], 1.0, [0.0; 4]);
        let clip = |data: f32, scale: f32, offset: f32| data * scale + offset;
        // x: 0 → -1, 10 → +1.
        assert!((clip(0.0, u.transform[0], u.transform[1]) + 1.0).abs() < 1e-5);
        assert!((clip(10.0, u.transform[0], u.transform[1]) - 1.0).abs() < 1e-5);
        // y: -100 → -1, 100 → +1, 0 → 0 (centre).
        assert!((clip(-100.0, u.transform[2], u.transform[3]) + 1.0).abs() < 1e-5);
        assert!((clip(100.0, u.transform[2], u.transform[3]) - 1.0).abs() < 1e-5);
        assert!(clip(0.0, u.transform[2], u.transform[3]).abs() < 1e-5);
    }

    #[test]
    fn from_view_handles_a_degenerate_window() {
        let u = PlotUniform::from_view((5.0, 5.0), (5.0, 5.0), [1.0, 1.0], 1.0, [0.0; 4]);
        assert_eq!(u.transform, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn slots_are_aligned_and_independently_addressable() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping uniform ring test");
            return;
        };
        let ring = UniformRing::new(ctx.clone(), 4);

        // Dynamic offsets are multiples of the (≥256) alignment.
        let align = ctx.device().limits().min_uniform_buffer_offset_alignment;
        assert_eq!(ring.dynamic_offset(0), 0);
        assert_eq!(ring.dynamic_offset(1) % align, 0);
        assert!(ring.dynamic_offset(1) >= align);

        let a = PlotUniform::new(
            2.0,
            -1.0,
            -2.0,
            1.0,
            [800.0, 600.0],
            1.5,
            [1.0, 0.0, 0.0, 1.0],
        );
        let b = PlotUniform::new(
            0.5,
            0.0,
            0.5,
            0.0,
            [640.0, 480.0],
            2.0,
            [0.0, 1.0, 0.0, 1.0],
        );
        ring.write(0, &a);
        ring.write(2, &b);

        assert_eq!(read_slot(&ctx, &ring, 0), a);
        assert_eq!(read_slot(&ctx, &ring, 2), b);
    }
}

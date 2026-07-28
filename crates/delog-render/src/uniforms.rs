//! Per-plot transform/style uniforms.
//!
//! A draw selects its plot via a dynamic offset rather than push constants
//! (not universally supported).

use crate::context::RenderContext;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PlotUniform {
    /// `[x_scale, x_min, y_scale, y_min]`. Clip = `(data - min) * scale - 1`.
    /// Subtracting the view min first keeps full f32 precision for
    /// large-magnitude coordinates (e.g. lat/lon in 1e-7 deg), where the old
    /// `data * scale + offset` form catastrophically cancelled.
    pub transform: [f32; 4],
    pub view: [f32; 4],
    pub color: [f32; 4],
    /// x: gap mode (`GAP_*`), y: x-units gap threshold (0 = off), z/w unused.
    pub gap: [f32; 4],
}

pub const GAP_CONNECT: u32 = 0;
pub const GAP_CUT: u32 = 1;
pub const GAP_DOTTED: u32 = 2;
pub const GAP_FORCE_DASH: u32 = 3;

impl PlotUniform {
    pub fn new(
        x_scale: f32,
        x_min: f32,
        y_scale: f32,
        y_min: f32,
        viewport: [f32; 2],
        width_px: f32,
        color: [f32; 4],
    ) -> Self {
        Self {
            transform: [x_scale, x_min, y_scale, y_min],
            view: [viewport[0], viewport[1], width_px, 0.0],
            color,
            gap: [0.0; 4],
        }
    }

    pub fn from_view(
        x: (f32, f32),
        y: (f32, f32),
        viewport: [f32; 2],
        width_px: f32,
        color: [f32; 4],
    ) -> Self {
        let (x_scale, x_min) = axis(x.0, x.1);
        let (y_scale, y_min) = axis(y.0, y.1);
        Self::new(x_scale, x_min, y_scale, y_min, viewport, width_px, color)
    }

    /// Build a plot transform whose samples are translated horizontally while
    /// the visible data window remains unchanged. A positive shift moves a
    /// sample to the right.
    pub fn from_view_with_x_shift(
        x: (f32, f32),
        y: (f32, f32),
        viewport: [f32; 2],
        width_px: f32,
        color: [f32; 4],
        x_shift: f32,
    ) -> Self {
        let mut uniform = Self::from_view(x, y, viewport, width_px, color);
        uniform.transform[1] -= x_shift;
        uniform
    }

    /// Edge anti-alias feather, stored in `view.w`.
    pub fn with_aa(mut self, aa: f32) -> Self {
        self.view[3] = aa.max(0.0);
        self
    }

    /// Gap mode and x-units delta threshold, stored in `gap.xy`.
    pub fn with_gap(mut self, mode: u32, threshold: f32) -> Self {
        self.gap[0] = mode as f32;
        self.gap[1] = threshold.max(0.0);
        self
    }

    /// Overwrite the Y transform with a precomputed scale and rebased min, so
    /// the caller can subtract a per-trace origin in f64 before narrowing.
    pub fn with_y_axis(mut self, y_scale: f32, y_min: f32) -> Self {
        self.transform[2] = y_scale;
        self.transform[3] = y_min;
        self
    }
}

/// Returns `(scale, min)`: clip = `(data - min) * scale - 1`, mapping
/// `[min, max]` onto `[-1, 1]`. Keeping `min` (rather than the pre-multiplied
/// offset `-1 - min*scale`) avoids f32 cancellation at large magnitudes.
fn axis(min: f32, max: f32) -> (f32, f32) {
    let span = max - min;
    if span.abs() <= f32::EPSILON {
        return (0.0, 0.0);
    }
    (2.0 / span, min)
}

const UNIFORM_SIZE: u64 = std::mem::size_of::<PlotUniform>() as u64;

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

pub struct UniformRing {
    ctx: RenderContext,
    buf: wgpu::Buffer,
    stride: u64,
    capacity: u32,
}

impl UniformRing {
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

    pub fn write(&self, slot: u32, uniform: &PlotUniform) {
        debug_assert!(slot < self.capacity, "uniform slot out of range");
        self.ctx.queue().write_buffer(
            &self.buf,
            slot as u64 * self.stride,
            bytemuck::bytes_of(uniform),
        );
    }

    pub fn dynamic_offset(&self, slot: u32) -> u32 {
        (slot as u64 * self.stride) as u32
    }

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
    fn uniform_is_four_vec4s() {
        assert_eq!(UNIFORM_SIZE, 64);
    }

    // Mirrors the shader's data_to_clip: (data - min) * scale - 1.
    fn clip(data: f32, scale: f32, min: f32) -> f32 {
        (data - min) * scale - 1.0
    }

    #[test]
    fn from_view_maps_window_corners_to_clip() {
        let u = PlotUniform::from_view((0.0, 10.0), (-100.0, 100.0), [1.0, 1.0], 1.0, [0.0; 4]);
        assert!((clip(0.0, u.transform[0], u.transform[1]) + 1.0).abs() < 1e-5);
        assert!((clip(10.0, u.transform[0], u.transform[1]) - 1.0).abs() < 1e-5);
        assert!((clip(-100.0, u.transform[2], u.transform[3]) + 1.0).abs() < 1e-5);
        assert!((clip(100.0, u.transform[2], u.transform[3]) - 1.0).abs() < 1e-5);
        assert!(clip(0.0, u.transform[2], u.transform[3]).abs() < 1e-5);
    }

    #[test]
    fn x_shift_moves_trace_without_changing_view() {
        let u = PlotUniform::from_view_with_x_shift(
            (0.0, 10.0),
            (-1.0, 1.0),
            [1.0, 1.0],
            1.0,
            [0.0; 4],
            2.0,
        );
        let base = PlotUniform::from_view((0.0, 10.0), (-1.0, 1.0), [1.0, 1.0], 1.0, [0.0; 4]);
        assert_eq!(u.transform[2..], base.transform[2..]);
        assert!(
            (clip(0.0, u.transform[0], u.transform[1])
                - clip(2.0, base.transform[0], base.transform[1]))
            .abs()
                < 1e-6
        );
    }

    #[test]
    fn with_y_axis_overrides_only_the_y_transform() {
        let base = PlotUniform::from_view((0.0, 10.0), (0.0, 1.0), [1.0, 1.0], 0.0, [0.0; 4]);
        let u = base.with_y_axis(0.5, -3.0);
        // x transform untouched
        assert_eq!(u.transform[0], base.transform[0]);
        assert_eq!(u.transform[1], base.transform[1]);
        // y transform replaced; clip = (data - min) * scale - 1
        assert_eq!(u.transform[2], 0.5);
        assert_eq!(u.transform[3], -3.0);
        assert!((clip(-3.0, u.transform[2], u.transform[3]) + 1.0).abs() < 1e-6);
        assert!((clip(1.0, u.transform[2], u.transform[3]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rebased_y_axis_keeps_a_sub_ulp_latitude_sample_at_its_geometric_position() {
        let y_origin = 437_129_284.25_f64;
        let (y0, y1) = (437_129_280.25_f64, 437_129_290.25_f64);
        let sample = 437_129_286.75_f64;
        let y_scale = (2.0 / (y1 - y0)) as f32;
        let y_min_rebased = (y0 - y_origin) as f32;
        let sample_rebased = (sample - y_origin) as f32;

        let u = PlotUniform::from_view((0.0, 10.0), (0.0, 1.0), [1.0, 1.0], 0.0, [0.0; 4])
            .with_y_axis(y_scale, y_min_rebased);
        let expected = (((sample - y0) / (y1 - y0)) * 2.0 - 1.0) as f32;

        assert!((clip(sample_rebased, u.transform[2], u.transform[3]) - expected).abs() < 1e-6);
    }

    #[test]
    fn transform_keeps_precision_at_large_magnitude() {
        // Latitude scale (~4.37e8): the old data*scale+offset form cancelled to
        // ~0 (everything at the view middle); (data-min)*scale-1 stays precise.
        let u = PlotUniform::from_view(
            (0.0, 1.0),
            (437129280.0, 437129380.0),
            [1.0, 1.0],
            0.0,
            [0.0; 4],
        );
        let (ys, ymin) = (u.transform[2], u.transform[3]);
        let v = 437129312.0f32; // representable; 32 above the view min
        // (v - min) is exact in f32, so the mapping is the true rebased clip,
        // placing v in the lower third — not cancelled toward the middle (0).
        let got = clip(v, ys, ymin);
        assert!((got - (32.0f32 * ys - 1.0)).abs() < 1e-5);
        assert!(
            (-0.5..-0.2).contains(&got),
            "not cancelled to middle: {got}"
        );

        // The old formula cancelled: value*scale + (-1 - min*scale) ~ 0.
        let old = v * ys + (-1.0f32 - 437129280.0f32 * ys);
        assert!(old.abs() < 0.1, "old form should cancel to ~0, was {old}");
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

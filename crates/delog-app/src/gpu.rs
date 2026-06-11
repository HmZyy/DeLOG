//! egui/eframe adapter for DeLOG's pure-wgpu renderer (PLAN.md §9.1-§9.2,
//! GPU-06, PLT-02/03).
//!
//! `delog-render` contains no egui types; this module is the thin boundary. It
//! adopts eframe's `wgpu` device/queue, keeps the pipeline + buffer/uniform
//! managers in egui_wgpu's callback-resource map, and each frame uploads the
//! ready trace caches, writes per-plot uniforms, and emits one paint callback
//! that draws every trace inside egui's main pass with a per-plot viewport +
//! scissor.

use std::collections::HashMap;
use std::sync::Arc;

use delog_cache::{CacheManager, MinMax};
use delog_core::identity::FieldId;
use delog_render::{
    BufferManager, LinePipeline, MinMaxColPipeline, PlotUniform, RenderContext, UniformRing,
};
use eframe::{egui_wgpu, wgpu};

use crate::plot::{PlotPane, ViewX};

/// Switch to the decimated min/max path above this many samples per pixel
/// (§9.5, GPU-10).
const DECIMATE_THRESHOLD: f32 = 8.0;

/// The inner plot rect plus the visible data window the GPU and the egui axes
/// share (so labels line up with the rendered lines).
#[derive(Clone, Copy)]
pub struct PaneView {
    pub rect: egui::Rect,
    pub x_range: (f32, f32),
    pub y_range: (f32, f32),
}

/// App-owned handle to the renderer resources stored in egui_wgpu.
#[derive(Clone, Copy, Debug)]
pub struct GpuBridge {
    available: bool,
}

impl GpuBridge {
    pub fn from_creation_context(cc: &eframe::CreationContext<'_>) -> Self {
        let Some(render_state) = &cc.wgpu_render_state else {
            return Self { available: false };
        };

        let ctx = RenderContext::new(
            Arc::new(render_state.device.clone()),
            Arc::new(render_state.queue.clone()),
        );
        let resources = PlotCallbackResources::new(ctx, render_state.target_format);
        render_state
            .renderer
            .write()
            .callback_resources
            .insert(resources);

        Self { available: true }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Upload the pane's ready trace caches into the `plot_rect`, write their
    /// uniforms for the given visible data window, and emit the paint callback.
    /// The caller supplies the X/Y ranges so the egui axes share them exactly.
    pub fn render_pane(
        &self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        caches: &mut CacheManager,
        pane: &PlotPane,
        view: PaneView,
    ) {
        let plot_rect = view.rect;
        if !self.available || plot_rect.width() < 2.0 || plot_rect.height() < 2.0 {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let ppp = ui.ctx().pixels_per_point();
        let viewport_px = [
            (plot_rect.width() * ppp).max(1.0),
            (plot_rect.height() * ppp).max(1.0),
        ];
        let (x0, x1) = view.x_range;
        let (y0, y1) = view.y_range;

        let mut items = Vec::new();
        {
            let mut renderer = render_state.renderer.write();
            let Some(res) = renderer
                .callback_resources
                .get_mut::<PlotCallbackResources>()
            else {
                return;
            };
            res.ensure_uniform_capacity(pane.traces.len() as u32);
            let plot_w = viewport_px[0];

            for (slot, trace) in pane.visible_traces().enumerate() {
                let Some(cache) = caches.get(trace.field) else {
                    continue;
                };
                res.uniforms.write(
                    slot as u32,
                    &PlotUniform::from_view(
                        (x0, x1),
                        (y0, y1),
                        viewport_px,
                        trace.width_px,
                        trace.color,
                    ),
                );

                // Draw-path selector (GPU-10): decimate when the visible window
                // packs more than ~8 samples per pixel.
                let (a, b) = cache.index_range(x0, x1);
                let visible = b.saturating_sub(a) as f32;
                let kind = if plot_w >= 1.0 && visible / plot_w > DECIMATE_THRESHOLD {
                    let width = plot_w as usize;
                    let cols = cache.minmax_columns(x0, x1, width);
                    res.col_buffers.sync(trace.field, &cols, true);
                    DrawKind::Columns {
                        count: width as u32,
                    }
                } else {
                    res.buffers.sync(trace.field, &cache.xy, false);
                    DrawKind::Line {
                        samples: res.buffers.samples(trace.field) as u32,
                    }
                };

                if kind.is_drawable() {
                    items.push(DrawItem {
                        field: trace.field,
                        slot: slot as u32,
                        kind,
                    });
                }
            }

            // Drop GPU buffers for fields no longer plotted.
            let plotted: Vec<FieldId> = pane.fields().collect();
            res.retain_buffers(&plotted);
        }

        if items.is_empty() {
            return;
        }
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            plot_rect,
            ScenePaintCallback { items },
        ));
    }
}

/// Union of every visible trace's auto-Y range, padded; a sane default when no
/// finite samples are in view (PLT-06 AutoVisible).
pub fn visible_y_range(caches: &mut CacheManager, pane: &PlotPane, x0: f32, x1: f32) -> (f32, f32) {
    let mut mm = MinMax::EMPTY;
    for trace in pane.visible_traces() {
        if let Some(cache) = caches.get(trace.field) {
            mm = mm.merge(cache.y_range(x0, x1));
        }
    }
    if !mm.is_finite() {
        return (-1.0, 1.0);
    }
    let (min, max) = (mm.min, mm.max);
    if (max - min).abs() <= f32::EPSILON {
        return (min - 1.0, max + 1.0);
    }
    let pad = (max - min) * 0.05;
    (min - pad, max + pad)
}

/// How one trace is drawn this frame (GPU-10).
#[derive(Clone, Copy)]
enum DrawKind {
    /// Full polyline: `samples` `[x,y]` pairs.
    Line { samples: u32 },
    /// Decimated: `count` per-pixel min/max columns.
    Columns { count: u32 },
}

impl DrawKind {
    fn is_drawable(self) -> bool {
        match self {
            DrawKind::Line { samples } => samples >= 2,
            DrawKind::Columns { count } => count >= 1,
        }
    }
}

struct DrawItem {
    field: FieldId,
    slot: u32,
    kind: DrawKind,
}

struct PlotCallbackResources {
    ctx: RenderContext,
    line: LinePipeline,
    minmax: MinMaxColPipeline,
    /// Interleaved `[x,y]` trace buffers (full path).
    buffers: BufferManager,
    /// Transient `[x,min,max]` column buffers (decimated path).
    col_buffers: BufferManager,
    uniforms: UniformRing,
    line_binds: HashMap<FieldId, wgpu::BindGroup>,
    col_binds: HashMap<FieldId, wgpu::BindGroup>,
}

impl PlotCallbackResources {
    fn new(ctx: RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let line = LinePipeline::new(&ctx, color_format);
        let minmax = MinMaxColPipeline::new(&ctx, color_format);
        let buffers = BufferManager::new(ctx.clone());
        let col_buffers = BufferManager::new(ctx.clone());
        let uniforms = UniformRing::new(ctx.clone(), 8);
        Self {
            ctx,
            line,
            minmax,
            buffers,
            col_buffers,
            uniforms,
            line_binds: HashMap::new(),
            col_binds: HashMap::new(),
        }
    }

    /// Grow the uniform ring if more plots than slots are needed.
    fn ensure_uniform_capacity(&mut self, needed: u32) {
        if needed > self.uniforms.capacity() {
            self.uniforms = UniformRing::new(self.ctx.clone(), needed.next_power_of_two());
        }
    }

    fn retain_buffers(&mut self, plotted: &[FieldId]) {
        let stale: Vec<FieldId> = self
            .buffers
            .fields()
            .chain(self.col_buffers.fields())
            .filter(|f| !plotted.contains(f))
            .collect();
        for field in stale {
            self.buffers.remove(field);
            self.col_buffers.remove(field);
        }
    }
}

struct ScenePaintCallback {
    items: Vec<DrawItem>,
}

impl egui_wgpu::CallbackTrait for ScenePaintCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = callback_resources.get_mut::<PlotCallbackResources>() {
            // Rebuild bind groups against the (possibly grown) buffers.
            let PlotCallbackResources {
                ctx,
                line,
                minmax,
                buffers,
                col_buffers,
                uniforms,
                line_binds,
                col_binds,
            } = res;
            line_binds.clear();
            col_binds.clear();
            for item in &self.items {
                match item.kind {
                    DrawKind::Line { .. } => {
                        if let Some(buf) = buffers.buffer(item.field) {
                            line_binds.insert(item.field, line.bind_group(ctx, buf, uniforms));
                        }
                    }
                    DrawKind::Columns { .. } => {
                        if let Some(buf) = col_buffers.buffer(item.field) {
                            col_binds.insert(item.field, minmax.bind_group(ctx, buf, uniforms));
                        }
                    }
                }
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(res) = callback_resources.get::<PlotCallbackResources>() else {
            return;
        };

        let viewport = info.viewport_in_pixels();
        if viewport.width_px <= 0 || viewport.height_px <= 0 {
            return;
        }
        let clip = info.clip_rect_in_pixels();
        let Some((sx, sy, sw, sh)) = intersect_scissor_rect(
            (
                viewport.left_px,
                viewport.top_px,
                viewport.width_px,
                viewport.height_px,
            ),
            (clip.left_px, clip.top_px, clip.width_px, clip.height_px),
            info.screen_size_px,
        ) else {
            return;
        };

        // Map clip space to the plot rect; scissor to the visible intersection.
        render_pass.set_viewport(
            viewport.left_px.max(0) as f32,
            viewport.top_px.max(0) as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        render_pass.set_scissor_rect(sx, sy, sw, sh);

        for item in &self.items {
            let offset = res.uniforms.dynamic_offset(item.slot);
            match item.kind {
                DrawKind::Line { samples } => {
                    if let Some(bind) = res.line_binds.get(&item.field) {
                        res.line.encode_trace(render_pass, bind, offset, samples);
                    }
                }
                DrawKind::Columns { count } => {
                    if let Some(bind) = res.col_binds.get(&item.field) {
                        res.minmax.encode(render_pass, bind, offset, count);
                    }
                }
            }
        }
    }
}

fn intersect_scissor_rect(
    viewport: (i32, i32, i32, i32),
    clip: (i32, i32, i32, i32),
    screen: [u32; 2],
) -> Option<(u32, u32, u32, u32)> {
    if viewport.2 <= 0 || viewport.3 <= 0 || clip.2 <= 0 || clip.3 <= 0 {
        return None;
    }
    let left = viewport.0.max(clip.0).max(0);
    let top = viewport.1.max(clip.1).max(0);
    let right = (viewport.0 + viewport.2)
        .min(clip.0 + clip.2)
        .min(screen[0] as i32);
    let bottom = (viewport.1 + viewport.3)
        .min(clip.1 + clip.3)
        .min(screen[1] as i32);
    (right > left && bottom > top).then_some((
        left as u32,
        top as u32,
        (right - left) as u32,
        (bottom - top) as u32,
    ))
}

/// Convert an egui drag delta and a wheel scroll into [`ViewX`] updates,
/// mapping screen pixels to the data window. Pure so it stays unit-testable.
pub fn apply_pan(view: &mut ViewX, drag_dx_px: f32, rect_width_px: f32) {
    if rect_width_px <= 0.0 {
        return;
    }
    let span = view.span_us() as f64;
    let delta = -(drag_dx_px as f64 / rect_width_px as f64) * span;
    view.pan_us(delta.round() as i64);
}

/// Zoom about the cursor. `cursor_frac` is the cursor's 0..1 position across the
/// plot width; `scroll` is the wheel delta (positive = zoom in).
pub fn apply_zoom(view: &mut ViewX, cursor_frac: f32, scroll: f32) {
    if scroll == 0.0 {
        return;
    }
    let focus = view.min_us + (view.span_us() as f64 * cursor_frac.clamp(0.0, 1.0) as f64) as i64;
    let factor = (1.0015_f64).powf(-scroll as f64);
    view.zoom_at(focus, factor);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scissor_is_viewport_clip_intersection_clamped_to_screen() {
        assert_eq!(
            intersect_scissor_rect((10, 20, 100, 80), (50, 0, 70, 50), [200, 200]),
            Some((50, 20, 60, 30))
        );
        assert_eq!(
            intersect_scissor_rect((-10, -10, 20, 20), (-5, -5, 20, 20), [100, 100]),
            Some((0, 0, 10, 10))
        );
        assert_eq!(
            intersect_scissor_rect((0, 0, 10, 10), (20, 20, 5, 5), [100, 100]),
            None
        );
    }

    #[test]
    fn pan_maps_pixels_to_time_and_follows_the_pointer() {
        let mut view = ViewX::new(0, 1000);
        // Drag right by half the width → window shifts left by half the span.
        apply_pan(&mut view, 50.0, 100.0);
        assert_eq!((view.min_us, view.max_us), (-500, 500));
    }

    #[test]
    fn zoom_in_shrinks_the_span_about_the_cursor() {
        let mut view = ViewX::new(0, 1000);
        apply_zoom(&mut view, 0.5, 200.0); // scroll up at centre
        assert!(view.span_us() < 1000);
        // Centre stays roughly fixed.
        let centre = (view.min_us + view.max_us) / 2;
        assert!((centre - 500).abs() < 50);
    }
}

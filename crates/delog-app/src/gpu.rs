//! egui/eframe adapter for DeLOG's pure-wgpu renderer (PLAN.md GPU-06).
//!
//! `delog-render` deliberately contains no egui types. This module is the thin
//! shell boundary: it adopts eframe's `wgpu` device/queue, stores renderer
//! resources in egui_wgpu's callback resource map, and emits paint callbacks
//! that draw inside egui's main render pass.

use std::sync::Arc;

use delog_core::identity::FieldId;
use delog_render::{BufferManager, LinePipeline, PlotUniform, RenderContext, UniformRing};
use eframe::{egui_wgpu, wgpu};

const DEMO_FIELD: FieldId = FieldId(u32::MAX);

/// App-owned handle to GPU resources stored in egui_wgpu callback resources.
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

    /// Paint a minimal line trace in `rect`. This is a real callback path (same
    /// pass, viewport, scissor and line pipeline) that PLT-02/03 will replace
    /// with cache-backed traces.
    pub fn paint_demo_line(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !self.available || rect.width() < 2.0 || rect.height() < 2.0 {
            return;
        }

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            PlotPaintCallback {
                field: DEMO_FIELD,
                uniform_slot: 0,
                viewport_points: [rect.width(), rect.height()],
                sample_count: DEMO_XY.len() as u32 / 2,
            },
        ));
    }
}

struct PlotCallbackResources {
    line: LinePipeline,
    buffers: BufferManager,
    uniforms: UniformRing,
    demo_bind_group: wgpu::BindGroup,
}

impl PlotCallbackResources {
    fn new(ctx: RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let line = LinePipeline::new(&ctx, color_format);
        let mut buffers = BufferManager::new(ctx.clone());
        buffers.sync(DEMO_FIELD, &DEMO_XY, false);

        let uniforms = UniformRing::new(ctx.clone(), 1);
        uniforms.write(
            0,
            &PlotUniform::new(1.0, 0.0, 1.0, 0.0, [64.0, 64.0], 2.0, [0.4, 0.85, 1.0, 1.0]),
        );
        let demo_bind_group = line.bind_group(&ctx, buffers.buffer(DEMO_FIELD).unwrap(), &uniforms);

        Self {
            line,
            buffers,
            uniforms,
            demo_bind_group,
        }
    }
}

struct PlotPaintCallback {
    field: FieldId,
    uniform_slot: u32,
    viewport_points: [f32; 2],
    sample_count: u32,
}

impl egui_wgpu::CallbackTrait for PlotPaintCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(resources) = callback_resources.get_mut::<PlotCallbackResources>() {
            let pixels_per_point = screen_descriptor.pixels_per_point;
            resources.uniforms.write(
                self.uniform_slot,
                &PlotUniform::new(
                    1.0,
                    0.0,
                    1.0,
                    0.0,
                    [
                        (self.viewport_points[0] * pixels_per_point).max(1.0),
                        (self.viewport_points[1] * pixels_per_point).max(1.0),
                    ],
                    2.0,
                    [0.4, 0.85, 1.0, 1.0],
                ),
            );
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<PlotCallbackResources>() else {
            return;
        };
        if resources.buffers.buffer(self.field).is_none() {
            return;
        }

        let viewport = info.viewport_in_pixels();
        let clip = info.clip_rect_in_pixels();
        let Some((x, y, width, height)) = intersect_scissor_rect(
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

        render_pass.set_scissor_rect(x, y, width, height);
        resources.line.encode_trace(
            render_pass,
            &resources.demo_bind_group,
            resources.uniforms.dynamic_offset(self.uniform_slot),
            self.sample_count,
        );
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

const DEMO_XY: [f32; 16] = [
    -0.92, -0.45, -0.64, 0.18, -0.38, -0.08, -0.12, 0.62, 0.16, 0.28, 0.42, 0.72, 0.68, -0.22,
    0.92, 0.36,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_trace_has_xy_pairs() {
        assert_eq!(DEMO_XY.len() % 2, 0);
        assert!(DEMO_XY.len() >= 4);
    }

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
}

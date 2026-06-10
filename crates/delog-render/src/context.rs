//! Render context: the shared wgpu device/queue (PLAN.md §9.1, GPU-01).
//!
//! DeLOG uses a *single* `wgpu::Device`/`Queue` — the one eframe already created
//! — for egui, every plot, and the 3D view, so buffers are shared with no
//! cross-device copies and there is one place to track VRAM. This crate is pure
//! wgpu (no egui types, §3.2): `delog-app` hands the device/queue from
//! `egui_wgpu`'s render state into [`RenderContext::new`]. [`RenderContext::headless`]
//! acquires a standalone device with no surface, for golden-image tests (GPU-13),
//! benches and headless export.

use std::sync::Arc;

/// The shared GPU device and queue all renderer subsystems draw with.
#[derive(Clone)]
pub struct RenderContext {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

impl RenderContext {
    /// Adopt an externally-owned device/queue (eframe's, via `egui_wgpu`).
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        Self { device, queue }
    }

    /// Acquire a standalone headless device (no window/surface) on the default
    /// adapter. Returns `None` when no adapter is available (e.g. a GPU-less CI
    /// runner) so callers can skip rather than fail.
    pub fn headless() -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("delog-headless"),
            ..Default::default()
        }))
        .ok()?;
        Some(Self::new(Arc::new(device), Arc::new(queue)))
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn device_arc(&self) -> Arc<wgpu::Device> {
        Arc::clone(&self.device)
    }

    pub fn queue_arc(&self) -> Arc<wgpu::Queue> {
        Arc::clone(&self.queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device acquires when an adapter exists; on a GPU-less host this is a
    /// graceful skip so CI stays green (GPU-13 follows the same policy).
    #[test]
    fn headless_context_is_usable_when_an_adapter_exists() {
        match RenderContext::headless() {
            Some(ctx) => {
                // The device is live and shareable.
                let _limits = ctx.device().limits();
                assert!(Arc::strong_count(&ctx.queue_arc()) >= 2);
            }
            None => {
                eprintln!("no wgpu adapter available — skipping GPU context test");
            }
        }
    }
}

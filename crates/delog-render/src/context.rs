//! Render context: the shared wgpu device/queue.
//!
//! DeLOG uses a single `wgpu::Device`/`Queue` (eframe's) for egui, every plot
//! and the 3D view, so buffers are shared with no cross-device copies.

use std::sync::Arc;

#[derive(Clone)]
pub struct RenderContext {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

impl RenderContext {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        Self { device, queue }
    }

    /// Returns `None` when no adapter is available (e.g. a GPU-less CI runner)
    /// so callers can skip rather than fail.
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

    #[test]
    fn headless_context_is_usable_when_an_adapter_exists() {
        match RenderContext::headless() {
            Some(ctx) => {
                let _limits = ctx.device().limits();
                assert!(Arc::strong_count(&ctx.queue_arc()) >= 2);
            }
            None => {
                eprintln!("no wgpu adapter available — skipping GPU context test");
            }
        }
    }
}

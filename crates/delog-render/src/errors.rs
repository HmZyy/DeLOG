//! wgpu error scopes → diagnostics.
//!
//! An uncaptured wgpu error aborts the process by default; [`GpuErrorHub`]
//! brackets GPU work in scopes and resolves the async results into messages.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

type ErrorFut = Pin<Box<dyn Future<Output = Option<wgpu::Error>> + Send + 'static>>;

const FILTERS: [wgpu::ErrorFilter; 2] = [
    wgpu::ErrorFilter::OutOfMemory,
    wgpu::ErrorFilter::Validation,
];

/// `!Send`: wgpu's scope stack is thread-local. Open before the work, hand to
/// [`GpuErrorHub::close`] right after.
#[must_use = "close the bracket via GpuErrorHub::close or its errors are dropped"]
pub struct ErrorScopeBracket {
    guards: Vec<wgpu::ErrorScopeGuard>,
}

/// Collects wgpu error-scope results across frames. A scope future resolves
/// only after the device processes its commands, so a result may surface a
/// frame or two later.
#[derive(Default)]
pub struct GpuErrorHub {
    pending: Vec<ErrorFut>,
}

impl GpuErrorHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(device: &wgpu::Device) -> ErrorScopeBracket {
        ErrorScopeBracket {
            guards: FILTERS
                .iter()
                .map(|&filter| device.push_error_scope(filter))
                .collect(),
        }
    }

    pub fn close(&mut self, bracket: ErrorScopeBracket) {
        for guard in bracket.guards.into_iter().rev() {
            self.pending.push(Box::pin(guard.pop()));
        }
    }

    /// Non-blocking: polls the device, resolving any finished scopes; the rest
    /// stay queued for the next call.
    pub fn drain(&mut self, device: &wgpu::Device) -> Vec<String> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let _ = device.poll(wgpu::PollType::Poll);
        let mut cx = Context::from_waker(Waker::noop());
        let mut messages = Vec::new();
        self.pending
            .retain_mut(|fut| match fut.as_mut().poll(&mut cx) {
                Poll::Ready(Some(error)) => {
                    messages.push(error.to_string());
                    false
                }
                Poll::Ready(None) => false,
                Poll::Pending => true,
            });
        messages
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::RenderContext;

    fn drain_until_settled(hub: &mut GpuErrorHub, device: &wgpu::Device) -> Vec<String> {
        let mut messages = Vec::new();
        for _ in 0..1000 {
            messages.extend(hub.drain(device));
            if hub.pending_len() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        messages
    }

    #[test]
    fn validation_error_inside_scopes_drains_as_a_message() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping error hub test");
            return;
        };
        let mut hub = GpuErrorHub::new();
        let bracket = GpuErrorHub::open(ctx.device());
        // Deliberate validation error: a buffer far beyond max_buffer_size.
        let _oversized = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-oversized-test-buffer"),
            size: 1 << 60,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        hub.close(bracket);

        let messages = drain_until_settled(&mut hub, ctx.device());
        assert!(
            !messages.is_empty(),
            "expected the oversized buffer to report a validation error"
        );
        assert_eq!(hub.pending_len(), 0);
    }

    #[test]
    fn clean_scopes_drain_to_nothing() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping error hub test");
            return;
        };
        let mut hub = GpuErrorHub::new();
        let bracket = GpuErrorHub::open(ctx.device());
        let _ok = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-small-test-buffer"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        hub.close(bracket);

        let messages = drain_until_settled(&mut hub, ctx.device());
        assert!(messages.is_empty(), "unexpected errors: {messages:?}");
        assert_eq!(hub.pending_len(), 0);
    }
}

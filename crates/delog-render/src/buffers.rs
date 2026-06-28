//! GPU buffer manager: per-`FieldId` STORAGE buffers mirroring each trace's
//! `xy` cache, uploading only the new tail span and growing GPU-side.

use std::collections::HashMap;

use delog_core::identity::FieldId;
use delog_core::mem::MemBreakdown;

use crate::context::RenderContext;

const F32: u64 = std::mem::size_of::<f32>() as u64;
const MIN_CAPACITY_FLOATS: u64 = 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct UploadStat {
    pub bytes: u64,
    /// True for a full re-upload (first alloc, grow, or rebuild) vs a tail append.
    pub full_upload: bool,
}

struct TraceGpu {
    buf: wgpu::Buffer,
    capacity_floats: u64,
    len_floats: u64,
}

pub struct BufferManager {
    ctx: RenderContext,
    traces: HashMap<FieldId, TraceGpu>,
    full_uploads: u64,
}

impl BufferManager {
    pub fn new(ctx: RenderContext) -> Self {
        Self {
            ctx,
            traces: HashMap::new(),
            full_uploads: 0,
        }
    }

    /// Mirror `xy` into `field`'s GPU buffer. Pass `rebuilt = true` when the
    /// contents changed wholesale (rebuild/rebase); otherwise only the appended
    /// tail is uploaded.
    pub fn sync(&mut self, field: FieldId, xy: &[f32], rebuilt: bool) -> UploadStat {
        let needed = xy.len() as u64;
        if needed == 0 {
            return UploadStat::default();
        }

        let prev_len = self.traces.get(&field).map_or(0, |t| t.len_floats);
        let is_new = !self.traces.contains_key(&field);
        // A shorter buffer can only mean the cache was rebuilt smaller.
        let full = rebuilt || is_new || needed < prev_len;
        let preserve = if full { 0 } else { prev_len };

        let grew = self.ensure_capacity(field, needed, preserve);

        let start = if full { 0 } else { prev_len };
        let uploaded_floats = if needed > start {
            let span = bytemuck::cast_slice(&xy[start as usize..needed as usize]);
            self.ctx
                .queue()
                .write_buffer(&self.traces[&field].buf, start * F32, span);
            needed - start
        } else {
            0
        };

        let full_upload = grew || (full && !is_new);
        if full_upload {
            self.full_uploads += 1;
        }
        self.traces.get_mut(&field).unwrap().len_floats = needed;
        UploadStat {
            bytes: uploaded_floats * F32,
            full_upload,
        }
    }

    /// Ensure `field`'s buffer holds at least `needed_floats`, preserving the
    /// first `preserve_floats` GPU-side on growth. Returns whether an existing
    /// buffer was grown.
    fn ensure_capacity(
        &mut self,
        field: FieldId,
        needed_floats: u64,
        preserve_floats: u64,
    ) -> bool {
        if let Some(t) = self.traces.get(&field)
            && t.capacity_floats >= needed_floats
        {
            return false;
        }

        let new_cap = ((needed_floats as f64 * 1.5).ceil() as u64).max(MIN_CAPACITY_FLOATS);
        let new_buf = self.ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-trace"),
            size: new_cap * F32,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let grew = match self.traces.get(&field) {
            Some(old) => {
                if preserve_floats > 0 {
                    let mut enc =
                        self.ctx
                            .device()
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("delog-trace-grow"),
                            });
                    enc.copy_buffer_to_buffer(&old.buf, 0, &new_buf, 0, preserve_floats * F32);
                    self.ctx.queue().submit([enc.finish()]);
                }
                true
            }
            None => false,
        };

        let len_floats = self.traces.get(&field).map_or(0, |t| t.len_floats);
        self.traces.insert(
            field,
            TraceGpu {
                buf: new_buf,
                capacity_floats: new_cap,
                len_floats,
            },
        );
        grew
    }

    pub fn buffer(&self, field: FieldId) -> Option<&wgpu::Buffer> {
        self.traces.get(&field).map(|t| &t.buf)
    }

    pub fn samples(&self, field: FieldId) -> u64 {
        self.traces.get(&field).map_or(0, |t| t.len_floats / 2)
    }

    pub fn remove(&mut self, field: FieldId) {
        self.traces.remove(&field);
    }

    pub fn fields(&self) -> impl Iterator<Item = FieldId> + '_ {
        self.traces.keys().copied()
    }

    pub fn buffer_count(&self) -> usize {
        self.traces.len()
    }

    pub fn full_uploads(&self) -> u64 {
        self.full_uploads
    }

    pub fn total_gpu_bytes(&self) -> u64 {
        self.traces.values().map(|t| t.capacity_floats * F32).sum()
    }

    pub fn field_mem(&self, field: FieldId) -> MemBreakdown {
        let gpu = self
            .traces
            .get(&field)
            .map_or(0, |t| t.capacity_floats * F32);
        MemBreakdown {
            gpu,
            ..MemBreakdown::ZERO
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readback(ctx: &RenderContext, buf: &wgpu::Buffer, count: u64) -> Vec<f32> {
        let bytes = count * F32;
        let staging = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, bytes);
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
        bytemuck::cast_slice::<u8, f32>(&data).to_vec()
    }

    #[test]
    fn append_then_grow_preserves_contents_and_counts_uploads() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping buffer manager test");
            return;
        };
        let mut mgr = BufferManager::new(ctx.clone());
        let field = FieldId(0);

        let a: Vec<f32> = (0..8).map(|i| i as f32).collect();
        mgr.sync(field, &a, false);
        assert_eq!(mgr.samples(field), 4);
        assert_eq!(mgr.full_uploads(), 0);
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 8), a);

        let b: Vec<f32> = (0..600).map(|i| i as f32).collect();
        mgr.sync(field, &b, false);
        assert_eq!(mgr.samples(field), 300);
        assert_eq!(mgr.full_uploads(), 0);
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 600), b);

        let c: Vec<f32> = (0..2000).map(|i| i as f32).collect();
        mgr.sync(field, &c, false);
        assert_eq!(mgr.samples(field), 1000);
        assert_eq!(mgr.full_uploads(), 1, "growth is one full-upload event");
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 2000), c);
        assert!(mgr.total_gpu_bytes() >= 2000 * F32);
        assert!(mgr.field_mem(field).gpu >= 2000 * F32);
    }

    #[test]
    fn rebuilt_smaller_window_reuploads_from_zero() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping buffer manager test");
            return;
        };
        let mut mgr = BufferManager::new(ctx.clone());
        let field = FieldId(7);

        // Large window first.
        let big: Vec<f32> = (0..2000).map(|i| i as f32).collect();
        mgr.sync(field, &big, true);
        assert_eq!(mgr.samples(field), 1000);

        // Smaller window, wholesale replace (windowed sync always passes rebuilt=true).
        let small: Vec<f32> = (0..20).map(|i| (i as f32) + 0.5).collect();
        let stat = mgr.sync(field, &small, true);
        assert_eq!(mgr.samples(field), 10);
        assert!(stat.full_upload, "rebuilt shrink is a full upload");
        // The first 20 floats now hold the new window; capacity is grow-only so
        // the buffer is still large, but only `small.len()` floats are live.
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 20), small);
    }

    #[test]
    fn rebuilt_forces_a_full_reupload() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut mgr = BufferManager::new(ctx.clone());
        let field = FieldId(1);

        let a: Vec<f32> = vec![1.0; 100];
        mgr.sync(field, &a, false);
        assert_eq!(mgr.full_uploads(), 0);

        // Same length, different contents, rebuilt = true → full re-upload.
        let b: Vec<f32> = vec![2.0; 100];
        mgr.sync(field, &b, true);
        assert_eq!(mgr.full_uploads(), 1);
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 100), b);
    }
}

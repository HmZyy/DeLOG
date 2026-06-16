//! GPU buffer manager (PLAN.md §9.3, GPU-02/03).
//!
//! Per-`FieldId` ledger of STORAGE buffers mirroring each trace's `xy` cache.
//! Appending uploads **only the new tail span** via `write_buffer` (ZC-4). When
//! a buffer must grow it allocates ×1.5 and copies the old contents **GPU-side**
//! (`copy_buffer_to_buffer`, no CPU round-trip), then uploads the new span. A
//! wholesale content change (rebuild / time rebase, §8.3) forces a full
//! re-upload. Growth and rebuilds bump `gpu_full_uploads` so a regression that
//! re-uploads too often is visible (ZC-4). Capacity bytes feed the `gpu` pool of
//! `MemBreakdown` (§4.6).

use std::collections::HashMap;

use delog_core::identity::FieldId;
use delog_core::mem::MemBreakdown;

use crate::context::RenderContext;

const F32: u64 = std::mem::size_of::<f32>() as u64;
/// Smallest buffer allocated, in floats — avoids churn on tiny traces.
const MIN_CAPACITY_FLOATS: u64 = 1024;

/// What a single [`BufferManager::sync`] call actually uploaded — the caller
/// feeds it into the `upload_bytes`/`gpu_full_uploads` metrics (§16, PRF-01).
#[derive(Debug, Clone, Copy, Default)]
pub struct UploadStat {
    /// Bytes written to the GPU this call (0 for a no-op resync).
    pub bytes: u64,
    /// Whether this was a full re-upload (first alloc, grow, or rebuild) rather
    /// than a clean tail append.
    pub full_upload: bool,
}

/// One trace's GPU storage buffer and its fill state.
struct TraceGpu {
    buf: wgpu::Buffer,
    capacity_floats: u64,
    len_floats: u64,
}

/// Owns the per-trace GPU buffers.
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
    /// tail beyond what is already resident is uploaded (ZC-4).
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

        // Growth and rebuilds are the "not a clean append" events to watch.
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
    /// buffer was grown (vs. first allocation or no-op).
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

    /// The storage buffer for `field`, if resident (for binding in a draw).
    pub fn buffer(&self, field: FieldId) -> Option<&wgpu::Buffer> {
        self.traces.get(&field).map(|t| &t.buf)
    }

    /// Samples resident on the GPU for `field` (`len_floats / 2`).
    pub fn samples(&self, field: FieldId) -> u64 {
        self.traces.get(&field).map_or(0, |t| t.len_floats / 2)
    }

    /// Drop `field`'s buffer (cache GC / unplot).
    pub fn remove(&mut self, field: FieldId) {
        self.traces.remove(&field);
    }

    /// The fields with a resident buffer.
    pub fn fields(&self) -> impl Iterator<Item = FieldId> + '_ {
        self.traces.keys().copied()
    }

    pub fn buffer_count(&self) -> usize {
        self.traces.len()
    }

    /// Count of full re-uploads (growth or rebuild) — a regression signal (ZC-4).
    pub fn full_uploads(&self) -> u64 {
        self.full_uploads
    }

    /// Total GPU capacity bytes across all traces.
    pub fn total_gpu_bytes(&self) -> u64 {
        self.traces.values().map(|t| t.capacity_floats * F32).sum()
    }

    /// `MemBreakdown` (gpu pool) for one field (§4.6).
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

    /// Read `count` floats back from a GPU buffer (blocking).
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

        // First upload: 4 samples (8 floats). New buffer → MIN_CAPACITY (1024).
        let a: Vec<f32> = (0..8).map(|i| i as f32).collect();
        mgr.sync(field, &a, false);
        assert_eq!(mgr.samples(field), 4);
        assert_eq!(mgr.full_uploads(), 0); // initial alloc is not a "full re-upload"
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 8), a);

        // Append within capacity: only the tail uploads, no growth, no counter.
        let b: Vec<f32> = (0..600).map(|i| i as f32).collect();
        mgr.sync(field, &b, false);
        assert_eq!(mgr.samples(field), 300);
        assert_eq!(mgr.full_uploads(), 0);
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 600), b);

        // Append beyond capacity (1024 floats) → growth via GPU-side copy.
        let c: Vec<f32> = (0..2000).map(|i| i as f32).collect();
        mgr.sync(field, &c, false);
        assert_eq!(mgr.samples(field), 1000);
        assert_eq!(mgr.full_uploads(), 1, "growth is one full-upload event");
        // The GPU-side copy preserved the old prefix and the new span uploaded.
        assert_eq!(readback(&ctx, mgr.buffer(field).unwrap(), 2000), c);
        assert!(mgr.total_gpu_bytes() >= 2000 * F32);
        assert!(mgr.field_mem(field).gpu >= 2000 * F32);
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

//! Frame-encode bench: 32 traces × 1M samples, decimated.
//!
//! Measures the per-frame CPU cost of the zoomed-out plot path: for each trace,
//! compute per-pixel min/max columns (pyramid), upload them, and encode the
//! draw. Budget: < 3 ms. Skips when no GPU adapter is present (the work is
//! GPU-buffer bound), so a headless CI runner just compiles it.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int64Array};
use arrow::datatypes::DataType;
use criterion::{Criterion, criterion_group, criterion_main};
use delog_cache::TraceCache;
use delog_core::chunk::Chunk;
use delog_core::identity::{FieldId, IdentityRegistry};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;
use delog_render::{
    BufferManager, MinMaxColPipeline, OffscreenTarget, PlotUniform, RenderContext, UniformRing,
};

const TRACES: usize = 32;
const SAMPLES: i64 = 1_000_000;
const CHUNK: i64 = 65_536;
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

/// A 1M-sample single-field cache (sine ramp), rebased at origin 0.
fn cache_of(seed: i64) -> TraceCache {
    let schema = Arc::new(
        TopicSchema::new(
            "S",
            [FieldSchema::new("V", DataType::Float32, Some("u"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < SAMPLES {
        let end = (start + CHUNK).min(SAMPLES);
        let times = Int64Array::from((start..end).collect::<Vec<_>>());
        let vals: ArrayRef = Arc::new(Float32Array::from(
            (start..end)
                .map(|i| ((i + seed) as f32 * 0.001).sin())
                .collect::<Vec<_>>(),
        ));
        chunks.push(Arc::new(
            Chunk::try_new(times, vec![vals], &schema).unwrap(),
        ));
        start = end;
    }
    let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), chunks).unwrap());

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "S").unwrap();
    let field = identity.add_field(topic, "V").unwrap();
    let snap = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
    TraceCache::build(&snap, field, 0, 0, &MetricsRegistry::new()).unwrap()
}

/// Isolate the CPU cost of decimating all 32 traces (no GPU), to see whether
/// the per-frame budget is column-compute bound or GPU-call bound.
fn bench_decimate_cpu(c: &mut Criterion) {
    let caches: Vec<TraceCache> = (0..TRACES as i64).map(cache_of).collect();
    let (x0, x1) = (0.0_f32, (SAMPLES as f32) * 1e-6);
    c.bench_function("decimate_cpu_32x1M", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for cache in &caches {
                let cols = cache.minmax_columns(x0, x1, WIDTH as usize, true);
                total += cols.len();
            }
            total
        });
    });
}

fn bench_frame_encode(c: &mut Criterion) {
    let Some(ctx) = RenderContext::headless() else {
        eprintln!("no wgpu adapter — skipping frame-encode bench");
        c.bench_function("frame_encode_32x1M_decimated", |b| b.iter(|| {}));
        return;
    };

    let caches: Vec<TraceCache> = (0..TRACES as i64).map(cache_of).collect();
    let target = OffscreenTarget::new(ctx.clone(), WIDTH, HEIGHT);
    let pipeline = MinMaxColPipeline::new(&ctx, target.format());
    let uniforms = UniformRing::new(ctx.clone(), TRACES as u32);
    let mut col_buffers = BufferManager::new(ctx.clone());

    // Full visible window (seconds): 1M samples at 1 µs spacing → 0..1 s.
    let (x0, x1) = (0.0_f32, (SAMPLES as f32) * 1e-6);

    c.bench_function("frame_encode_32x1M_decimated", |b| {
        b.iter(|| {
            let mut binds = Vec::with_capacity(TRACES);
            for (i, cache) in caches.iter().enumerate() {
                let field = FieldId(i as u32);
                let cols = cache.minmax_columns(x0, x1, WIDTH as usize, true);
                col_buffers.sync(field, &cols, true);
                uniforms.write(
                    i as u32,
                    &PlotUniform::from_view(
                        (x0, x1),
                        (-1.0, 1.0),
                        [WIDTH as f32, HEIGHT as f32],
                        1.0,
                        [0.4, 0.85, 1.0, 1.0],
                    ),
                );
                binds.push((
                    pipeline.bind_group(&ctx, col_buffers.buffer(field).unwrap(), &uniforms),
                    i as u32,
                ));
            }

            let mut enc = ctx
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("frame-encode"),
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
                for (bind, slot) in &binds {
                    pipeline.encode(&mut pass, bind, uniforms.dynamic_offset(*slot), WIDTH);
                }
            }
            enc.finish()
        });
    });
}

criterion_group!(benches, bench_decimate_cpu, bench_frame_encode);
criterion_main!(benches);

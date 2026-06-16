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
use std::sync::{Arc, Mutex};

use delog_cache::{CacheManager, MinMax};
use delog_core::identity::FieldId;
use delog_core::metrics::MetricsRegistry;
use delog_render::{
    BufferManager, GpuErrorHub, Grid3dPipeline, GridUniform, LinePipeline, MeshGpu, MeshPipeline,
    MeshUniform, MinMaxColPipeline, PlotUniform, RenderContext, ScatterPipeline, Scene3dTarget,
    StepPipeline, Traj3dPipeline, Traj3dUniform, UniformRing,
};
use eframe::{egui_wgpu, wgpu};

use crate::camera::OrbitCamera;
use crate::models;
use crate::plot::{PlotPane, TraceMode, ViewX};
use crate::settings::Scene3dSettings;
use crate::vehicle::ModelKind;

/// Render-ready data for one vehicle this frame (TDV-09/10): its model, world
/// transform (pose), colors, and resampled render-space trajectory. Built in
/// `scene_ui` from a `VehicleConfig` + the snapshot + playhead.
pub struct VehicleDraw<'a> {
    /// Stable per-frame key for one configured vehicle row.
    pub key: u32,
    pub model: &'a ModelKind,
    /// Body→render model matrix (column-major) and its normal matrix.
    pub model_matrix: [[f32; 4]; 4],
    pub normal_matrix: [[f32; 4]; 4],
    pub color: [f32; 4],
    pub path_color: [f32; 4],
    /// Render-space `[x,y,z]` trajectory points (NaN = gap).
    pub trajectory: &'a [[f32; 3]],
    /// Config generation the trajectory was built at. Unchanged across pure
    /// data-append rebuilds (so the path only grows), bumped on config/offset
    /// change — lets the GPU upload just the new tail vs. a full re-upload.
    pub traj_generation: u64,
}

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
    /// Whether the egui render target is an sRGB format (it gamma-encodes the
    /// shader's linear output) vs a plain UNORM target (raw write). Trace colours
    /// are stored in sRGB; we convert to match the target so the rendered line
    /// matches the legend swatch.
    srgb_target: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuSummary {
    pub buffer_count: usize,
    pub gpu_bytes: u64,
}

impl GpuBridge {
    pub fn from_creation_context(cc: &eframe::CreationContext<'_>) -> Self {
        let Some(render_state) = &cc.wgpu_render_state else {
            return Self {
                available: false,
                srgb_target: false,
            };
        };

        let ctx = RenderContext::new(
            Arc::new(render_state.device.clone()),
            Arc::new(render_state.queue.clone()),
        );
        let srgb_target = render_state.target_format.is_srgb();
        let scene = SceneResources::new(ctx.clone());
        let resources = PlotCallbackResources::new(ctx, render_state.target_format);
        {
            let mut renderer = render_state.renderer.write();
            renderer.callback_resources.insert(resources);
            renderer.callback_resources.insert(scene);
        }

        Self {
            available: true,
            srgb_target,
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn begin_plot_frame(&self, frame: &eframe::Frame) {
        if !self.available {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let mut renderer = render_state.renderer.write();
        if let Some(res) = renderer
            .callback_resources
            .get_mut::<PlotCallbackResources>()
        {
            res.next_uniform_slot = 0;
        }
    }

    pub fn retain_plotted_buffers(&self, frame: &eframe::Frame, plotted: &[FieldId]) {
        if !self.available {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let mut renderer = render_state.renderer.write();
        if let Some(res) = renderer
            .callback_resources
            .get_mut::<PlotCallbackResources>()
        {
            res.retain_buffers(plotted);
        }
    }

    /// Resolve finished wgpu error scopes into messages for the diagnostics
    /// hub (GPU-12). Call once per frame.
    pub fn drain_gpu_errors(&self, frame: &eframe::Frame) -> Vec<String> {
        if !self.available {
            return Vec::new();
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return Vec::new();
        };
        let renderer = render_state.renderer.read();
        let Some(res) = renderer.callback_resources.get::<PlotCallbackResources>() else {
            return Vec::new();
        };
        res.errors.lock().unwrap().drain(res.ctx.device())
    }

    pub fn field_gpu_bytes(&self, frame: &eframe::Frame, field: FieldId) -> u64 {
        if !self.available {
            return 0;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return 0;
        };
        let renderer = render_state.renderer.read();
        let Some(res) = renderer.callback_resources.get::<PlotCallbackResources>() else {
            return 0;
        };
        res.buffers
            .field_mem(field)
            .gpu
            .saturating_add(res.col_buffers.field_mem(field).gpu)
    }

    pub fn summary(&self, frame: &eframe::Frame) -> GpuSummary {
        if !self.available {
            return GpuSummary::default();
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return GpuSummary::default();
        };
        let renderer = render_state.renderer.read();
        let Some(res) = renderer.callback_resources.get::<PlotCallbackResources>() else {
            return GpuSummary::default();
        };
        GpuSummary {
            buffer_count: res.buffers.buffer_count() + res.col_buffers.buffer_count(),
            gpu_bytes: res
                .buffers
                .total_gpu_bytes()
                .saturating_add(res.col_buffers.total_gpu_bytes()),
        }
    }

    /// Upload the pane's ready trace caches into the `plot_rect`, write their
    /// uniforms for the given visible data window, and emit the paint callback.
    /// The caller supplies the X/Y ranges so the egui axes share them exactly.
    #[allow(clippy::too_many_arguments)]
    pub fn render_pane(
        &self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        caches: &mut CacheManager,
        pane: &PlotPane,
        view: PaneView,
        tuning: crate::settings::RenderTuning,
        metrics: &Arc<MetricsRegistry>,
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
        // This pane's GPU uploads, accumulated inside the renderer block and
        // recorded after it (§16 `upload_bytes`/`gpu_full_uploads`, PRF-01).
        let mut upload_bytes = 0u64;
        let mut full_uploads = 0u64;
        {
            let mut renderer = render_state.renderer.write();
            let Some(res) = renderer
                .callback_resources
                .get_mut::<PlotCallbackResources>()
            else {
                return;
            };
            // Capture buffer growth/upload + uniform-write errors (GPU-12).
            let scope = GpuErrorHub::open(res.ctx.device());
            // Share the registry so the deferred paint callback can time
            // `gpu_encode` (§16, PRF-01).
            if res.metrics.is_none() {
                res.metrics = Some(Arc::clone(metrics));
            }
            let base_slot = res.next_uniform_slot;
            res.next_uniform_slot += pane.traces.len() as u32;
            res.ensure_uniform_capacity(res.next_uniform_slot);
            let plot_w = viewport_px[0];

            for (slot, trace) in pane.visible_traces().enumerate() {
                let slot = base_slot + slot as u32;
                let Some(cache) = caches.get(trace.field) else {
                    continue;
                };
                res.uniforms.write(
                    slot,
                    &PlotUniform::from_view(
                        (x0, x1),
                        (y0, y1),
                        viewport_px,
                        trace.width_px,
                        shader_color(trace.color, self.srgb_target),
                    )
                    .with_aa(tuning.line_aa_px),
                );

                let kind = match trace.mode {
                    TraceMode::Line => {
                        // Draw-path selector (GPU-10): decimate when the visible
                        // window packs more than `decimate_threshold` samples/px.
                        let (a, b) = cache.index_range(x0, x1);
                        let visible = b.saturating_sub(a) as f32;
                        if plot_w >= 1.0 && visible / plot_w > tuning.decimate_threshold {
                            let width = plot_w as usize;
                            let cols = cache.minmax_columns(x0, x1, width, tuning.bridge_columns);
                            let stat = res.col_buffers.sync(trace.field, &cols, true);
                            upload_bytes += stat.bytes;
                            full_uploads += stat.full_upload as u64;
                            DrawKind::Columns {
                                count: width as u32,
                            }
                        } else {
                            let stat = res.buffers.sync(trace.field, &cache.xy, false);
                            upload_bytes += stat.bytes;
                            full_uploads += stat.full_upload as u64;
                            DrawKind::Line {
                                samples: res.buffers.samples(trace.field) as u32,
                            }
                        }
                    }
                    TraceMode::Scatter => {
                        res.buffers.sync(trace.field, &cache.xy, false);
                        DrawKind::Scatter {
                            samples: res.buffers.samples(trace.field) as u32,
                        }
                    }
                    TraceMode::Step => {
                        res.buffers.sync(trace.field, &cache.xy, false);
                        DrawKind::Step {
                            samples: res.buffers.samples(trace.field) as u32,
                        }
                    }
                };

                if kind.is_drawable() {
                    items.push(DrawItem {
                        field: trace.field,
                        slot,
                        kind,
                    });
                }
            }
            res.errors.get_mut().unwrap().close(scope);
        }

        // §16 GPU-upload gauges/counters for this pane (PRF-01).
        if upload_bytes > 0 {
            metrics.record("upload_bytes", upload_bytes as f32);
        }
        if full_uploads > 0 {
            metrics.add("gpu_full_uploads", full_uploads);
        }

        if items.is_empty() {
            return;
        }
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            plot_rect,
            ScenePaintCallback { items },
        ));
    }

    /// Render the 3D scene (grid + axes for now) for `camera` into the
    /// offscreen [`Scene3dTarget`], resolve it, and return an egui texture id
    /// the caller composites with `ui.image`/`painter().image` (PLAN.md §9.1,
    /// TDV-01). The offscreen pass is submitted on our own queue during
    /// `update()`, so the texture is ready before eframe paints this frame.
    pub fn render_scene(
        &self,
        frame: &eframe::Frame,
        ui: &egui::Ui,
        rect: egui::Rect,
        camera: &OrbitCamera,
        scene3d: Scene3dSettings,
        vehicles: &[VehicleDraw],
    ) -> Option<egui::TextureId> {
        if !self.available {
            return None;
        }
        let render_state = frame.wgpu_render_state()?;
        let ppp = ui.ctx().pixels_per_point();
        let px_w = (rect.width() * ppp).round().max(1.0) as u32;
        let px_h = (rect.height() * ppp).round().max(1.0) as u32;
        let device = render_state.device.clone();
        let mut renderer = render_state.renderer.write();

        // Render into the offscreen target and take a handle to its resolved
        // color view (cloning the view ends the resource borrow so the
        // texture-registration calls below can borrow the renderer mutably).
        let (view, resized, existing) = {
            let res = renderer.callback_resources.get_mut::<SceneResources>()?;
            let resized = res.target.width() != px_w || res.target.height() != px_h;
            res.target.resize(px_w, px_h);

            // Build the view-projection and its inverse in f64 (downcast to f32
            // for the GPU). Inverting in f32 is ill-conditioned once the camera
            // tracks a vehicle far from the render origin and makes the grid crawl
            // while zooming/following — see `OrbitCamera::view_proj_and_inverse`.
            let (vp, inv) = camera
                .view_proj_and_inverse(px_w as f32 / px_h as f32, scene3d.resolved_far_clip_m());
            let vp_cols = vp.to_cols_array_2d();
            let (fade_start, fade_end) = scene3d.resolved_fog_m();
            // Auto cell tracks height above the y=0 ground plane (where the grid
            // is), so tightly orbiting an airborne vehicle does not collapse the
            // grid to a shimmering fine mesh; the LOD flag lets the shader
            // cross-fade levels so it never pops between sizes.
            let (cell, lod) = scene3d.resolved_grid(camera.eye().y);
            res.grid.set_uniform(
                &res.ctx,
                &GridUniform::new(
                    vp_cols,
                    inv.to_cols_array_2d(),
                    camera.eye().to_array(),
                    cell,
                    fade_start,
                    fade_end,
                    scene3d.fog_enabled,
                    lod,
                ),
            );
            // Refresh the gizmo's view_proj for this frame (color is fixed).
            res.ctx.queue().write_buffer(
                &res.axis_gizmo.uniform,
                0,
                bytemuck::bytes_of(&Traj3dUniform::new(vp_cols, res.axis_gizmo.color)),
            );
            res.prepare_vehicles(vp_cols, camera.eye().to_array(), vehicles);

            let clear = wgpu::Color {
                r: 0.07,
                g: 0.078,
                b: 0.10,
                a: 1.0,
            };
            let mut enc =
                res.ctx
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("delog-scene-encoder"),
                    });
            {
                let mut pass = res.target.begin_pass(&mut enc, clear);
                if scene3d.show_grid {
                    res.grid.draw(&mut pass);
                }
                if scene3d.show_axes {
                    res.traj
                        .draw(&mut pass, &res.axis_gizmo.bind, res.axis_gizmo.count);
                }
                res.draw_vehicles(&mut pass, vehicles);
            }
            res.ctx.queue().submit([enc.finish()]);
            (res.target.resolve_view().clone(), resized, res.texture_id)
        };

        let id = match existing {
            Some(id) => {
                if resized {
                    renderer.update_egui_texture_from_wgpu_texture(
                        &device,
                        &view,
                        wgpu::FilterMode::Linear,
                        id,
                    );
                }
                id
            }
            None => renderer.register_native_texture(&device, &view, wgpu::FilterMode::Linear),
        };
        if existing != Some(id) {
            renderer
                .callback_resources
                .get_mut::<SceneResources>()?
                .texture_id = Some(id);
        }
        Some(id)
    }
}

/// Union of every visible trace's auto-Y range, padded; a sane default when no
/// finite samples are in view. The Y axis always auto-fits the visible window
/// (PLT-06 AutoVisible).
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
    padded(mm.min, mm.max)
}

/// 5% pad, degenerate ranges widened to ±1.
fn padded(min: f32, max: f32) -> (f32, f32) {
    if (max - min).abs() <= f32::EPSILON {
        return (min - 1.0, max + 1.0);
    }
    let pad = (max - min) * 0.05;
    (min - pad, max + pad)
}

/// Convert a stored sRGB colour to what the shader must output so the rendered
/// pixel equals the sRGB colour: on an sRGB target the GPU encodes the shader's
/// linear output, so pass linear; on a UNORM target the write is raw, so pass
/// the sRGB values as-is (matching egui's own UI output). Keeps the trace and
/// its legend swatch identical.
fn shader_color(srgb: [f32; 4], srgb_target: bool) -> [f32; 4] {
    if srgb_target {
        [
            srgb_to_linear(srgb[0]),
            srgb_to_linear(srgb[1]),
            srgb_to_linear(srgb[2]),
            srgb[3],
        ]
    } else {
        srgb
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// How one trace is drawn this frame (GPU-10).
#[derive(Clone, Copy)]
enum DrawKind {
    /// Full polyline: `samples` `[x,y]` pairs.
    Line {
        samples: u32,
    },
    Scatter {
        samples: u32,
    },
    Step {
        samples: u32,
    },
    /// Decimated: `count` per-pixel min/max columns.
    Columns {
        count: u32,
    },
}

/// Which render pipeline a [`DrawKind`] uses (GPU-11 batching key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipelineKind {
    Line,
    Scatter,
    Step,
    Columns,
}

/// Consecutive same-pipeline runs in draw order. Each run costs exactly one
/// `set_pipeline`; items inside it only rebind their trace bind group with a
/// per-trace dynamic uniform offset (GPU-11). Order-preserving so trace
/// overlap (z-order) is unchanged — a homogeneous pane is a single run.
fn pipeline_runs(kinds: impl Iterator<Item = PipelineKind>) -> Vec<(PipelineKind, u32)> {
    let mut runs: Vec<(PipelineKind, u32)> = Vec::new();
    for kind in kinds {
        match runs.last_mut() {
            Some((last, count)) if *last == kind => *count += 1,
            _ => runs.push((kind, 1)),
        }
    }
    runs
}

impl DrawKind {
    fn pipeline(self) -> PipelineKind {
        match self {
            DrawKind::Line { .. } => PipelineKind::Line,
            DrawKind::Scatter { .. } => PipelineKind::Scatter,
            DrawKind::Step { .. } => PipelineKind::Step,
            DrawKind::Columns { .. } => PipelineKind::Columns,
        }
    }

    fn is_drawable(self) -> bool {
        match self {
            DrawKind::Line { samples } => samples >= 2,
            DrawKind::Scatter { samples } => samples >= 1,
            DrawKind::Step { samples } => samples >= 2,
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
    scatter: ScatterPipeline,
    step: StepPipeline,
    minmax: MinMaxColPipeline,
    /// Interleaved `[x,y]` trace buffers (full path).
    buffers: BufferManager,
    /// Transient `[x,min,max]` column buffers (decimated path).
    col_buffers: BufferManager,
    uniforms: UniformRing,
    next_uniform_slot: u32,
    line_binds: HashMap<FieldId, wgpu::BindGroup>,
    scatter_binds: HashMap<FieldId, wgpu::BindGroup>,
    step_binds: HashMap<FieldId, wgpu::BindGroup>,
    col_binds: HashMap<FieldId, wgpu::BindGroup>,
    /// Error-scope results awaiting drain (GPU-12). Mutex only for the Sync
    /// bound of `CallbackResources`; never contended (all access is on the
    /// render thread).
    errors: Mutex<GpuErrorHub>,
    /// Shared metrics registry, populated on the first `render_pane` so the
    /// paint callback can time `gpu_encode` (§16, PRF-01).
    metrics: Option<Arc<MetricsRegistry>>,
}

impl PlotCallbackResources {
    fn new(ctx: RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let line = LinePipeline::new(&ctx, color_format);
        let scatter = ScatterPipeline::new(&ctx, color_format);
        let step = StepPipeline::new(&ctx, color_format);
        let minmax = MinMaxColPipeline::new(&ctx, color_format);
        let buffers = BufferManager::new(ctx.clone());
        let col_buffers = BufferManager::new(ctx.clone());
        let uniforms = UniformRing::new(ctx.clone(), 8);
        Self {
            ctx,
            line,
            scatter,
            step,
            minmax,
            buffers,
            col_buffers,
            uniforms,
            next_uniform_slot: 0,
            line_binds: HashMap::new(),
            scatter_binds: HashMap::new(),
            step_binds: HashMap::new(),
            col_binds: HashMap::new(),
            errors: Mutex::new(GpuErrorHub::new()),
            metrics: None,
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

/// Offscreen 3D-scene resources held in egui_wgpu's callback map (TDV-01).
/// The grid pipeline matches the target's MSAA/format; `texture_id` is the
/// egui handle to the resolved color, (re)pointed when the pane resizes.
/// One static scene polyline (the axis gizmo): its points + a uniform whose
/// `view_proj` is rewritten each frame, with a stable bind group.
struct SceneTraj {
    /// Per-frame uniform (view_proj rewritten each frame).
    uniform: wgpu::Buffer,
    /// Holds an internal reference to the points storage buffer, keeping it
    /// alive — the points are uploaded once and never change.
    bind: wgpu::BindGroup,
    count: u32,
    color: [f32; 4],
}

/// Per-vehicle GPU state (TDV-09/10), keyed by configured vehicle row: a mesh
/// uniform/bind and a growable trajectory line (points + uniform + bind).
struct VehicleGpu {
    mesh_uniform: wgpu::Buffer,
    mesh_bind: wgpu::BindGroup,
    traj_points: wgpu::Buffer,
    traj_capacity: u32,
    /// Points currently resident in `traj_points` (also the draw count).
    traj_count: u32,
    /// Config generation of the resident points; a mismatch forces a full
    /// re-upload, a match lets a longer path upload only its appended tail.
    traj_generation: u64,
    traj_uniform: wgpu::Buffer,
    traj_bind: wgpu::BindGroup,
}

struct SceneResources {
    ctx: RenderContext,
    target: Scene3dTarget,
    grid: Grid3dPipeline,
    traj: Traj3dPipeline,
    mesh: MeshPipeline,
    /// Decoded meshes by model kind (lazy; built on first use).
    model_cache: HashMap<ModelKind, MeshGpu>,
    /// Per-vehicle GPU buffers, keyed by configured vehicle row.
    vehicles: HashMap<u32, VehicleGpu>,
    /// Vertical world Y-axis line (the up axis the ground grid can't draw).
    axis_gizmo: SceneTraj,
    texture_id: Option<egui::TextureId>,
}

impl SceneResources {
    fn new(ctx: RenderContext) -> Self {
        // Start at 1×1; the first `render_scene` resizes to the pane.
        let target = Scene3dTarget::new(ctx.clone(), 1, 1);
        let grid = Grid3dPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        let traj = Traj3dPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        let mesh = MeshPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );

        // Vertical Y (Up) axis, green — completes the §12.3 axes gizmo.
        let y_axis = vec![[0.0, 0.0, 0.0], [0.0, 12.0, 0.0]];
        let axis_gizmo = SceneTraj::new(&ctx, &traj, &y_axis, [0.25, 0.9, 0.3, 1.0]);

        Self {
            ctx,
            target,
            grid,
            traj,
            mesh,
            model_cache: HashMap::new(),
            vehicles: HashMap::new(),
            axis_gizmo,
            texture_id: None,
        }
    }

    /// Ensure a model's mesh is uploaded, returning it from the cache.
    fn model_mesh(&mut self, kind: &ModelKind) -> &MeshGpu {
        self.model_cache
            .entry(kind.clone())
            .or_insert_with(|| MeshGpu::upload(&self.ctx, &models::mesh_for(kind)))
    }

    /// Prepare GPU buffers + uniforms for the frame's vehicles (before the
    /// pass): upload each mesh once, (re)grow trajectory buffers, write uniforms.
    fn prepare_vehicles(
        &mut self,
        vp_cols: [[f32; 4]; 4],
        cam_pos: [f32; 3],
        vehicles: &[VehicleDraw],
    ) {
        // Light from upper front-right; ambient keeps shadowed faces readable.
        let light = glam::Vec3::new(0.4, 1.0, 0.6).normalize().to_array();
        for v in vehicles {
            // Upload the model mesh on first use (no-op afterwards).
            self.model_mesh(v.model);

            let needed = v.trajectory.len() as u32;
            let mut realloc = false;
            let entry = self.vehicles.entry(v.key);
            let vg = match entry {
                std::collections::hash_map::Entry::Occupied(o) => {
                    let vg = o.into_mut();
                    if needed > vg.traj_capacity {
                        // Grow geometrically (power-of-two) so appends are
                        // usually tail-only uploads; only a boundary reallocs.
                        let cap = needed.next_power_of_two();
                        vg.traj_points = new_points_buffer(&self.ctx, cap, "delog-veh-traj-points");
                        vg.traj_capacity = cap;
                        vg.traj_bind =
                            self.traj
                                .bind_group(&self.ctx, &vg.traj_points, &vg.traj_uniform);
                        realloc = true;
                    }
                    vg
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let cap = needed.max(1).next_power_of_two();
                    let mesh_uniform = new_uniform_buffer(
                        &self.ctx,
                        std::mem::size_of::<MeshUniform>() as u64,
                        "delog-veh-mesh-uniform",
                    );
                    let mesh_bind = self.mesh.bind_group(&self.ctx, &mesh_uniform);
                    let traj_points = new_points_buffer(&self.ctx, cap, "delog-veh-traj-points");
                    let traj_uniform = new_uniform_buffer(
                        &self.ctx,
                        std::mem::size_of::<Traj3dUniform>() as u64,
                        "delog-veh-traj-uniform",
                    );
                    let traj_bind = self.traj.bind_group(&self.ctx, &traj_points, &traj_uniform);
                    realloc = true;
                    slot.insert(VehicleGpu {
                        mesh_uniform,
                        mesh_bind,
                        traj_points,
                        traj_capacity: cap,
                        traj_count: 0,
                        traj_generation: v.traj_generation,
                        traj_uniform,
                        traj_bind,
                    })
                }
            };

            // Trajectory upload: full re-upload only when the buffer was just
            // (re)allocated or the config generation changed; otherwise the path
            // is append-only, so write just the new tail — and skip entirely
            // when unchanged. Avoids re-converting/re-uploading the whole path
            // every frame (the cost decimation used to hide).
            let full = realloc || vg.traj_generation != v.traj_generation || needed < vg.traj_count;
            if full && needed > 0 {
                let pts = points_to_vec4(v.trajectory);
                self.ctx
                    .queue()
                    .write_buffer(&vg.traj_points, 0, bytemuck::cast_slice(&pts));
            } else if !full && needed > vg.traj_count {
                let start = vg.traj_count as usize;
                let tail = points_to_vec4(&v.trajectory[start..]);
                let offset = start as u64 * std::mem::size_of::<[f32; 4]>() as u64;
                self.ctx
                    .queue()
                    .write_buffer(&vg.traj_points, offset, bytemuck::cast_slice(&tail));
            }
            vg.traj_count = needed;
            vg.traj_generation = v.traj_generation;
            self.ctx.queue().write_buffer(
                &vg.traj_uniform,
                0,
                bytemuck::bytes_of(&Traj3dUniform::new(vp_cols, v.path_color)),
            );
            self.ctx.queue().write_buffer(
                &vg.mesh_uniform,
                0,
                bytemuck::bytes_of(&MeshUniform::new(
                    vp_cols,
                    v.model_matrix,
                    v.normal_matrix,
                    light,
                    v.color,
                    cam_pos,
                    0.28,
                )),
            );
        }
    }

    /// Draw the frame's vehicles inside the scene pass: trajectory line then
    /// the posed mesh. Buffers must be prepared via [`Self::prepare_vehicles`].
    fn draw_vehicles(&self, pass: &mut wgpu::RenderPass<'_>, vehicles: &[VehicleDraw]) {
        for v in vehicles {
            let Some(vg) = self.vehicles.get(&v.key) else {
                continue;
            };
            self.traj.draw(pass, &vg.traj_bind, vg.traj_count);
            if let Some(mesh) = self.model_cache.get(v.model) {
                self.mesh.draw(pass, &vg.mesh_bind, mesh);
            }
        }
    }
}

fn new_points_buffer(ctx: &RenderContext, count: u32, label: &str) -> wgpu::Buffer {
    ctx.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (count as u64) * std::mem::size_of::<[f32; 4]>() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Pad render-space `[x,y,z]` points to the vec4 layout the line shader reads.
fn points_to_vec4(pts: &[[f32; 3]]) -> Vec<[f32; 4]> {
    pts.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect()
}

fn new_uniform_buffer(ctx: &RenderContext, size: u64, label: &str) -> wgpu::Buffer {
    ctx.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl SceneTraj {
    fn new(
        ctx: &RenderContext,
        pipeline: &Traj3dPipeline,
        pts: &[[f32; 3]],
        color: [f32; 4],
    ) -> Self {
        let data = points_to_vec4(pts);
        let points = new_points_buffer(ctx, data.len() as u32, "delog-scene-traj-points");
        ctx.queue()
            .write_buffer(&points, 0, bytemuck::cast_slice(&data));
        let uniform = new_uniform_buffer(
            ctx,
            std::mem::size_of::<Traj3dUniform>() as u64,
            "delog-scene-traj-uniform",
        );
        let bind = pipeline.bind_group(ctx, &points, &uniform);
        Self {
            uniform,
            bind,
            count: pts.len() as u32,
            color,
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
                scatter,
                step,
                minmax,
                buffers,
                col_buffers,
                uniforms,
                next_uniform_slot: _,
                line_binds,
                scatter_binds,
                step_binds,
                col_binds,
                errors,
                metrics: _,
            } = res;
            // Capture bind-group creation errors (GPU-12).
            let scope = GpuErrorHub::open(ctx.device());
            for item in &self.items {
                match item.kind {
                    DrawKind::Line { .. } => {
                        if let Some(buf) = buffers.buffer(item.field) {
                            line_binds.insert(item.field, line.bind_group(ctx, buf, uniforms));
                        }
                    }
                    DrawKind::Scatter { .. } => {
                        if let Some(buf) = buffers.buffer(item.field) {
                            scatter_binds
                                .insert(item.field, scatter.bind_group(ctx, buf, uniforms));
                        }
                    }
                    DrawKind::Step { .. } => {
                        if let Some(buf) = buffers.buffer(item.field) {
                            step_binds.insert(item.field, step.bind_group(ctx, buf, uniforms));
                        }
                    }
                    DrawKind::Columns { .. } => {
                        if let Some(buf) = col_buffers.buffer(item.field) {
                            col_binds.insert(item.field, minmax.bind_group(ctx, buf, uniforms));
                        }
                    }
                }
            }
            errors.get_mut().unwrap().close(scope);
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

        // CPU cost of recording this pane's draw commands (§16 `gpu_encode`,
        // PRF-01); the guard drops at the end of the callback.
        let _encode_timer = res.metrics.as_ref().map(|m| m.scope("gpu_encode"));

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

        // Batched encoding (GPU-11): one set_pipeline per same-pipeline run in
        // draw order; each trace then only rebinds its bind group with its
        // dynamic uniform offset.
        let runs = pipeline_runs(self.items.iter().map(|i| i.kind.pipeline()));
        let mut next = 0usize;
        for (kind, count) in runs {
            match kind {
                PipelineKind::Line => res.line.bind(render_pass),
                PipelineKind::Scatter => res.scatter.bind(render_pass),
                PipelineKind::Step => res.step.bind(render_pass),
                PipelineKind::Columns => res.minmax.bind(render_pass),
            }
            for item in &self.items[next..next + count as usize] {
                let offset = res.uniforms.dynamic_offset(item.slot);
                match item.kind {
                    DrawKind::Line { samples } => {
                        if let Some(bind) = res.line_binds.get(&item.field) {
                            res.line.draw_trace(render_pass, bind, offset, samples);
                        }
                    }
                    DrawKind::Scatter { samples } => {
                        if let Some(bind) = res.scatter_binds.get(&item.field) {
                            res.scatter.draw_trace(render_pass, bind, offset, samples);
                        }
                    }
                    DrawKind::Step { samples } => {
                        if let Some(bind) = res.step_binds.get(&item.field) {
                            res.step.draw_trace(render_pass, bind, offset, samples);
                        }
                    }
                    DrawKind::Columns { count } => {
                        if let Some(bind) = res.col_binds.get(&item.field) {
                            res.minmax.draw_trace(render_pass, bind, offset, count);
                        }
                    }
                }
            }
            next += count as usize;
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
    fn batching_groups_consecutive_items_into_one_bind_per_pipeline_run() {
        use PipelineKind::{Columns, Line, Scatter};
        let kinds = [
            DrawKind::Line { samples: 10 },
            DrawKind::Line { samples: 20 },
            DrawKind::Scatter { samples: 5 },
            DrawKind::Line { samples: 7 },
            DrawKind::Columns { count: 100 },
        ];
        let runs = pipeline_runs(kinds.iter().map(|k| k.pipeline()));
        // Draw order is preserved; each run = exactly one set_pipeline call.
        assert_eq!(runs, vec![(Line, 2), (Scatter, 1), (Line, 1), (Columns, 1)]);
        assert_eq!(pipeline_runs([].into_iter()), vec![]);
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

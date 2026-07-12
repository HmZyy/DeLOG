use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use delog_cache::{CacheManager, MinMax};
use delog_core::identity::FieldId;
use delog_core::metrics::MetricsRegistry;
use delog_render::{
    BufferManager, GpuErrorHub, Grid3dPipeline, GridUniform, LinePipeline, MAP_TILE_CAPACITY,
    MapTileDrawGroups, MapTilePipeline, MapTileUpload, MeshGpu, MeshPipeline, MeshUniform,
    MinMaxColPipeline, PlotUniform, RenderContext, ScatterPipeline, Scene3dTarget, StepPipeline,
    Traj3dPipeline, Traj3dUniform, UniformRing,
};
use eframe::{egui_wgpu, wgpu};

use crate::camera::OrbitCamera;
use crate::map::provider::{MapProviderId, TileId};
use crate::map::worker::{MapScopeId, ReadyTile};
use crate::models;
use crate::plot::{PlotPane, TraceMode, ViewX};
use crate::settings::Scene3dSettings;
use crate::vehicle::ModelKind;

#[derive(Clone, Debug)]
pub struct MapTileSelection {
    pub scope: MapScopeId,
    pub epoch: u64,
    pub provider: MapProviderId,
    pub generation: u64,
    /// Exactly the tiles visible in the current camera footprint, in priority order.
    pub current_tiles: Vec<(TileId, i32)>,
    /// Exact previous-zoom footprint intentionally retained as a visual fallback.
    pub previous_tiles: Vec<TileId>,
    pub enabled: bool,
}

pub struct VehicleDraw<'a> {
    pub key: u32,
    pub model: &'a ModelKind,
    pub model_matrix: [[f32; 4]; 4],
    pub normal_matrix: [[f32; 4]; 4],
    pub color: [f32; 4],
    pub path_color: [f32; 4],
    /// Render-space `[x,y,z]` trajectory points; NaN = gap. Full resident path.
    pub trajectory: &'a [[f32; 3]],
    /// Build-time config generation; a mismatch forces a full re-upload, a match
    /// lets a grown path upload only its appended tail.
    pub traj_generation: u64,
    /// Points to draw this frame (≤ trajectory len); the rest stays resident.
    pub visible_count: u32,
}

/// Plot rect + data window shared by the GPU and the egui axes so labels line
/// up with the rendered lines.
#[derive(Clone, Copy)]
pub struct PaneView {
    pub rect: egui::Rect,
    pub x_range: (f32, f32),
    pub y_range: (f32, f32),
}

#[derive(Clone, Copy, Debug)]
pub struct GpuBridge {
    available: bool,
    /// True when the render target gamma-encodes the shader's linear output
    /// (sRGB) vs raw write (UNORM); selects the trace-colour conversion.
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

    /// Drop map state belonging to 3D panes that no longer exist in the
    /// workspace. Call before rendering any scene panes for the frame.
    pub fn retain_map_scopes(&self, frame: &eframe::Frame, live: &[MapScopeId]) {
        if !self.available {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let mut renderer = render_state.renderer.write();
        if let Some(res) = renderer.callback_resources.get_mut::<SceneResources>() {
            res.retain_map_scopes(live);
        }
    }

    /// Whether the exact current selection has imagery resident for drawing.
    pub fn map_selection_has_current_imagery(
        &self,
        frame: &eframe::Frame,
        selection: &MapTileSelection,
    ) -> bool {
        if !self.available {
            return false;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return false;
        };
        let renderer = render_state.renderer.read();
        renderer
            .callback_resources
            .get::<SceneResources>()
            .is_some_and(|resources| resources.selection_transition_complete(selection))
    }

    /// Call once per frame.
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
            .saturating_add(res.win_buffers.field_mem(field).gpu)
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
            buffer_count: res.buffers.buffer_count()
                + res.col_buffers.buffer_count()
                + res.win_buffers.buffer_count(),
            gpu_bytes: res
                .buffers
                .total_gpu_bytes()
                .saturating_add(res.col_buffers.total_gpu_bytes())
                .saturating_add(res.win_buffers.total_gpu_bytes()),
        }
    }

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
            let scope = GpuErrorHub::open(res.ctx.device());
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
                        // Decimate when the window packs > decimate_threshold samples/px.
                        let (a, b) = cache.index_range(x0, x1);
                        let visible = b.saturating_sub(a) as f32;
                        if plot_w >= 1.0 && visible / plot_w > tuning.decimate_threshold {
                            let width = plot_w as usize;
                            // Skip the per-frame decimation + upload when the same
                            // view over unchanged data already produced the
                            // resident columns (static/paused views go ~free).
                            let key = ColKey {
                                x0: x0.to_bits(),
                                x1: x1.to_bits(),
                                width: width as u32,
                                bridge: tuning.bridge_columns,
                                len: cache.samples(),
                            };
                            if res.col_params.get(&trace.field) != Some(&key) {
                                let cols =
                                    cache.minmax_columns(x0, x1, width, tuning.bridge_columns);
                                let stat = res.col_buffers.sync(trace.field, &cols, true);
                                upload_bytes += stat.bytes;
                                full_uploads += stat.full_upload as u64;
                                res.col_params.insert(trace.field, key);
                            }
                            DrawKind::Columns {
                                count: width as u32,
                            }
                        } else {
                            let (aw, bw) = pad_window(a, b, cache.samples());
                            let key = WinKey {
                                a: aw,
                                b: bw,
                                len: cache.samples(),
                            };
                            if res.win_params.get(&trace.field) != Some(&key) {
                                let line_xy = line_window_xy(&cache.xy, aw, bw);
                                let stat = res.win_buffers.sync(trace.field, &line_xy, true);
                                if line_xy.is_empty() {
                                    res.win_buffers.remove(trace.field);
                                }
                                upload_bytes += stat.bytes;
                                full_uploads += stat.full_upload as u64;
                                res.win_params.insert(trace.field, key);
                            }
                            DrawKind::Line {
                                samples: res.win_buffers.samples(trace.field) as u32,
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

    /// The offscreen pass is submitted on our own queue during `update()`, so
    /// the texture is ready before eframe paints this frame.
    pub fn render_scene(
        &self,
        frame: &eframe::Frame,
        ui: &egui::Ui,
        rect: egui::Rect,
        camera: &OrbitCamera,
        scene3d: Scene3dSettings,
        map_selection: MapTileSelection,
        ready_tiles: &[ReadyTile],
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

        // Clone the resolved view to end the resource borrow, so the
        // texture-registration below can borrow the renderer mutably.
        let (view, resized, existing) = {
            let res = renderer.callback_resources.get_mut::<SceneResources>()?;
            let resized = res.target.width() != px_w || res.target.height() != px_h;
            res.target.resize(px_w, px_h);

            // f64 inverse: f32 is ill-conditioned far from the origin and crawls the grid.
            let (vp, inv) = camera
                .view_proj_and_inverse(px_w as f32 / px_h as f32, scene3d.resolved_far_clip_m());
            let vp_cols = vp.to_cols_array_2d();
            let (fade_start, fade_end) = scene3d.resolved_fog_m();
            // Cell tracks height above the y=0 ground so orbiting low doesn't shimmer
            // into a fine mesh; lod lets the shader cross-fade levels to avoid popping.
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
            res.ctx.queue().write_buffer(
                &res.axis_gizmo.uniform,
                0,
                bytemuck::bytes_of(&Traj3dUniform::new(vp_cols, res.axis_gizmo.color)),
            );
            res.prepare_vehicles(vp_cols, camera.eye().to_array(), vehicles);
            let visible_map_tiles = res.prepare_map_tiles(vp_cols, &map_selection, ready_tiles);

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
                res.map_tiles.draw_visible(&mut pass, &visible_map_tiles);
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

fn padded(min: f32, max: f32) -> (f32, f32) {
    if (max - min).abs() <= f32::EPSILON {
        return (min - 1.0, max + 1.0);
    }
    let pad = (max - min) * 0.05;
    (min - pad, max + pad)
}

/// sRGB target gamma-encodes the shader output, so emit linear; UNORM writes
/// raw, so emit sRGB as-is. Keeps the trace identical to its legend swatch.
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

#[derive(Clone, Copy)]
enum DrawKind {
    Line {
        samples: u32,
    },
    Scatter {
        samples: u32,
    },
    Step {
        samples: u32,
    },
    /// `count` per-pixel min/max columns.
    Columns {
        count: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipelineKind {
    Line,
    Scatter,
    Step,
    Columns,
}

/// Pad a visible index range `[a, b)` over `n` samples by one sample of context
/// each side so the line segments entering/leaving the viewport are drawn.
/// Clamps to `[0, n]`.
fn pad_window(a: usize, b: usize, n: usize) -> (usize, usize) {
    (a.saturating_sub(1), (b + 1).min(n))
}

fn line_window_xy(xy: &[f32], a: usize, b: usize) -> Vec<f32> {
    let samples = xy.len() / 2;
    let a = a.min(samples);
    let b = b.min(samples);
    if a >= b {
        return Vec::new();
    }

    let mut out = Vec::with_capacity((b - a) * 2);
    for p in xy[2 * a..2 * b].chunks_exact(2) {
        if p[0].is_finite() && p[1].is_finite() {
            out.extend_from_slice(p);
        }
    }
    out
}

/// Consecutive same-pipeline runs in draw order (one `set_pipeline` each).
/// Order-preserving so trace overlap (z-order) is unchanged.
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
    /// `[x,min,max]` column buffers (decimated path).
    col_buffers: BufferManager,
    /// Interleaved `[x,y]` buffers holding only the visible window per field
    /// (raw `Line` path); sized to what's on screen, not the full trace.
    win_buffers: BufferManager,
    uniforms: UniformRing,
    next_uniform_slot: u32,
    line_binds: HashMap<FieldId, wgpu::BindGroup>,
    scatter_binds: HashMap<FieldId, wgpu::BindGroup>,
    step_binds: HashMap<FieldId, wgpu::BindGroup>,
    col_binds: HashMap<FieldId, wgpu::BindGroup>,
    /// Memoizes the decimated columns resident in `col_buffers` per field, so a
    /// static view skips the per-frame `minmax_columns` recompute and upload.
    col_params: HashMap<FieldId, ColKey>,
    /// Memoizes the window resident in `win_buffers` per field, so a static view
    /// skips the per-frame slice upload (mirrors `col_params`).
    win_params: HashMap<FieldId, WinKey>,
    /// Mutex only satisfies the `Sync` bound; never contended (render thread only).
    errors: Mutex<GpuErrorHub>,
    metrics: Option<Arc<MetricsRegistry>>,
}

/// Identifies the decimated columns currently resident for a field. A match
/// means the same view over unchanged data, so the GPU buffer is already
/// correct and `minmax_columns` can be skipped entirely.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ColKey {
    x0: u32,
    x1: u32,
    width: u32,
    bridge: bool,
    len: usize,
}

/// Identifies the raw-line sample window currently resident for a field. A
/// match means the same visible window over unchanged data, so the windowed
/// GPU buffer is already correct and the slice upload can be skipped.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WinKey {
    a: usize,
    b: usize,
    len: usize,
}

impl PlotCallbackResources {
    fn new(ctx: RenderContext, color_format: wgpu::TextureFormat) -> Self {
        let line = LinePipeline::new(&ctx, color_format);
        let scatter = ScatterPipeline::new(&ctx, color_format);
        let step = StepPipeline::new(&ctx, color_format);
        let minmax = MinMaxColPipeline::new(&ctx, color_format);
        let buffers = BufferManager::new(ctx.clone());
        let col_buffers = BufferManager::new(ctx.clone());
        let win_buffers = BufferManager::new(ctx.clone());
        let uniforms = UniformRing::new(ctx.clone(), 8);
        Self {
            ctx,
            line,
            scatter,
            step,
            minmax,
            buffers,
            col_buffers,
            win_buffers,
            uniforms,
            next_uniform_slot: 0,
            line_binds: HashMap::new(),
            scatter_binds: HashMap::new(),
            step_binds: HashMap::new(),
            col_binds: HashMap::new(),
            col_params: HashMap::new(),
            win_params: HashMap::new(),
            errors: Mutex::new(GpuErrorHub::new()),
            metrics: None,
        }
    }

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
            .chain(self.win_buffers.fields())
            .filter(|f| !plotted.contains(f))
            .collect();
        for field in stale {
            self.buffers.remove(field);
            self.col_buffers.remove(field);
            self.col_params.remove(&field);
            self.win_buffers.remove(field);
            self.win_params.remove(&field);
        }
    }
}

struct SceneTraj {
    uniform: wgpu::Buffer,
    /// Holds the only reference to the points buffer, keeping it alive.
    bind: wgpu::BindGroup,
    count: u32,
    color: [f32; 4],
}

struct VehicleGpu {
    mesh_uniform: wgpu::Buffer,
    mesh_bind: wgpu::BindGroup,
    traj_points: wgpu::Buffer,
    traj_capacity: u32,
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
    map_tiles: MapTilePipeline,
    map_tile_cache: HashMap<MapScopeId, HashMap<u64, ReadyTile>>,
    map_tile_selections: HashMap<MapScopeId, MapTileSelection>,
    map_tile_resident_signatures: HashMap<u64, u64>,
    map_tile_epoch: u64,
    traj: Traj3dPipeline,
    mesh: MeshPipeline,
    /// Decoded meshes by model kind (lazy; built on first use).
    model_cache: HashMap<ModelKind, MeshGpu>,
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
        let map_tiles = MapTilePipeline::new(
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

        // Vertical Y (Up) axis, green — completes the axes gizmo.
        let y_axis = vec![[0.0, 0.0, 0.0], [0.0, 12.0, 0.0]];
        let axis_gizmo = SceneTraj::new(&ctx, &traj, &y_axis, [0.25, 0.9, 0.3, 1.0]);

        Self {
            ctx,
            target,
            grid,
            map_tiles,
            map_tile_cache: HashMap::new(),
            map_tile_selections: HashMap::new(),
            map_tile_resident_signatures: HashMap::new(),
            map_tile_epoch: 0,
            traj,
            mesh,
            model_cache: HashMap::new(),
            vehicles: HashMap::new(),
            axis_gizmo,
            texture_id: None,
        }
    }

    fn model_mesh(&mut self, kind: &ModelKind) -> &MeshGpu {
        self.model_cache
            .entry(kind.clone())
            .or_insert_with(|| MeshGpu::upload(&self.ctx, &models::mesh_for(kind)))
    }

    fn retain_map_scopes(&mut self, live: &[MapScopeId]) {
        let live: std::collections::HashSet<_> = live.iter().copied().collect();
        self.map_tile_cache.retain(|scope, _| live.contains(scope));
        self.map_tile_selections
            .retain(|scope, _| live.contains(scope));

        // Recompute the complete residency union immediately. This both drops
        // signatures for dead scopes and lets the surviving panes consume any
        // quota released by them before the first scene render of this frame.
        self.admit_map_tiles(&std::collections::HashSet::new(), None);
    }

    fn selection_transition_complete(&self, selection: &MapTileSelection) -> bool {
        if selection.previous_tiles.is_empty() {
            return false;
        }
        self.map_tile_cache
            .get(&selection.scope)
            .is_some_and(|cache| {
                let resident_ids: std::collections::HashSet<_> = cache
                    .iter()
                    .filter_map(|(key, tile)| self.map_tiles.contains(*key).then_some(tile.id))
                    .collect();
                selection
                    .current_tiles
                    .iter()
                    .all(|(id, _)| resident_ids.contains(id))
                    && selection
                        .previous_tiles
                        .iter()
                        .all(|id| !resident_ids.contains(id))
            })
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
            self.traj
                .draw(pass, &vg.traj_bind, v.visible_count.min(vg.traj_count));
            if let Some(mesh) = self.model_cache.get(v.model) {
                self.mesh.draw(pass, &vg.mesh_bind, mesh);
            }
        }
    }

    fn prepare_map_tiles(
        &mut self,
        vp: [[f32; 4]; 4],
        selection: &MapTileSelection,
        ready: &[ReadyTile],
    ) -> MapTileDrawGroups {
        self.map_tiles.set_view_proj(vp);
        if self.map_tile_epoch != selection.epoch {
            self.map_tile_epoch = selection.epoch;
            self.map_tile_cache.clear();
            self.map_tile_selections.clear();
            self.map_tile_resident_signatures.clear();
        }
        if !selection.enabled {
            self.map_tile_cache.remove(&selection.scope);
            self.map_tile_selections.remove(&selection.scope);
            self.admit_map_tiles(&std::collections::HashSet::new(), Some(selection.scope));
            return MapTileDrawGroups::default();
        }
        self.map_tile_selections
            .insert(selection.scope, selection.clone());
        let selection_changed = self
            .map_tile_cache
            .get(&selection.scope)
            .into_iter()
            .flat_map(HashMap::values)
            .any(|tile| {
                tile.provider != selection.provider || tile.generation != selection.generation
            });
        if selection_changed {
            self.map_tile_cache.remove(&selection.scope);
        }
        let changed = {
            let cache = self.map_tile_cache.entry(selection.scope).or_default();
            let mut changed = std::collections::HashSet::new();
            for tile in ready
                .iter()
                .filter(|tile| map_tile_matches(selection, tile))
            {
                let key = map_tile_key(tile);
                let signature = map_tile_signature(tile);
                if self.map_tile_resident_signatures.get(&key) != Some(&signature) {
                    changed.insert(key);
                }
                cache.insert(key, tile.clone());
            }
            cache.retain(|_, tile| map_tile_matches(selection, tile));
            changed
        };
        self.admit_map_tiles(&changed, Some(selection.scope));
        let cache = &self.map_tile_cache[&selection.scope];
        let mut visible = MapTileDrawGroups::default();
        for (key, tile) in cache {
            if !self.map_tiles.contains(*key) {
                continue;
            }
            if map_tile_is_current(selection, tile) {
                visible.current.push(*key);
            } else if map_tile_is_previous(selection, tile) {
                visible.previous.push(*key);
            }
        }
        visible.previous.sort_unstable();
        visible.current.sort_unstable();
        visible
    }

    fn admit_map_tiles(
        &mut self,
        changed: &std::collections::HashSet<u64>,
        active_scope: Option<MapScopeId>,
    ) {
        let mut scopes: Vec<_> = self.map_tile_selections.keys().copied().collect();
        scopes.sort_by_key(|scope| scope.0);
        // Above capacity, not every scope can own a slot. Keep the pane being
        // rendered first so it always retains one usable current tile; normal
        // <=128 deterministic quota allocation is unchanged.
        prioritize_active_scope(&mut scopes, active_scope);
        let scope_count = scopes.len().max(1);
        let base_quota = MAP_TILE_CAPACITY / scope_count;
        let remainder = MAP_TILE_CAPACITY % scope_count;
        let mut order = Vec::with_capacity(MAP_TILE_CAPACITY);
        for (index, scope) in scopes.iter().enumerate() {
            let quota = base_quota + usize::from(index < remainder);
            let selection = &self.map_tile_selections[scope];
            let empty = HashMap::new();
            let cache = self.map_tile_cache.get(scope).unwrap_or(&empty);
            order.extend(coverage_aware_scope_order(selection, cache, quota));
        }
        let admitted: std::collections::HashSet<_> = order.iter().copied().collect();

        self.map_tiles.retain(order.iter().copied());
        self.map_tile_resident_signatures
            .retain(|key, _| admitted.contains(key));
        for key in order {
            let tile = self
                .map_tile_cache
                .values()
                .find_map(|cache| cache.get(&key))
                .expect("admitted tile cached");
            if self.map_tiles.contains(key) && !changed.contains(&key) {
                continue;
            }
            if let Err(error) = self.map_tiles.upload(MapTileUpload {
                key,
                rgba: &tile.rgba,
                corners: tile.corners,
            }) {
                tracing::error!(%error, key, "capacity-aware map tile admission failed");
            } else {
                self.map_tile_resident_signatures
                    .insert(key, map_tile_signature(tile));
            }
        }
    }
}

fn prioritize_active_scope(scopes: &mut [MapScopeId], active_scope: Option<MapScopeId>) {
    if scopes.len() > MAP_TILE_CAPACITY
        && let Some(active) = active_scope
        && let Some(index) = scopes.iter().position(|scope| *scope == active)
    {
        scopes.swap(0, index);
    }
}

fn tiles_overlap_at_different_zooms(a: TileId, b: TileId) -> bool {
    let (coarse, fine) = if a.zoom <= b.zoom { (a, b) } else { (b, a) };
    let delta = fine.zoom - coarse.zoom;
    delta < 32
        && fine.x.checked_shr(delta as u32) == Some(coarse.x)
        && fine.y.checked_shr(delta as u32) == Some(coarse.y)
}

fn coverage_aware_scope_order(
    selection: &MapTileSelection,
    cache: &HashMap<u64, ReadyTile>,
    quota: usize,
) -> Vec<u64> {
    let ready_by_id: HashMap<_, _> = cache
        .iter()
        .filter_map(|(key, tile)| map_tile_matches(selection, tile).then_some((tile.id, *key)))
        .collect();
    let mut order = Vec::with_capacity(quota);
    let mut admitted = std::collections::HashSet::new();
    let mut replaced_previous = std::collections::HashSet::new();
    for id in &selection.previous_tiles {
        if order.len() == quota {
            break;
        }
        if let Some(key) = ready_by_id.get(id)
            && admitted.insert(*key)
        {
            order.push(*key);
        }
    }

    struct Group {
        current: Vec<(TileId, i32)>,
        previous: Vec<TileId>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for &(current_id, priority) in &selection.current_tiles {
        let overlaps: Vec<_> = selection
            .previous_tiles
            .iter()
            .copied()
            .filter(|previous_id| tiles_overlap_at_different_zooms(current_id, *previous_id))
            .collect();
        let matching: Vec<_> = groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                group
                    .previous
                    .iter()
                    .any(|id| overlaps.contains(id))
                    .then_some(index)
            })
            .collect();
        if let Some(&first) = matching.first() {
            groups[first].current.push((current_id, priority));
            for id in overlaps {
                if !groups[first].previous.contains(&id) {
                    groups[first].previous.push(id);
                }
            }
            for &index in matching.iter().skip(1).rev() {
                let merged = groups.remove(index);
                groups[first].current.extend(merged.current);
                for id in merged.previous {
                    if !groups[first].previous.contains(&id) {
                        groups[first].previous.push(id);
                    }
                }
            }
        } else {
            groups.push(Group {
                current: vec![(current_id, priority)],
                previous: overlaps,
            });
        }
    }
    groups.sort_by_key(|group| {
        let priority = group
            .current
            .iter()
            .map(|(_, priority)| *priority)
            .min()
            .unwrap_or(i32::MAX);
        let coarse = group
            .current
            .iter()
            .map(|(id, _)| *id)
            .chain(group.previous.iter().copied())
            .min_by_key(|id| (id.zoom, id.y, id.x))
            .unwrap();
        (priority, coarse.zoom, coarse.y, coarse.x)
    });
    for group in &mut groups {
        group
            .current
            .sort_by_key(|(id, priority)| (*priority, id.zoom, id.y, id.x));
        if group.current.is_empty()
            || !group
                .current
                .iter()
                .all(|(id, _)| ready_by_id.contains_key(id))
        {
            continue;
        }
        let previous_keys: std::collections::HashSet<_> = group
            .previous
            .iter()
            .filter_map(|id| ready_by_id.get(id).copied())
            .collect();
        let admitted_previous_count = order
            .iter()
            .filter(|key| previous_keys.contains(key))
            .count();
        let current_keys: Vec<_> = group
            .current
            .iter()
            .filter_map(|(id, _)| ready_by_id.get(id).copied())
            .collect();
        let candidate_len = order.len() - admitted_previous_count + current_keys.len();
        if candidate_len <= quota {
            order.retain(|key| !previous_keys.contains(key));
            admitted.retain(|key| !previous_keys.contains(key));
            replaced_previous.extend(previous_keys);
            for key in current_keys {
                if admitted.insert(key) {
                    order.push(key);
                }
            }
        }
    }

    let mut current = selection.current_tiles.clone();
    current.sort_by_key(|(id, priority)| (*priority, id.zoom, id.y, id.x));
    for (id, _) in current {
        if order.len() == quota {
            break;
        }
        if let Some(key) = ready_by_id.get(&id)
            && admitted.insert(*key)
        {
            order.push(*key);
        }
    }
    for id in &selection.previous_tiles {
        if order.len() == quota {
            break;
        }
        if let Some(key) = ready_by_id.get(id)
            && !replaced_previous.contains(key)
            && admitted.insert(*key)
        {
            order.push(*key);
        }
    }
    order
}

fn map_tile_matches(selection: &MapTileSelection, tile: &ReadyTile) -> bool {
    selection.enabled
        && tile.scope == selection.scope
        && tile.epoch == selection.epoch
        && tile.provider == selection.provider
        && tile.generation == selection.generation
        && (map_tile_is_current(selection, tile) || map_tile_is_previous(selection, tile))
}

fn map_tile_is_current(selection: &MapTileSelection, tile: &ReadyTile) -> bool {
    selection.current_tiles.iter().any(|(id, _)| *id == tile.id)
}

fn map_tile_is_previous(selection: &MapTileSelection, tile: &ReadyTile) -> bool {
    selection.previous_tiles.contains(&tile.id)
}

fn map_tile_key(tile: &ReadyTile) -> u64 {
    let mut hasher = DefaultHasher::new();
    tile.scope.hash(&mut hasher);
    tile.epoch.hash(&mut hasher);
    tile.provider.hash(&mut hasher);
    tile.generation.hash(&mut hasher);
    tile.id.hash(&mut hasher);
    hasher.finish()
}

fn map_tile_signature(tile: &ReadyTile) -> u64 {
    let mut hasher = DefaultHasher::new();
    tile.scope.hash(&mut hasher);
    tile.epoch.hash(&mut hasher);
    tile.provider.hash(&mut hasher);
    tile.generation.hash(&mut hasher);
    tile.id.zoom.hash(&mut hasher);
    tile.id.x.hash(&mut hasher);
    tile.id.y.hash(&mut hasher);
    tile.rgba.hash(&mut hasher);
    for corner in tile.corners {
        for coordinate in corner {
            coordinate.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
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
                win_buffers,
                uniforms,
                next_uniform_slot: _,
                line_binds,
                scatter_binds,
                step_binds,
                col_binds,
                col_params: _,
                win_params: _,
                errors,
                metrics: _,
            } = res;
            // Capture bind-group creation errors.
            let scope = GpuErrorHub::open(ctx.device());
            for item in &self.items {
                match item.kind {
                    DrawKind::Line { .. } => {
                        if let Some(buf) = win_buffers.buffer(item.field) {
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

        // CPU cost of recording this pane's draw commands (`gpu_encode`); the
        // guard drops at the end of the callback.
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

        render_pass.set_viewport(
            viewport.left_px.max(0) as f32,
            viewport.top_px.max(0) as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        render_pass.set_scissor_rect(sx, sy, sw, sh);

        // Batched encoding: one set_pipeline per same-pipeline run in
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

const MIN_ZOOM_DRAG_PX: f32 = 3.0;

/// X view for a right-drag zoom between two pixel x-positions within the plot
/// rect. None when the drag is too small to act on.
pub fn zoom_drag_view(
    view: ViewX,
    rect_left: f32,
    rect_width: f32,
    x_a: f32,
    x_b: f32,
) -> Option<ViewX> {
    if rect_width <= 0.0 || (x_a - x_b).abs() <= MIN_ZOOM_DRAG_PX {
        return None;
    }
    let span = view.span_us() as f64;
    let time_at = |x: f32| {
        let frac = ((x - rect_left) / rect_width).clamp(0.0, 1.0) as f64;
        view.min_us + (frac * span) as i64
    };
    let (a, b) = (time_at(x_a), time_at(x_b));
    Some(ViewX::new(a.min(b), a.max(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_scope_is_first_only_when_scope_count_exceeds_capacity() {
        let mut saturated: Vec<_> = (0..=MAP_TILE_CAPACITY as u64).map(MapScopeId).collect();
        prioritize_active_scope(&mut saturated, Some(MapScopeId(128)));
        assert_eq!(saturated[0], MapScopeId(128));

        let mut normal = vec![MapScopeId(1), MapScopeId(2)];
        prioritize_active_scope(&mut normal, Some(MapScopeId(2)));
        assert_eq!(normal, vec![MapScopeId(1), MapScopeId(2)]);
    }

    fn live_zoom(zoom: u8) -> Vec<(TileId, i32)> {
        (0..4)
            .flat_map(|y| (0..256).map(move |x| (TileId { zoom, x, y }, x as i32)))
            .collect()
    }

    fn live_zoom_ids(zoom: u8) -> Vec<TileId> {
        live_zoom(zoom).into_iter().map(|(id, _)| id).collect()
    }

    #[test]
    fn pad_window_adds_one_sample_of_context_each_side() {
        assert_eq!(pad_window(10, 20, 100), (9, 21));
    }

    #[test]
    fn pad_window_clamps_at_buffer_ends() {
        assert_eq!(pad_window(0, 100, 100), (0, 100)); // both ends clamp
        assert_eq!(pad_window(0, 5, 100), (0, 6)); // low clamps, high pads
        assert_eq!(pad_window(95, 100, 100), (94, 100)); // low pads, high clamps
    }

    #[test]
    fn pad_window_handles_empty() {
        assert_eq!(pad_window(0, 0, 0), (0, 0));
    }

    #[test]
    fn line_window_upload_skips_nan_points_so_finite_samples_connect() {
        let xy = [
            0.0,
            10.0, //
            1.0,
            f32::NAN, //
            2.0,
            12.0, //
            3.0,
            f32::INFINITY, //
            4.0,
            14.0,
        ];

        let line_xy = line_window_xy(&xy, 0, 5);

        assert_eq!(line_xy, vec![0.0, 10.0, 2.0, 12.0, 4.0, 14.0]);
    }

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
        apply_zoom(&mut view, 0.5, 200.0);
        assert!(view.span_us() < 1000);
        // Centre stays roughly fixed.
        let centre = (view.min_us + view.max_us) / 2;
        assert!((centre - 500).abs() < 50);
    }

    #[test]
    fn zoom_drag_left_to_right_selects_window() {
        let view = ViewX::new(0, 1000);
        // rect: left=100, width=100. Drag from 25% to 75% of the rect.
        let out = zoom_drag_view(view, 100.0, 100.0, 125.0, 175.0).unwrap();
        assert_eq!(out.min_us, 250);
        assert_eq!(out.max_us, 750);
    }

    #[test]
    fn zoom_drag_is_symmetric() {
        let view = ViewX::new(0, 1000);
        let fwd = zoom_drag_view(view, 100.0, 100.0, 125.0, 175.0).unwrap();
        let rev = zoom_drag_view(view, 100.0, 100.0, 175.0, 125.0).unwrap();
        assert_eq!(fwd.min_us, rev.min_us);
        assert_eq!(fwd.max_us, rev.max_us);
    }

    #[test]
    fn zoom_drag_below_threshold_is_noop() {
        let view = ViewX::new(0, 1000);
        assert!(zoom_drag_view(view, 100.0, 100.0, 150.0, 152.0).is_none());
    }

    #[test]
    fn zoom_drag_clamps_past_rect_edges() {
        let view = ViewX::new(0, 1000);
        // x well outside the rect on both sides clamps to full 0..1000.
        let out = zoom_drag_view(view, 100.0, 100.0, -50.0, 500.0).unwrap();
        assert_eq!(out.min_us, 0);
        assert_eq!(out.max_us, 1000);
    }

    #[test]
    fn zoom_drag_zero_width_rect_is_noop() {
        let view = ViewX::new(0, 1000);
        assert!(zoom_drag_view(view, 0.0, 0.0, 5.0, 50.0).is_none());
    }

    #[test]
    fn mixed_ready_batch_keeps_requested_current_and_previous_zoom_independent_of_order() {
        let selection = MapTileSelection {
            scope: MapScopeId(4),
            epoch: 3,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 9,
            current_tiles: live_zoom(12),
            previous_tiles: live_zoom_ids(11),
            enabled: true,
        };
        let tile = |zoom| ReadyTile {
            scope: MapScopeId(4),
            epoch: 3,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            id: crate::map::provider::TileId { zoom, x: 0, y: 0 },
            generation: 9,
            priority: 0,
            rgba: Vec::new(),
            corners: [[0.0; 3]; 4],
        };
        let mixed = [tile(11), tile(12)];
        assert!(mixed.iter().all(|tile| map_tile_matches(&selection, tile)));
        assert!(
            mixed
                .iter()
                .rev()
                .all(|tile| map_tile_matches(&selection, tile))
        );
        assert!(!map_tile_matches(&selection, &tile(10)));
    }

    #[test]
    fn prepare_map_tiles_exposes_sorted_previous_then_current_draw_groups() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping map zoom grouping test");
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = MapTileSelection {
            scope: MapScopeId(44),
            epoch: 2,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 7,
            current_tiles: live_zoom(8),
            previous_tiles: live_zoom_ids(7),
            enabled: true,
        };
        let tile = |zoom, x| ReadyTile {
            scope: selection.scope,
            epoch: selection.epoch,
            provider: selection.provider,
            id: crate::map::provider::TileId { zoom, x, y: 3 },
            generation: selection.generation,
            priority: x as i32,
            rgba: [x as u8, zoom, 0, 255].repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let tiles = [tile(8, 5), tile(7, 2), tile(8, 1), tile(7, 6)];
        let mut expected_previous = vec![map_tile_key(&tiles[1]), map_tile_key(&tiles[3])];
        let mut expected_current = vec![map_tile_key(&tiles[0]), map_tile_key(&tiles[2])];
        expected_previous.sort_unstable();
        expected_current.sort_unstable();
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();

        let first = resources.prepare_map_tiles(identity, &selection, &tiles);
        let reversed = resources.prepare_map_tiles(
            identity,
            &selection,
            &tiles.iter().rev().cloned().collect::<Vec<_>>(),
        );
        assert_eq!(first.previous, expected_previous);
        assert_eq!(first.current, expected_current);
        assert_eq!(reversed, first, "ready insertion order cannot affect draws");
        assert!(!resources.selection_transition_complete(&selection));

        let mut other_pane = selection.clone();
        other_pane.scope = MapScopeId(45);
        assert!(!resources.selection_transition_complete(&other_pane));

        let mut switched_generation = selection.clone();
        switched_generation.generation += 1;
        assert!(!resources.selection_transition_complete(&switched_generation));
    }

    #[test]
    fn cache_epoch_change_purges_cpu_and_gpu_tiles_on_empty_poll() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping map clear residency test");
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = MapTileSelection {
            scope: MapScopeId(7),
            epoch: 0,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: live_zoom(3),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = ReadyTile {
            scope: selection.scope,
            epoch: 0,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            id: crate::map::provider::TileId {
                zoom: 3,
                x: 1,
                y: 2,
            },
            generation: 1,
            priority: 0,
            rgba: [40, 80, 120, 255].repeat(256 * 256),
            corners: [[0.0, 0.0, 0.0]; 4],
        };
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection, &[tile]);
        assert_eq!(resources.map_tile_cache[&selection.scope].len(), 1);
        assert_eq!(resources.map_tiles.resident_tile_count(), 1);

        resources.prepare_map_tiles(
            identity,
            &MapTileSelection {
                epoch: 1,
                ..selection
            },
            &[],
        );
        assert_eq!(resources.map_tile_cache[&selection.scope].len(), 0);
        assert_eq!(resources.map_tiles.resident_tile_count(), 0);
    }

    #[test]
    fn map_tile_prepare_only_uploads_and_allocates_changed_residency() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping map residency instrumentation test");
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = MapTileSelection {
            scope: MapScopeId(8),
            epoch: 2,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 5,
            current_tiles: live_zoom(4),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |zoom, x, color: [u8; 4]| ReadyTile {
            scope: selection.scope,
            epoch: selection.epoch,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            id: crate::map::provider::TileId { zoom, x, y: 1 },
            generation: selection.generation,
            priority: x as i32,
            rgba: color.repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection, &[tile(4, 1, [1, 2, 3, 255])]);
        assert_eq!(resources.map_tiles.upload_count(), 1);
        assert_eq!(resources.map_tiles.allocation_count(), 1);

        resources.prepare_map_tiles(identity, &selection, &[]);
        assert_eq!(
            resources.map_tiles.upload_count(),
            1,
            "static frame uploads zero"
        );
        assert_eq!(
            resources.map_tiles.allocation_count(),
            1,
            "static frame allocates zero"
        );

        let zoomed = MapTileSelection {
            current_tiles: live_zoom(5),
            previous_tiles: live_zoom_ids(4),
            ..selection
        };
        resources.prepare_map_tiles(identity, &zoomed, &[tile(5, 2, [4, 5, 6, 255])]);
        assert_eq!(resources.map_tiles.resident_tile_count(), 2);
        assert_eq!(
            resources.map_tiles.upload_count(),
            2,
            "only the new zoom uploads"
        );
        assert_eq!(
            resources.map_tiles.allocation_count(),
            2,
            "only the new zoom allocates"
        );

        resources.prepare_map_tiles(identity, &zoomed, &[tile(5, 2, [7, 8, 9, 255])]);
        assert_eq!(
            resources.map_tiles.upload_count(),
            3,
            "changed content uploads"
        );
    }

    #[test]
    fn alternating_map_scopes_keep_union_resident_without_cross_pane_draws() {
        let Some(ctx) = RenderContext::headless() else {
            eprintln!("no wgpu adapter — skipping multi-pane map residency test");
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = |scope| MapTileSelection {
            scope: MapScopeId(scope),
            epoch: 4,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: live_zoom(6),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |scope, x, color: [u8; 4]| ReadyTile {
            scope: MapScopeId(scope),
            epoch: 4,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            id: crate::map::provider::TileId { zoom: 6, x, y: 2 },
            generation: 1,
            priority: x as i32,
            rgba: color.repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        let tile_a = tile(10, 1, [10, 20, 30, 255]);
        let tile_b = tile(20, 2, [40, 50, 60, 255]);
        let key_a = map_tile_key(&tile_a);
        let key_b = map_tile_key(&tile_b);

        let draw_a = resources.prepare_map_tiles(identity, &selection(10), &[tile_a]);
        assert_eq!(draw_a.current, vec![key_a], "pane A draws only A");
        assert!(draw_a.previous.is_empty());
        let draw_b = resources.prepare_map_tiles(identity, &selection(20), &[tile_b]);
        assert_eq!(draw_b.current, vec![key_b], "pane B draws only B");
        assert!(draw_b.previous.is_empty());
        let draw_a_again = resources.prepare_map_tiles(identity, &selection(10), &[]);
        assert_eq!(
            draw_a_again.current,
            vec![key_a],
            "pane A still draws only A"
        );
        assert!(draw_a_again.previous.is_empty());
        assert_eq!(resources.map_tiles.resident_tile_count(), 2);
        assert_eq!(
            resources.map_tiles.upload_count(),
            2,
            "each scope uploads once"
        );

        let draw_disabled_b = resources.prepare_map_tiles(
            identity,
            &MapTileSelection {
                current_tiles: Vec::new(),
                previous_tiles: Vec::new(),
                enabled: false,
                ..selection(20)
            },
            &[],
        );
        assert!(draw_disabled_b.is_empty());
        assert!(!resources.map_tile_cache.contains_key(&MapScopeId(20)));
        assert_eq!(resources.map_tiles.resident_tile_count(), 1);
        assert!(resources.map_tiles.contains(key_a), "disabling B retains A");

        let draw_a_after_b_disabled = resources.prepare_map_tiles(identity, &selection(10), &[]);
        assert_eq!(draw_a_after_b_disabled.current, vec![key_a]);
        assert!(draw_a_after_b_disabled.previous.is_empty());
        assert_eq!(resources.map_tiles.upload_count(), 2, "A is not reuploaded");

        let draw_after_epoch = resources.prepare_map_tiles(
            identity,
            &MapTileSelection {
                epoch: 5,
                ..selection(10)
            },
            &[],
        );
        assert!(draw_after_epoch.is_empty());
        assert!(resources.map_tile_cache.values().all(HashMap::is_empty));
        assert_eq!(resources.map_tiles.resident_tile_count(), 0);
    }

    #[test]
    fn xyz_overlap_handles_zoom_in_out_and_jumps() {
        let id = |zoom, x, y| TileId { zoom, x, y };
        assert!(tiles_overlap_at_different_zooms(
            id(8, 3, 5),
            id(10, 13, 21)
        ));
        assert!(tiles_overlap_at_different_zooms(
            id(10, 13, 21),
            id(8, 3, 5)
        ));
        assert!(!tiles_overlap_at_different_zooms(
            id(8, 3, 5),
            id(10, 16, 21)
        ));
    }

    fn transition_selection(
        current: Vec<(TileId, i32)>,
        previous: Vec<TileId>,
    ) -> MapTileSelection {
        MapTileSelection {
            scope: MapScopeId(31),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: current,
            previous_tiles: previous,
            enabled: true,
        }
    }

    fn transition_tile(selection: &MapTileSelection, id: TileId, priority: i32) -> ReadyTile {
        ReadyTile {
            scope: selection.scope,
            epoch: selection.epoch,
            provider: selection.provider,
            id,
            generation: selection.generation,
            priority,
            rgba: [id.x as u8, id.zoom, id.y as u8, 255].repeat(256 * 256),
            corners: [[id.x as f32, id.y as f32, 0.0]; 4],
        }
    }

    #[test]
    fn partial_children_keep_saturated_fallback_coverage() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let parent_ids: Vec<_> = (0..128).map(|x| TileId { zoom: 7, x, y: 5 }).collect();
        let child_ids = [
            TileId {
                zoom: 8,
                x: 6,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 6,
                y: 11,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 11,
            },
        ];
        let selection = transition_selection(
            child_ids
                .iter()
                .enumerate()
                .map(|(p, id)| (*id, p as i32))
                .collect(),
            parent_ids.clone(),
        );
        let previous: Vec<_> = parent_ids
            .iter()
            .copied()
            .map(|id| transition_tile(&selection, id, id.x as i32))
            .collect();
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        let first = resources.prepare_map_tiles(identity, &selection, &previous);
        assert_eq!(first.previous.len(), 128);

        let child = transition_tile(&selection, child_ids[0], 0);
        let child_key = map_tile_key(&child);
        let draw = resources.prepare_map_tiles(identity, &selection, &[child]);
        assert!(!draw.current.contains(&child_key));
        assert!(!resources.map_tiles.contains(child_key));
        assert_eq!(draw.previous.len(), 128);
        assert_eq!(resources.map_tiles.resident_tile_count(), 128);
    }

    #[test]
    fn partial_child_uses_spare_slot_over_retained_parent() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let parent_ids: Vec<_> = (0..127).map(|x| TileId { zoom: 7, x, y: 5 }).collect();
        let child_ids = [
            TileId {
                zoom: 8,
                x: 6,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 6,
                y: 11,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 11,
            },
        ];
        let selection = transition_selection(
            child_ids
                .iter()
                .enumerate()
                .map(|(p, id)| (*id, p as i32))
                .collect(),
            parent_ids.clone(),
        );
        let previous: Vec<_> = parent_ids
            .iter()
            .copied()
            .map(|id| transition_tile(&selection, id, id.x as i32))
            .collect();
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection, &previous);
        let child = transition_tile(&selection, child_ids[0], 0);
        let draw = resources.prepare_map_tiles(identity, &selection, &[child.clone()]);
        assert_eq!(draw.previous.len(), 127);
        assert!(draw.current.contains(&map_tile_key(&child)));
        assert!(resources.map_tiles.contains(map_tile_key(&transition_tile(
            &selection,
            parent_ids[3],
            3
        ))));
    }

    #[test]
    fn complete_children_replace_parent_atomically_when_quota_fits() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let parent_ids: Vec<_> = (0..125).map(|x| TileId { zoom: 7, x, y: 5 }).collect();
        let children = [
            TileId {
                zoom: 8,
                x: 6,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 6,
                y: 11,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 11,
            },
        ];
        let selection = transition_selection(
            children
                .iter()
                .enumerate()
                .map(|(priority, id)| (*id, priority as i32))
                .collect(),
            parent_ids.clone(),
        );
        let previous: Vec<_> = parent_ids
            .iter()
            .copied()
            .map(|id| transition_tile(&selection, id, id.x as i32))
            .collect();
        let ready_children: Vec<_> = children
            .iter()
            .enumerate()
            .map(|(priority, id)| transition_tile(&selection, *id, priority as i32))
            .collect();
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection, &previous);
        let draw = resources.prepare_map_tiles(identity, &selection, &ready_children);
        assert_eq!(draw.previous.len(), 124);
        assert_eq!(draw.current.len(), 4);
        assert!(!resources.map_tiles.contains(map_tile_key(&transition_tile(
            &selection,
            parent_ids[3],
            3
        ))));
        let reversed = resources.prepare_map_tiles(
            identity,
            &selection,
            &ready_children.iter().rev().cloned().collect::<Vec<_>>(),
        );
        assert_eq!(
            reversed, draw,
            "ready arrival order cannot affect atomic replacement"
        );
    }

    #[test]
    fn zoom_out_parent_replaces_fallback_children_atomically() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let parent = TileId {
            zoom: 7,
            x: 3,
            y: 5,
        };
        let children = vec![
            TileId {
                zoom: 8,
                x: 6,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 10,
            },
            TileId {
                zoom: 8,
                x: 6,
                y: 11,
            },
            TileId {
                zoom: 8,
                x: 7,
                y: 11,
            },
        ];
        let selection = transition_selection(vec![(parent, 0)], children.clone());
        let fallback: Vec<_> = children
            .iter()
            .copied()
            .map(|id| transition_tile(&selection, id, 0))
            .collect();
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection, &fallback);
        let current = transition_tile(&selection, parent, 0);
        let draw = resources.prepare_map_tiles(identity, &selection, &[current.clone()]);
        assert_eq!(draw.current, vec![map_tile_key(&current)]);
        assert!(draw.previous.is_empty());
        assert!(resources.selection_transition_complete(&selection));
    }

    #[test]
    fn saturated_scope_draws_only_deterministic_first_128_candidates() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = MapTileSelection {
            scope: MapScopeId(32),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: live_zoom(8),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |x| ReadyTile {
            scope: selection.scope,
            epoch: 1,
            provider: selection.provider,
            id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
            generation: 1,
            priority: x as i32,
            rgba: [x as u8, 8, 0, 255].repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let tiles: Vec<_> = (0..140).rev().map(tile).collect();
        let expected: std::collections::HashSet<_> =
            (0..128).map(|x| map_tile_key(&tile(x))).collect();
        let draw = resources.prepare_map_tiles(
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            &selection,
            &tiles,
        );
        assert_eq!(draw.current.len(), 128);
        assert_eq!(
            draw.current
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        assert!(
            draw.current
                .iter()
                .all(|key| resources.map_tiles.contains(*key))
        );
        assert_eq!(
            resources.map_tile_cache[&selection.scope].len(),
            140,
            "CPU cache retains overflow"
        );
    }

    #[test]
    fn saturated_same_zoom_pan_replaces_disjoint_old_tiles_and_bounds_cpu_cache() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let mut selection = MapTileSelection {
            scope: MapScopeId(33),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: live_zoom(8),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |x| ReadyTile {
            scope: selection.scope,
            epoch: selection.epoch,
            provider: selection.provider,
            id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
            generation: selection.generation,
            priority: (x % 128) as i32,
            rgba: [x as u8, 8, 0, 255].repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let old: Vec<_> = (0..128).map(tile).collect();
        let new: Vec<_> = (128..256).map(tile).collect();
        let expected: std::collections::HashSet<_> = new.iter().map(map_tile_key).collect();
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        selection.current_tiles = old.iter().map(|tile| (tile.id, tile.priority)).collect();
        resources.prepare_map_tiles(identity, &selection, &old);
        selection.current_tiles = new.iter().map(|tile| (tile.id, tile.priority)).collect();
        let draw = resources.prepare_map_tiles(identity, &selection, &new);

        assert_eq!(
            draw.current
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        assert_eq!(resources.map_tile_cache[&selection.scope].len(), 128);
        assert_eq!(resources.map_tiles.resident_tile_count(), 128);
    }

    #[test]
    fn same_zoom_one_tile_pan_retains_old_nonoverlap_with_exact_current_group() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let id = |x| crate::map::provider::TileId { zoom: 8, x, y: 2 };
        let mut selection = MapTileSelection {
            scope: MapScopeId(34),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: vec![(id(1), 0), (id(2), 1)],
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |x, priority| ReadyTile {
            scope: selection.scope,
            epoch: selection.epoch,
            provider: selection.provider,
            id: id(x),
            generation: selection.generation,
            priority,
            rgba: [x as u8, 8, 0, 255].repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let old = vec![tile(1, 0), tile(2, 1)];
        let old_key = map_tile_key(&old[0]);
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection, &old);

        selection.current_tiles = vec![(id(2), 0), (id(3), 1)];
        selection.previous_tiles = vec![id(1)];
        let new = tile(3, 1);
        let expected_current = [map_tile_key(&old[1]), map_tile_key(&new)]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let draw = resources.prepare_map_tiles(identity, &selection, &[new]);

        assert_eq!(draw.previous, vec![old_key]);
        assert_eq!(
            draw.current
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            expected_current
        );
        assert!(resources.map_tiles.contains(old_key));
    }

    #[test]
    fn three_saturated_scopes_have_stable_sorted_quotas_and_uploads() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = |scope| MapTileSelection {
            scope: MapScopeId(scope),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: (0..128)
                .map(|x| (crate::map::provider::TileId { zoom: 8, x, y: 0 }, x as i32))
                .collect(),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tiles = |scope| {
            (0..128)
                .map(|x| ReadyTile {
                    scope: MapScopeId(scope),
                    epoch: 1,
                    provider: crate::map::provider::MapProviderId::BingSatellite,
                    id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
                    generation: 1,
                    priority: x as i32,
                    rgba: [scope as u8, x as u8, 0, 255].repeat(256 * 256),
                    corners: [[x as f32, 0.0, 0.0]; 4],
                })
                .collect::<Vec<_>>()
        };
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        for scope in [30, 10, 20] {
            resources.prepare_map_tiles(identity, &selection(scope), &tiles(scope));
        }
        let warm = resources.map_tiles.upload_count();
        for _ in 0..3 {
            for scope in [30, 10, 20] {
                resources.prepare_map_tiles(identity, &selection(scope), &[]);
            }
        }
        let resident = |scope| {
            resources.map_tile_cache[&MapScopeId(scope)]
                .keys()
                .filter(|key| resources.map_tiles.contains(**key))
                .count()
        };
        assert_eq!((resident(10), resident(20), resident(30)), (43, 43, 42));
        assert_eq!(resources.map_tiles.upload_count(), warm);
    }

    #[test]
    fn saturated_other_scope_cannot_starve_active_scope() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = |scope| MapTileSelection {
            scope: MapScopeId(scope),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: live_zoom(8),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |scope, x| ReadyTile {
            scope: MapScopeId(scope),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
            generation: 1,
            priority: x as i32,
            rgba: [scope as u8, x as u8, 0, 255].repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let a: Vec<_> = (0..128).map(|x| tile(41, x)).collect();
        let b = tile(42, 0);
        let b_key = map_tile_key(&b);
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        resources.prepare_map_tiles(identity, &selection(41), &a);
        let draw_b = resources.prepare_map_tiles(identity, &selection(42), &[b]);
        assert_eq!(draw_b.current, vec![b_key]);
        assert!(resources.map_tiles.contains(b_key));
        assert!(
            draw_b
                .current
                .iter()
                .all(|key| resources.map_tiles.contains(*key))
        );
        let draw_a = resources.prepare_map_tiles(identity, &selection(41), &[]);
        assert_eq!(draw_a.current.len(), 64);
        assert!(
            resources.map_tiles.contains(b_key),
            "alternating panes stabilizes without evicting B"
        );
        let draw_b_again = resources.prepare_map_tiles(identity, &selection(42), &[]);
        assert_eq!(draw_b_again.current, vec![b_key]);
        assert_eq!(resources.map_tiles.resident_tile_count(), 65);
    }

    #[test]
    fn retaining_live_map_scopes_reclaims_closed_scope_quota_and_cache() {
        let Some(ctx) = RenderContext::headless() else {
            return;
        };
        let mut resources = SceneResources::new(ctx);
        let selection = |scope| MapTileSelection {
            scope: MapScopeId(scope),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            generation: 1,
            current_tiles: live_zoom(8),
            previous_tiles: Vec::new(),
            enabled: true,
        };
        let tile = |scope, x| ReadyTile {
            scope: MapScopeId(scope),
            epoch: 1,
            provider: crate::map::provider::MapProviderId::BingSatellite,
            id: crate::map::provider::TileId { zoom: 8, x, y: 0 },
            generation: 1,
            priority: x as i32,
            rgba: [scope as u8, x as u8, 0, 255].repeat(256 * 256),
            corners: [[x as f32, 0.0, 0.0]; 4],
        };
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        let a: Vec<_> = (0..128).map(|x| tile(51, x)).collect();
        let b: Vec<_> = (0..128).map(|x| tile(52, x)).collect();
        resources.prepare_map_tiles(identity, &selection(51), &a);
        resources.prepare_map_tiles(identity, &selection(52), &b);

        resources.retain_map_scopes(&[MapScopeId(52)]);

        assert!(!resources.map_tile_cache.contains_key(&MapScopeId(51)));
        assert!(!resources.map_tile_selections.contains_key(&MapScopeId(51)));
        assert_eq!(resources.map_tiles.resident_tile_count(), 128);
        let draw_b = resources.prepare_map_tiles(identity, &selection(52), &[]);
        assert_eq!(draw_b.current.len(), 128, "B gets the closed pane's quota");

        resources.retain_map_scopes(&[]);
        assert!(resources.map_tile_cache.is_empty());
        assert!(resources.map_tile_selections.is_empty());
        assert!(resources.map_tile_resident_signatures.is_empty());
        assert_eq!(resources.map_tiles.resident_tile_count(), 0);

        for scope in 60..64 {
            let tiles: Vec<_> = (0..128).map(|x| tile(scope, x)).collect();
            resources.prepare_map_tiles(identity, &selection(scope), &tiles);
            resources.retain_map_scopes(&[]);
            assert!(resources.map_tile_cache.is_empty());
            assert!(resources.map_tile_selections.is_empty());
            assert!(resources.map_tile_resident_signatures.is_empty());
            assert_eq!(resources.map_tiles.resident_tile_count(), 0);
        }

        let disabled_scope = MapScopeId(70);
        resources.prepare_map_tiles(
            identity,
            &MapTileSelection {
                current_tiles: Vec::new(),
                previous_tiles: Vec::new(),
                enabled: false,
                ..selection(disabled_scope.0)
            },
            &[],
        );
        resources.retain_map_scopes(&[disabled_scope]);
        assert!(resources.map_tile_cache.is_empty());
        assert!(resources.map_tile_selections.is_empty());
        assert_eq!(resources.map_tiles.resident_tile_count(), 0);
    }

    #[test]
    fn scene_pass_encodes_tiles_before_grid_before_vehicle_overlays() {
        let source = include_str!("gpu.rs");
        let pass = source
            .split("let mut pass = res.target.begin_pass")
            .nth(1)
            .expect("scene pass");
        let tiles = pass.find("res.map_tiles.draw").expect("tile draw");
        let grid = pass.find("res.grid.draw").expect("grid draw");
        let vehicles = pass.find("res.draw_vehicles").expect("vehicle draw");
        assert!(tiles < grid && grid < vehicles);
    }
}

//! DeLOG renderer: wgpu pipelines, the GPU buffer manager, the 2D plot
//! pass and the 3D scene.
//!
//! Dependency rule (PLAN.md §3.2): this crate is pure wgpu — **no egui
//! types** — so headless benchmarks and golden-image tests can drive it
//! without a window. `delog-app` adapts it through `egui_wgpu` callbacks.
//!
//! ## Embedded assets (PLAN.md §3.1, checklist ARC-06)
//!
//! Everything the renderer ships lives under the workspace `assets/` directory
//! and is embedded at compile time — there is no runtime asset directory to
//! locate, so a stripped single binary always renders:
//!
//! - **Palette** — [`palette`], `include!`d from `assets/palette.rs`.
//! - **Shaders** — WGSL sources are pulled in with `include_str!` at each
//!   pipeline's construction site (the `line_pull` shader arrives with GPU-05:
//!   `include_str!(".../assets/shaders/line_pull.wgsl")`).
//! - **Models** — vehicle GLBs are pulled in with `include_bytes!` by the model
//!   registry (quad / fixed-wing / delta / marker arrive with TDV-08), with a
//!   procedural cone as the unconditional fallback (PLAN.md §10.3).

pub mod buffers;
pub mod context;
pub mod errors;
pub mod line;
pub mod minmax;
pub mod palette;
pub mod scatter;
pub mod step;
pub mod target;
pub mod uniforms;

pub use buffers::BufferManager;
pub use context::RenderContext;
pub use errors::GpuErrorHub;
pub use line::LinePipeline;
pub use minmax::{COLUMN_STRIDE, MinMaxColPipeline};
pub use scatter::ScatterPipeline;
pub use step::StepPipeline;
pub use target::{OffscreenTarget, RgbaImage};
pub use uniforms::{PlotUniform, UniformRing};
